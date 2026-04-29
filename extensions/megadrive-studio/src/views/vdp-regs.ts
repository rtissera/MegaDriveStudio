// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

const VDP_REGS_URI = "mega://vdp/registers";

const VDP_REG_DESCRIPTIONS: Record<number, string> = {
  0x00: "Mode Set Register 1 — bit1=HV counter latch on TH, bit4=enable HINT, bit5=enable Hint",
  0x01: "Mode Set Register 2 — bit3=PAL/V30, bit4=DMA enable, bit5=enable VINT, bit6=display enable, bit7=128KB VRAM",
  0x02: "Plane A Name Table address (bits 13-15) shifted by 10",
  0x03: "Window Name Table address (bits 11-15) shifted by 10",
  0x04: "Plane B Name Table address (bits 13-15) shifted by 13",
  0x05: "Sprite Attribute Table address (bits 9-15) shifted by 9",
  0x06: "Sprite Pattern Generator base bit (128KB VRAM mode only)",
  0x07: "Background Colour — palette/index (bits 0-3 = colour, bits 4-5 = palette)",
  0x08: "Master System H Scroll (unused on Mega Drive)",
  0x09: "Master System V Scroll (unused on Mega Drive)",
  0x0A: "H Interrupt counter — fires HINT every (n+1) lines",
  0x0B: "Mode Set Register 3 — bits 0-1 HScroll mode, bit2=VScroll mode, bit3=External interrupt enable",
  0x0C: "Mode Set Register 4 — bit0=H40, bit1=H40 HW, bit3=shadow/highlight, bits6-7=interlace mode",
  0x0D: "H Scroll Table address (bits 6-15) shifted by 10",
  0x0E: "Plane A/B Pattern Generator base bit (128KB VRAM mode only)",
  0x0F: "Auto-increment value after VRAM/CRAM/VSRAM access",
  0x10: "Plane Size — bits 0-1 H size, bits 4-5 V size (00=32, 01=64, 11=128 tiles)",
  0x11: "Window plane H position — bits 0-4 (in tile cells *2), bit7=right side",
  0x12: "Window plane V position — bits 0-4 (in tile cells), bit7=bottom side",
  0x13: "DMA length lo",
  0x14: "DMA length hi",
  0x15: "DMA source addr lo",
  0x16: "DMA source addr mid",
  0x17: "DMA source addr hi + DMA mode (bits 6-7)",
};

interface VdpRegsResp {
  regs?: number[];
  decoded?: Record<string, unknown>;
  ok?: boolean;
  reason?: string;
}

class RegItem extends vscode.TreeItem {
  constructor(idx: number, value: number) {
    super(`Reg $${idx.toString(16).padStart(2, "0").toUpperCase()}`, vscode.TreeItemCollapsibleState.None);
    this.description = `= 0x${value.toString(16).padStart(2, "0").toUpperCase()} (${value})`;
    this.tooltip = VDP_REG_DESCRIPTIONS[idx] ?? "(no description)";
  }
}

class GroupItem extends vscode.TreeItem {
  constructor(label: string, public children: vscode.TreeItem[]) {
    super(label, vscode.TreeItemCollapsibleState.Expanded);
  }
}

export class VdpRegsProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private regs: number[] = [];
  private decoded: Record<string, unknown> = {};
  private error?: string;

  constructor(private bridge: McpBridge) {
    void this.subscribe();
  }

  private async subscribe(): Promise<void> {
    await this.bridge.subscribe(VDP_REGS_URI, () => void this.refresh());
    await this.refresh();
  }

  async refresh(): Promise<void> {
    const r = (await this.bridge.callTool("mega_get_vdp_registers")) as VdpRegsResp | undefined;
    if (!r) { this.error = "Debug API not available — rebuild the libretro core fork"; }
    else if (r.ok === false) { this.error = `Debug API not available — ${r.reason ?? "unknown"}`; }
    else {
      this.error = undefined;
      this.regs = r.regs ?? [];
      this.decoded = r.decoded ?? {};
    }
    this._onDidChange.fire();
  }

  getTreeItem(el: vscode.TreeItem): vscode.TreeItem { return el; }

  getChildren(el?: vscode.TreeItem): vscode.TreeItem[] {
    if (this.error) {
      return [new vscode.TreeItem(this.error, vscode.TreeItemCollapsibleState.None)];
    }
    if (!el) {
      const items: vscode.TreeItem[] = [];
      if (this.regs.length === 0) {
        items.push(new vscode.TreeItem("Load a ROM to see VDP registers", vscode.TreeItemCollapsibleState.None));
      } else {
        for (let i = 0; i < this.regs.length; i++) items.push(new RegItem(i, this.regs[i] ?? 0));
      }
      const decodedChildren: vscode.TreeItem[] = [];
      for (const [k, v] of Object.entries(this.decoded)) {
        const ti = new vscode.TreeItem(k, vscode.TreeItemCollapsibleState.None);
        ti.description = typeof v === "number" ? `0x${v.toString(16).toUpperCase()} (${v})` : String(v);
        decodedChildren.push(ti);
      }
      if (decodedChildren.length > 0) items.push(new GroupItem("Decoded", decodedChildren));
      return items;
    }
    if (el instanceof GroupItem) return el.children;
    return [];
  }
}
