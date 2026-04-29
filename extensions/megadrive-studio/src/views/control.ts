// SPDX-License-Identifier: MIT
//! "Sega Control" tree view — top of the activity bar. Single source of
//! truth for emulator control: ROM info, MCP connection, run state. The
//! view-title row hosts Load / Unload / Pause / Resume / Reset buttons; the
//! body shows live-refreshing status lines (1 s tick).

import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

interface Status {
  rom_loaded?: boolean;
  paused?: boolean;
  frame?: number;
  fps_avg?: number;
  target?: string;
  libra_linked?: boolean;
  connected?: boolean;
}

interface RomMeta {
  path?: string;
  size?: number;
  header_name?: string;
  region?: string;
}

/** Records the most recent ROM that was successfully loaded so the view can
 * surface it without re-walking the filesystem. */
export class ControlState {
  private rom?: RomMeta;
  private listeners = new Set<() => void>();
  setRom(meta: RomMeta | undefined): void { this.rom = meta; this.fire(); }
  getRom(): RomMeta | undefined { return this.rom; }
  onChange(fn: () => void): void { this.listeners.add(fn); }
  private fire(): void { for (const l of this.listeners) l(); }
}

export class ControlProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private status?: Status;
  private timer?: NodeJS.Timeout;

  constructor(private bridge: McpBridge, private state: ControlState) {
    state.onChange(() => this._onDidChange.fire());
    // Refresh every second.
    this.timer = setInterval(() => this.tick(), 1000);
    void this.tick();
  }

  dispose(): void { if (this.timer) clearInterval(this.timer); }

  private async tick(): Promise<void> {
    if (!this.bridge.connected) {
      this.status = undefined;
      this._onDidChange.fire();
      return;
    }
    const s = (await this.bridge.callTool("mega_get_status", {})) as Status | undefined;
    this.status = s;
    this._onDidChange.fire();
  }

  getTreeItem(el: vscode.TreeItem): vscode.TreeItem { return el; }

  getChildren(): vscode.TreeItem[] {
    const items: vscode.TreeItem[] = [];

    // Status line.
    const stateLabel = !this.status
      ? "Status: disconnected"
      : !this.status.rom_loaded
        ? "Status: idle (no ROM)"
        : this.status.paused
          ? `Status: paused (frame ${this.status.frame ?? 0})`
          : `Status: running (frame ${this.status.frame ?? 0})`;
    const stateItem = new vscode.TreeItem(stateLabel, vscode.TreeItemCollapsibleState.None);
    stateItem.iconPath = new vscode.ThemeIcon(
      !this.status ? "circle-slash"
        : this.status.rom_loaded
          ? (this.status.paused ? "debug-pause" : "play")
          : "stop-circle"
    );
    if (this.status?.fps_avg !== undefined && this.status.rom_loaded) {
      stateItem.description = `${this.status.fps_avg.toFixed(1)} fps`;
    }
    items.push(stateItem);

    // ROM line.
    const rom = this.state.getRom();
    if (rom?.path) {
      const sizeKb = rom.size !== undefined ? `${(rom.size / 1024).toFixed(1)} KB` : "?";
      const name = rom.header_name?.trim() || rom.path.split("/").pop() || rom.path;
      const r = new vscode.TreeItem(`ROM: ${name}`, vscode.TreeItemCollapsibleState.None);
      r.description = rom.region ? `${sizeKb} · ${rom.region}` : sizeKb;
      r.tooltip = rom.path;
      r.iconPath = new vscode.ThemeIcon("file-binary");
      items.push(r);
    } else {
      const r = new vscode.TreeItem("ROM: not loaded", vscode.TreeItemCollapsibleState.None);
      r.description = "click [Load ROM] in the title bar";
      r.iconPath = new vscode.ThemeIcon("file-binary");
      items.push(r);
    }

    // Connection line.
    const cn = new vscode.TreeItem(
      this.bridge.connected ? "Connection: connected" : "Connection: not connected",
      vscode.TreeItemCollapsibleState.None,
    );
    cn.iconPath = new vscode.ThemeIcon(this.bridge.connected ? "vm-active" : "vm-outline");
    if (this.status) {
      const target = this.status.target ?? "emulator";
      cn.description = `${target}${this.status.libra_linked ? "" : " (libra missing)"}`;
    }
    items.push(cn);

    return items;
  }

  /** External nudge — call after Load/Unload/Pause/Resume/Reset commands. */
  refresh(): void { void this.tick(); }
}
