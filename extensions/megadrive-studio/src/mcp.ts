// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import * as fs from "fs";
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

  constructor(private readonly ctx: vscode.ExtensionContext) {}

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
      const bin = this.locateBinary();
      if (!bin) {
        vscode.window.showWarningMessage(
          "mds-mcp binary not found. Set megadriveStudio.mdsMcpBinary or build mds-mcp/target/release/mds-mcp."
        );
        return;
      }
      this.spawned = spawn(bin, ["--sse", String(port)], {
        stdio: ["ignore", "pipe", "pipe"],
      });
      this.spawned.on("exit", () => { this.connected = false; });
      // Wait briefly for the listener to bind.
      await new Promise<void>((res) => setTimeout(res, 600));
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
    const explicit = cfg.get<string>("mdsMcpBinary", "");
    if (explicit && fs.existsSync(explicit)) return explicit;
    const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (ws) {
      const p = path.join(ws, "mds-mcp", "target", "release", "mds-mcp");
      if (fs.existsSync(p)) return p;
    }
    // PATH lookup — just hand back "mds-mcp" and let spawn resolve.
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
