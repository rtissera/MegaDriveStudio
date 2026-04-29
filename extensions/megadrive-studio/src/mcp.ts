// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import * as fs from "fs";
import * as net from "net";
import * as path from "path";
import { spawn, ChildProcess } from "child_process";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { ResourceUpdatedNotificationSchema } from "@modelcontextprotocol/sdk/types.js";

type Listener = (uri: string) => void;

export class McpBridge {
  private client?: Client;
  private spawned?: ChildProcess;
  private listeners = new Map<string, Set<Listener>>();
  private connectingPromise?: Promise<void>;
  public connected = false;

  private out?: vscode.OutputChannel;
  constructor(private readonly ctx: vscode.ExtensionContext) {
    // Reuse the extension's "Megadrive Studio" channel if it exists.
    this.out = vscode.window.createOutputChannel("Megadrive Studio (MCP)");
    ctx.subscriptions.push(this.out);
  }
  private log(s: string): void { this.out?.appendLine(`[mcp] ${s}`); }

  async connect(): Promise<void> {
    if (this.connected) return;
    if (this.connectingPromise) return this.connectingPromise;
    this.connectingPromise = this.connectInner().finally(() => {
      this.connectingPromise = undefined;
    });
    return this.connectingPromise;
  }

  private async connectInner(): Promise<void> {
    const cfg = vscode.workspace.getConfiguration("megadriveStudio");
    const url = cfg.get<string>("mcpServer", "sse://127.0.0.1:28765");
    const autoSpawn = cfg.get<boolean>("mcpAutoSpawn", true);

    const client = new Client(
      { name: "megadrive-studio", version: "0.1.0" },
      { capabilities: {} }
    );

    let useStdio = url.startsWith("stdio:");
    if (autoSpawn && !useStdio) {
      const port = parsePort(url) ?? 28765;
      // Probe first — if something already listens on the port, reuse it
      // instead of spawning a duplicate that would die on EADDRINUSE.
      const alive = await probePort("127.0.0.1", port, 400);
      if (alive) {
        this.log(`port ${port} already serving — reusing existing mds-mcp`);
      } else {
        const bin = this.locateBinary();
        if (!bin) {
          vscode.window.showWarningMessage(
            "mds-mcp binary not found. Set megadriveStudio.mdsMcpBinary or build mds-mcp/target/release/mds-mcp."
          );
          return;
        }
        const args = ["--sse", String(port)];
        // Resolve core path: explicit setting → MDS_CORE env (bundle) →
        // <bundle>/bin/<so> next to the binary if it exists.
        const cfg2 = vscode.workspace.getConfiguration("megadriveStudio");
        const coreSetting = cfg2.get<string>("clownmdemuPath", "");
        const coreEnv = process.env.MDS_CORE;
        const binDir = path.dirname(bin);
        const coreNext = path.join(binDir, "clownmdemu_libretro.so");
        const corePath =
          (coreSetting && fs.existsSync(coreSetting) && coreSetting) ||
          (coreEnv && fs.existsSync(coreEnv) && coreEnv) ||
          (fs.existsSync(coreNext) && coreNext) ||
          "";
        if (corePath) args.push("--core", corePath);
        this.log(`spawn ${bin} ${args.join(" ")}`);
        this.spawned = spawn(bin, args, {
          stdio: ["ignore", "pipe", "pipe"],
          cwd: binDir, // so any relative paths in mds-mcp resolve next to the binary
        });
        let stderrBuf = "";
        this.spawned.stderr?.on("data", (b: Buffer) => {
          stderrBuf += b.toString();
          if (stderrBuf.length > 4096) stderrBuf = stderrBuf.slice(-4096);
          this.log(`mds-mcp[stderr] ${b.toString().trim()}`);
        });
        this.spawned.on("exit", (code, sig) => {
          this.log(`mds-mcp exited code=${code} sig=${sig}`);
          this.connected = false;
        });
        this.spawned.on("error", (e) => {
          this.log(`mds-mcp spawn error: ${e}`);
          vscode.window.showErrorMessage(`Failed to start mds-mcp: ${e.message}`);
        });
        await new Promise<void>((res) => setTimeout(res, 800));
        if (this.spawned.exitCode !== null) {
          vscode.window.showErrorMessage(
            `mds-mcp exited immediately (code=${this.spawned.exitCode}). stderr tail: ${stderrBuf.slice(-300)}`
          );
          return;
        }
      }
    }

    if (useStdio) {
      const bin = this.locateBinary();
      if (!bin) {
        vscode.window.showWarningMessage("mds-mcp binary not found for stdio transport.");
        return;
      }
      const transport = new StdioClientTransport({ command: bin, args: [] });
      await client.connect(transport);
    } else {
      const httpUrl = url.replace(/^sse:\/\//, "http://").replace(/\/$/, "") + "/mcp";
      const transport = new StreamableHTTPClientTransport(new URL(httpUrl));
      try {
        await client.connect(transport);
      } catch (e) {
        vscode.window.showWarningMessage(`mds-mcp connect failed: ${e}`);
        return;
      }
    }

    client.setNotificationHandler(ResourceUpdatedNotificationSchema, async (n) => {
      const u = n.params.uri;
      const set = this.listeners.get(u);
      if (set) for (const cb of set) cb(u);
    });

    this.client = client;
    this.connected = true;
    this.ctx.subscriptions.push({ dispose: () => this.dispose() });
  }

  private locateBinary(): string | undefined {
    const cfg = vscode.workspace.getConfiguration("megadriveStudio");
    const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

    // 1. Explicit setting — resolve ${workspaceFolder} ourselves.
    const raw = cfg.get<string>("mdsMcpBinary", "");
    const explicit = raw && ws ? raw.replace(/\$\{workspaceFolder\}/g, ws) : raw;
    if (explicit && fs.existsSync(explicit)) return explicit;

    // 2. MDS_MCP env (set by bundle's start.sh).
    const env = process.env.MDS_MCP;
    if (env && fs.existsSync(env)) return env;

    // 3. dev tree.
    if (ws) {
      const p = path.join(ws, "mds-mcp", "target", "release", "mds-mcp");
      if (fs.existsSync(p)) return p;
    }

    // 4. PATH (start.sh prepends bundle bin/).
    return "mds-mcp";
  }

  async subscribe(uri: string, cb: Listener): Promise<void> {
    if (!this.client) await this.connect();
    if (!this.client) return;
    let set = this.listeners.get(uri);
    if (!set) {
      set = new Set();
      this.listeners.set(uri, set);
      try { await this.client.subscribeResource({ uri }); } catch { /* tolerate */ }
    }
    set.add(cb);
  }

  async readResource(uri: string): Promise<{ blob?: Buffer; text?: string } | undefined> {
    if (!this.client) return undefined;
    try {
      const r = await this.client.readResource({ uri });
      const c = (r.contents ?? [])[0] as { blob?: string; text?: string } | undefined;
      if (!c) return undefined;
      if (c.blob) return { blob: Buffer.from(c.blob, "base64") };
      if (c.text) return { text: c.text };
    } catch { /* ignore */ }
    return undefined;
  }

  async callTool(name: string, args?: Record<string, unknown>): Promise<unknown> {
    if (!this.client) return undefined;
    try {
      const r = await this.client.callTool({ name, arguments: args ?? {} });
      const first = (r.content as Array<{ type: string; text?: string }> | undefined)?.[0];
      if (first?.type === "text" && first.text) {
        try { return JSON.parse(first.text); } catch { return first.text; }
      }
    } catch { /* ignore */ }
    return undefined;
  }

  dispose(): void {
    try { this.client?.close(); } catch { /* */ }
    this.client = undefined;
    this.connected = false;
    if (this.spawned && !this.spawned.killed) {
      try { this.spawned.kill(); } catch { /* */ }
    }
  }
}

function parsePort(url: string): number | undefined {
  const m = url.match(/:(\d+)(?:\/|$)/);
  return m ? Number(m[1]) : undefined;
}

function probePort(host: string, port: number, timeoutMs: number): Promise<boolean> {
  return new Promise(resolve => {
    const sock = new net.Socket();
    let done = false;
    const finish = (ok: boolean) => {
      if (done) return; done = true;
      sock.destroy();
      resolve(ok);
    };
    sock.setTimeout(timeoutMs);
    sock.on("connect", () => finish(true));
    sock.on("timeout", () => finish(false));
    sock.on("error", () => finish(false));
    sock.connect(port, host);
  });
}
