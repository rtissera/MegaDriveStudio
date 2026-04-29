// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

const BP_URI = "mega://breakpoints";

interface Bp {
  id: number;
  addr: number;
  kind?: string;       // "exec" | "read" | "write" | "access"
  space?: string;      // "rom" | "ram" | "vram" | ...
  enabled?: boolean;
  hits?: number;
}

class BpItem extends vscode.TreeItem {
  constructor(public bp: Bp) {
    super(`#${bp.id} @ 0x${(bp.addr >>> 0).toString(16).toUpperCase().padStart(8, "0")}`,
      vscode.TreeItemCollapsibleState.None);
    const kind = bp.kind ?? "exec";
    const space = bp.space ?? "ram";
    const status = bp.enabled === false ? "disabled" : "enabled";
    this.description = `${kind} ${space} [${status}] hits=${bp.hits ?? 0}`;
    this.contextValue = "mdBreakpoint";
    this.tooltip = `Breakpoint #${bp.id} (${kind}) on ${space} at 0x${(bp.addr >>> 0).toString(16)}`;
  }
}

export class BreakpointsProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private bps: Bp[] = [];
  private error?: string;

  constructor(private bridge: McpBridge) {
    void this.subscribe();
  }

  private async subscribe(): Promise<void> {
    await this.bridge.subscribe(BP_URI, () => void this.refresh());
    await this.refresh();
  }

  async refresh(): Promise<void> {
    const r = (await this.bridge.callTool("mega_list_breakpoints")) as
      | { ok?: boolean; reason?: string; breakpoints?: Bp[] }
      | Bp[]
      | undefined;
    if (!r) { this.error = "Debug API not available — rebuild the libretro core fork"; }
    else if (Array.isArray(r)) { this.error = undefined; this.bps = r; }
    else if (r.ok === false) { this.error = `Debug API not available — ${r.reason ?? "unknown"}`; }
    else { this.error = undefined; this.bps = r.breakpoints ?? []; }
    this._onDidChange.fire();
  }

  getTreeItem(el: vscode.TreeItem): vscode.TreeItem { return el; }
  getChildren(): vscode.TreeItem[] {
    if (this.error) return [new vscode.TreeItem(this.error, vscode.TreeItemCollapsibleState.None)];
    if (this.bps.length === 0) {
      return [new vscode.TreeItem("No breakpoints set", vscode.TreeItemCollapsibleState.None)];
    }
    return this.bps.map((b) => new BpItem(b));
  }
}

export function registerBreakpointCommands(
  ctx: vscode.ExtensionContext,
  bridge: McpBridge,
  provider: BreakpointsProvider,
): void {
  ctx.subscriptions.push(
    vscode.commands.registerCommand("megadriveStudio.deleteBreakpoint", async (item: BpItem) => {
      if (!item) return;
      await bridge.callTool("mega_clear_breakpoint", { id: item.bp.id });
      await provider.refresh();
    }),
    vscode.commands.registerCommand("megadriveStudio.disableBreakpoint", async (item: BpItem) => {
      if (!item) return;
      await bridge.callTool("mega_set_breakpoint", { id: item.bp.id, enabled: false });
      await provider.refresh();
    }),
    vscode.commands.registerCommand("megadriveStudio.editBreakpointCondition", async () => {
      void vscode.window.showInformationMessage("TODO (M5+): conditional breakpoints not yet supported.");
    }),
    vscode.commands.registerCommand("megadriveStudio.clearAllBreakpoints", async () => {
      await provider.refresh();
      const cur = (await bridge.callTool("mega_list_breakpoints")) as
        | { breakpoints?: Bp[] } | Bp[] | undefined;
      const list: Bp[] = Array.isArray(cur) ? cur : (cur?.breakpoints ?? []);
      for (const b of list) await bridge.callTool("mega_clear_breakpoint", { id: b.id });
      await provider.refresh();
      void vscode.window.showInformationMessage(`Cleared ${list.length} breakpoint(s).`);
    }),
  );
}
