// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import { McpBridge } from "./mcp.js";

const VRAM_URI = "mega://vram";
const CRAM_URI = "mega://cram";
const SPRITES_URI = "mega://sprites";

abstract class CanvasView implements vscode.WebviewViewProvider {
  protected view?: vscode.WebviewView;
  constructor(protected bridge: McpBridge, private title: string) {}
  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = canvasHtml(this.title);
    void this.init();
  }
  protected abstract init(): Promise<void>;
  protected post(msg: unknown): void {
    void this.view?.webview.postMessage(msg);
  }
}

/** VRAM webview: 64 KiB rendered as 8x8 4bpp tiles in a 16-tile-wide grid. */
export class VramViewProvider extends CanvasView {
  private vram: Buffer = Buffer.alloc(0);
  private cram: Buffer = Buffer.alloc(0);
  constructor(bridge: McpBridge) { super(bridge, "VRAM"); }
  protected async init(): Promise<void> {
    void this.bridge.subscribe(VRAM_URI, () => void this.refresh(VRAM_URI));
    void this.bridge.subscribe(CRAM_URI, () => void this.refresh(CRAM_URI));
    await this.refresh(VRAM_URI);
    await this.refresh(CRAM_URI);
  }
  private async refresh(uri: string): Promise<void> {
    const r = await this.bridge.readResource(uri);
    if (!r?.blob) return;
    if (uri === VRAM_URI) this.vram = r.blob; else this.cram = r.blob;
    this.post({
      type: "vram",
      vram: this.vram.toString("base64"),
      cram: this.cram.toString("base64"),
    });
  }
}

/** CRAM webview: 4 rows × 16 colour swatches. */
export class CramViewProvider extends CanvasView {
  constructor(bridge: McpBridge) { super(bridge, "CRAM"); }
  protected async init(): Promise<void> {
    void this.bridge.subscribe(CRAM_URI, () => void this.refresh());
    await this.refresh();
  }
  private async refresh(): Promise<void> {
    const r = await this.bridge.readResource(CRAM_URI);
    if (r?.blob) this.post({ type: "cram", cram: r.blob.toString("base64") });
  }
}

interface Sprite { index: number; y: number; x: number; width: number; height: number; tile: number; palette: number; }

/** Sprites tree view: list sprites from `mega_get_sprites` tool. */
export class SpritesProvider implements vscode.TreeDataProvider<vscode.TreeItem> {
  private _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;
  private sprites: Sprite[] = [];

  constructor(private bridge: McpBridge) {
    void bridge.subscribe(SPRITES_URI, () => this.refresh());
    setInterval(() => this.refresh(), 1000);
  }

  refresh(): void { void this.load().then(() => this._onDidChange.fire()); }
  private async load(): Promise<void> {
    const r = await this.bridge.callTool("mega_get_sprites");
    if (Array.isArray(r)) this.sprites = r as Sprite[];
  }

  getTreeItem(el: vscode.TreeItem): vscode.TreeItem { return el; }
  getChildren(): vscode.TreeItem[] {
    if (this.sprites.length === 0) {
      return [new vscode.TreeItem("No sprites (ROM not loaded?)", vscode.TreeItemCollapsibleState.None)];
    }
    return this.sprites.map((s) => {
      const i = new vscode.TreeItem(`#${s.index} tile=${s.tile} ${s.width}x${s.height}`, vscode.TreeItemCollapsibleState.None);
      i.description = `(${s.x},${s.y}) pal=${s.palette}`;
      return i;
    });
  }
}

function canvasHtml(title: string): string {
  // The webview JS decodes CRAM (9-bit BGR) and renders 4bpp VRAM tiles.
  return `<!doctype html><html><body style="font-family:sans-serif;margin:0;padding:8px">
<div style="font-weight:bold;margin-bottom:6px">${title}</div>
<canvas id="c" width="256" height="256" style="image-rendering:pixelated;border:1px solid #444"></canvas>
<script>
const cv=document.getElementById('c'),ctx=cv.getContext('2d');let pal=[];
const u8=b=>Uint8Array.from(atob(b),c=>c.charCodeAt(0));
function dCram(b){const r=u8(b),o=[];for(let i=0;i+1<r.length;i+=2){const w=(r[i]<<8)|r[i+1];o.push([((w>>1)&7)<<5,((w>>5)&7)<<5,((w>>9)&7)<<5]);}return o;}
function rCram(b){pal=dCram(b);cv.width=256;cv.height=64;for(let p=0;p<4;p++)for(let i=0;i<16;i++){const c=pal[p*16+i]||[0,0,0];ctx.fillStyle='rgb('+c[0]+','+c[1]+','+c[2]+')';ctx.fillRect(i*16,p*16,16,16);}}
function rVram(b,cb){if(cb)pal=dCram(cb);const r=u8(b),t=(r.length/32)|0,cols=16,rows=Math.ceil(t/cols);cv.width=cols*8;cv.height=rows*8;const im=ctx.createImageData(cv.width,cv.height);for(let n=0;n<t;n++){const tx=(n%cols)*8,ty=((n/cols)|0)*8;for(let y=0;y<8;y++)for(let x=0;x<8;x+=2){const by=r[n*32+y*4+(x>>1)]||0,a=(by>>4)&15,b2=by&15;for(const[dx,idx]of[[0,a],[1,b2]]){const c=pal[idx]||[0,0,0],p=((ty+y)*cv.width+(tx+x+dx))*4;im.data[p]=c[0];im.data[p+1]=c[1];im.data[p+2]=c[2];im.data[p+3]=255;}}}ctx.putImageData(im,0,0);}
addEventListener('message',e=>{const m=e.data;if(m.type==='cram')rCram(m.cram);if(m.type==='vram')rVram(m.vram,m.cram);});
</script></body></html>`;
}
