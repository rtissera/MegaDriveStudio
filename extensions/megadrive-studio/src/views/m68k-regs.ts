// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

const M68K_URI = "mega://m68k/registers";

interface M68kResp {
  d?: number[];
  a?: number[];
  pc?: number;
  sr?: number;
  usp?: number;
  ssp?: number;
  ok?: boolean;
  reason?: string;
}

function hex(v: number, n = 8): string {
  return "0x" + (v >>> 0).toString(16).padStart(n, "0").toUpperCase();
}

function decodeSr(sr: number): string {
  const t = (sr >> 15) & 1;
  const s = (sr >> 13) & 1;
  const m = (sr >> 12) & 1;
  const i = (sr >> 8) & 7;
  const x = (sr >> 4) & 1;
  const n = (sr >> 3) & 1;
  const z = (sr >> 2) & 1;
  const v = (sr >> 1) & 1;
  const c = sr & 1;
  return `T=${t} S=${s} M=${m} I=${i} X=${x} N=${n} Z=${z} V=${v} C=${c}`;
}

class RegisterItem extends vscode.TreeItem {
  constructor(public name: string, public value: number, hexWidth = 8) {
    super(name, vscode.TreeItemCollapsibleState.None);
    this.description = `${hex(value, hexWidth)} (${(value >>> 0)})`;
    this.contextValue = "mdRegister";
    this.tooltip = `${name} = ${hex(value, hexWidth)}`;
  }
}

class GroupItem extends vscode.TreeItem {
  constructor(label: string, public children: vscode.TreeItem[]) {
    super(label, vscode.TreeItemCollapsibleState.Expanded);
  }
}

export class M68kRegsProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private state?: M68kResp;
  private error?: string;

  constructor(private bridge: McpBridge) {
    void this.subscribe();
  }

  private async subscribe(): Promise<void> {
    await this.bridge.subscribe(M68K_URI, () => void this.refresh());
    await this.refresh();
  }

  async refresh(): Promise<void> {
    const r = (await this.bridge.callTool("mega_get_68k_registers")) as M68kResp | undefined;
    if (!r) this.error = "Debug API not available — rebuild the libretro core fork";
    else if (r.ok === false) this.error = `Debug API not available — ${r.reason ?? "unknown"}`;
    else { this.error = undefined; this.state = r; }
    this._onDidChange.fire();
  }

  getTreeItem(el: vscode.TreeItem): vscode.TreeItem { return el; }

  getChildren(el?: vscode.TreeItem): vscode.TreeItem[] {
    if (this.error) return [new vscode.TreeItem(this.error, vscode.TreeItemCollapsibleState.None)];
    if (!this.state) return [new vscode.TreeItem("Load a ROM to see 68k registers", vscode.TreeItemCollapsibleState.None)];
    if (!el) {
      const data: vscode.TreeItem[] = [];
      const addr: vscode.TreeItem[] = [];
      const status: vscode.TreeItem[] = [];

      const d = this.state.d ?? [];
      for (let i = 0; i < 8; i++) data.push(new RegisterItem(`D${i}`, d[i] ?? 0));
      const a = this.state.a ?? [];
      for (let i = 0; i < 8; i++) {
        const r = new RegisterItem(`A${i}`, a[i] ?? 0);
        if (i === 7 && this.state.sr !== undefined) {
          const supervisor = ((this.state.sr >> 13) & 1) === 1;
          r.description = `${hex(a[i] ?? 0)} (${supervisor ? "SSP" : "USP"})`;
        }
        addr.push(r);
      }
      status.push(new RegisterItem("PC", this.state.pc ?? 0));
      const srItem = new RegisterItem("SR", this.state.sr ?? 0, 4);
      srItem.tooltip = `SR = ${hex(this.state.sr ?? 0, 4)}\n${decodeSr(this.state.sr ?? 0)}`;
      srItem.description = `${hex(this.state.sr ?? 0, 4)}  [${decodeSr(this.state.sr ?? 0)}]`;
      status.push(srItem);
      status.push(new RegisterItem("USP", this.state.usp ?? 0));
      status.push(new RegisterItem("SSP", this.state.ssp ?? 0));

      return [
        new GroupItem("Data Registers", data),
        new GroupItem("Address Registers", addr),
        new GroupItem("Status", status),
      ];
    }
    if (el instanceof GroupItem) return el.children;
    return [];
  }
}

export function registerM68kCommands(ctx: vscode.ExtensionContext): void {
  ctx.subscriptions.push(
    vscode.commands.registerCommand("megadriveStudio.copyRegHex", (item: RegisterItem) => {
      if (!item) return;
      void vscode.env.clipboard.writeText(hex(item.value));
      void vscode.window.showInformationMessage(`Copied ${item.name} = ${hex(item.value)}`);
    }),
    vscode.commands.registerCommand("megadriveStudio.copyRegDec", (item: RegisterItem) => {
      if (!item) return;
      void vscode.env.clipboard.writeText(String(item.value >>> 0));
      void vscode.window.showInformationMessage(`Copied ${item.name} = ${item.value >>> 0}`);
    }),
  );
}
