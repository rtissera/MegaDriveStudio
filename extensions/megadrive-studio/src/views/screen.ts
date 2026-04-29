// SPDX-License-Identifier: MIT
//! Live Screen — webview renders the framebuffer, plus an on-screen joypad
//! and keyboard pass-through. Subscribes to `mega://framebuffer`; on each
//! notification, reads the resource (PNG bytes) and pushes the base64 into
//! the `<img>` tag. Throttled to ≤30 Hz on the client side.

import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

const FB_URI = "mega://framebuffer";

type ButtonName =
  | "up" | "down" | "left" | "right"
  | "a" | "b" | "c" | "start"
  | "x" | "y" | "z" | "mode";

export class ScreenViewProvider implements vscode.WebviewViewProvider {
  private view?: vscode.WebviewView;
  private subscribed = false;
  private retryTimer?: NodeJS.Timeout;

  constructor(private bridge: McpBridge, private log: vscode.OutputChannel) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.html();

    view.webview.onDidReceiveMessage((msg) => this.onMessage(msg));
    void this.subscribe();
    void this.firstPaint();
  }

  private async subscribe(): Promise<void> {
    if (this.subscribed) return;
    this.subscribed = true;
    await this.bridge.subscribe(FB_URI, () => void this.pushFrame());
  }

  /** One-shot capture so the panel isn't blank on first show. */
  private async firstPaint(): Promise<void> {
    const r = (await this.bridge.callTool("mega_screenshot", { format: "png" })) as
      | { ok?: boolean; reason?: string; data?: string } | undefined;
    if (r?.ok && r.data) {
      this.post({ type: "frame", b64: r.data });
      return;
    }
    if (r?.reason === "no_frame_yet") {
      this.post({ type: "status", text: "Waiting for first frame…" });
      // Retry every 500 ms until non-empty (cancel on dispose).
      if (!this.retryTimer) {
        this.retryTimer = setInterval(() => void this.retryPaint(), 500);
      }
    } else {
      this.post({ type: "status", text: "Load a ROM to see the live screen." });
    }
  }

  private async retryPaint(): Promise<void> {
    const r = (await this.bridge.callTool("mega_screenshot", { format: "png" })) as
      | { ok?: boolean; reason?: string; data?: string } | undefined;
    if (r?.ok && r.data) {
      this.post({ type: "frame", b64: r.data });
      if (this.retryTimer) { clearInterval(this.retryTimer); this.retryTimer = undefined; }
    }
  }

  private async pushFrame(): Promise<void> {
    if (this.retryTimer) { clearInterval(this.retryTimer); this.retryTimer = undefined; }
    const r = await this.bridge.readResource(FB_URI);
    if (!r?.blob || r.blob.length === 0) return;
    this.post({ type: "frame", b64: r.blob.toString("base64") });
  }

  private post(msg: unknown): void {
    void this.view?.webview.postMessage(msg);
  }

  private async onMessage(msg: { type?: string; button?: ButtonName; state?: "press" | "release" }): Promise<void> {
    if (!this.bridge.connected) return;
    try {
      switch (msg.type) {
        case "btn":
          if (!msg.button || !msg.state) return;
          await this.bridge.callTool(
            msg.state === "press" ? "mega_input_press" : "mega_input_release",
            { port: 0, button: msg.button },
          );
          break;
        case "pause":
          await this.bridge.callTool("mega_pause", {}); break;
        case "resume":
          await this.bridge.callTool("mega_resume", {}); break;
        case "refresh":
          void this.firstPaint(); break;
        case "snapshot": {
          const r = (await this.bridge.callTool("mega_screenshot", { format: "png" })) as
            | { ok?: boolean; data?: string } | undefined;
          if (r?.ok && r.data) this.post({ type: "frame", b64: r.data });
          break;
        }
      }
    } catch (e) {
      this.log.appendLine(`[screen] message error: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  private html(): string {
    // Inline CSP-friendly: scripts inline, no external assets.
    // The HTML doubles as joypad UI: D-pad on left, face buttons (X Y Z / A B C),
    // START + MODE in the middle. Keyboard bindings active when the panel has focus.
    return /* html */ `<!doctype html><html><head><meta charset="utf-8">
<style>
:root { color-scheme: dark light; }
html,body { margin:0; padding:0; font-family: var(--vscode-font-family); }
body { padding: 6px 8px 8px; }
#screen { display:block; width:100%; max-width:480px; margin:0 auto;
  image-rendering:pixelated; background:#000; border:1px solid var(--vscode-panel-border);
  aspect-ratio: 320/224; }
#status { font-size:11px; opacity:0.8; min-height:14px; margin:4px 0 6px; text-align:center; }
.toolbar { display:flex; gap:4px; justify-content:center; margin:4px 0 8px; }
.toolbar button {
  font: inherit; font-size: 11px; padding: 2px 8px; cursor:pointer;
  background: var(--vscode-button-secondaryBackground);
  color: var(--vscode-button-secondaryForeground);
  border: 1px solid var(--vscode-panel-border); border-radius: 3px;
}
.pad { display:flex; justify-content:space-between; align-items:flex-start;
  gap:8px; flex-wrap:wrap; max-width:480px; margin:0 auto; }
.dpad { display:grid; grid-template-columns: repeat(3, 28px); grid-template-rows: repeat(3, 28px); gap:2px; }
.dpad button:nth-child(1){ grid-column:2; grid-row:1; }
.dpad button:nth-child(2){ grid-column:1; grid-row:2; }
.dpad button:nth-child(3){ grid-column:3; grid-row:2; }
.dpad button:nth-child(4){ grid-column:2; grid-row:3; }
.face { display:grid; grid-template-columns: repeat(3, 32px); gap:3px; align-self:center; }
.center { display:flex; flex-direction:column; gap:4px; align-self:center; }
button.btn {
  font: inherit; font-size: 11px; padding: 0;
  background: var(--vscode-button-background);
  color: var(--vscode-button-foreground);
  border: 1px solid var(--vscode-button-border, transparent);
  border-radius: 4px; cursor: pointer;
  height:28px; min-width:28px;
  user-select:none; -webkit-user-select:none;
}
button.btn:active, button.btn.pressed { filter:brightness(1.4); }
.help { font-size:10px; opacity:0.7; text-align:center; margin-top:6px;
  border-top:1px solid var(--vscode-panel-border); padding-top:4px; }
</style></head><body tabindex="0">
<img id="screen" alt="Live screen">
<div id="status">Loading…</div>
<div class="toolbar">
  <button data-cmd="pause">⏸ Pause</button>
  <button data-cmd="resume">▶ Resume</button>
  <button data-cmd="refresh">⟳ Refresh</button>
  <button data-cmd="snapshot">📸 Snapshot</button>
</div>
<div class="pad">
  <div class="dpad">
    <button class="btn" data-btn="up">▲</button>
    <button class="btn" data-btn="left">◀</button>
    <button class="btn" data-btn="right">▶</button>
    <button class="btn" data-btn="down">▼</button>
  </div>
  <div class="center">
    <button class="btn" data-btn="start" style="width:60px">START</button>
    <button class="btn" data-btn="mode"  style="width:60px">MODE</button>
  </div>
  <div class="face">
    <button class="btn" data-btn="x">X</button>
    <button class="btn" data-btn="y">Y</button>
    <button class="btn" data-btn="z">Z</button>
    <button class="btn" data-btn="a">A</button>
    <button class="btn" data-btn="b">B</button>
    <button class="btn" data-btn="c">C</button>
  </div>
</div>
<div class="help">Keys: arrows · Z=A · X=B · C=C · A=X · S=Y · D=Z · Enter=START · Backspace=MODE</div>
<script>
(function(){
  const vscode = acquireVsCodeApi();
  const img = document.getElementById('screen');
  const status = document.getElementById('status');
  let lastPaint = 0;

  window.addEventListener('message', (e) => {
    const m = e.data;
    if (m.type === 'frame') {
      const now = performance.now();
      if (now - lastPaint < 33) return; // ≤30 Hz
      lastPaint = now;
      img.src = 'data:image/png;base64,' + m.b64;
      status.textContent = '';
    } else if (m.type === 'status') {
      status.textContent = m.text || '';
    }
  });

  // Toolbar.
  document.querySelectorAll('.toolbar button[data-cmd]').forEach((el) => {
    el.addEventListener('click', () => vscode.postMessage({ type: el.dataset.cmd }));
  });

  // On-screen joypad: pointerdown=press, pointerup/leave=release.
  function bindBtn(el) {
    const button = el.dataset.btn;
    const set = (state) => {
      el.classList.toggle('pressed', state === 'press');
      vscode.postMessage({ type: 'btn', button, state });
    };
    el.addEventListener('pointerdown', (e) => { e.preventDefault(); set('press'); el.setPointerCapture(e.pointerId); });
    el.addEventListener('pointerup',   () => set('release'));
    el.addEventListener('pointercancel', () => set('release'));
    el.addEventListener('pointerleave', () => { if (el.classList.contains('pressed')) set('release'); });
    el.addEventListener('contextmenu', (e) => e.preventDefault());
  }
  document.querySelectorAll('.btn[data-btn]').forEach(bindBtn);

  // Keyboard map (panel must be focused — body has tabindex).
  const KMAP = {
    'ArrowUp':'up','ArrowDown':'down','ArrowLeft':'left','ArrowRight':'right',
    'KeyZ':'a','KeyX':'b','KeyC':'c','KeyA':'x','KeyS':'y','KeyD':'z',
    'Enter':'start','Backspace':'mode',
  };
  const held = new Set();
  document.body.addEventListener('keydown', (e) => {
    const b = KMAP[e.code]; if (!b) return;
    e.preventDefault();
    if (held.has(b)) return; // repeat suppression
    held.add(b);
    vscode.postMessage({ type:'btn', button:b, state:'press' });
    const el = document.querySelector('[data-btn="'+b+'"]');
    if (el) el.classList.add('pressed');
  });
  document.body.addEventListener('keyup', (e) => {
    const b = KMAP[e.code]; if (!b) return;
    e.preventDefault();
    held.delete(b);
    vscode.postMessage({ type:'btn', button:b, state:'release' });
    const el = document.querySelector('[data-btn="'+b+'"]');
    if (el) el.classList.remove('pressed');
  });
  document.body.focus();
})();
</script></body></html>`;
  }
}
