// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

const VRAM_URI = "mega://vram";
const CRAM_URI = "mega://cram";
const VSRAM_URI = "mega://vsram";
const VDP_REGS_URI = "mega://vdp/registers";

let panel: vscode.WebviewPanel | undefined;

export function openPlaneViewer(ctx: vscode.ExtensionContext, bridge: McpBridge): void {
  if (panel) { panel.reveal(); return; }
  panel = vscode.window.createWebviewPanel(
    "megadriveStudio.planes",
    "Mega Drive — Plane Viewer",
    vscode.ViewColumn.Active,
    { enableScripts: true, retainContextWhenHidden: true },
  );
  panel.onDidDispose(() => { panel = undefined; }, null, ctx.subscriptions);
  panel.webview.html = planesHtml();

  panel.webview.onDidReceiveMessage((msg) => {
    if (msg?.type === "tile_click") {
      void vscode.env.clipboard.writeText(String(msg.tile));
      void vscode.window.showInformationMessage(`Copied tile index ${msg.tile} (0x${Number(msg.tile).toString(16)})`);
    }
  });

  const refresh = async (uri: string) => {
    if (!panel) return;
    const r = await bridge.readResource(uri);
    if (uri === VDP_REGS_URI) {
      const tool = (await bridge.callTool("mega_get_vdp_registers")) as { regs?: number[] } | undefined;
      panel.webview.postMessage({ type: "regs", regs: tool?.regs ?? [] });
      return;
    }
    if (!r?.blob) return;
    const key = uri === VRAM_URI ? "vram" : uri === CRAM_URI ? "cram" : "vsram";
    panel.webview.postMessage({ type: key, data: r.blob.toString("base64") });
  };

  void bridge.subscribe(VRAM_URI, () => void refresh(VRAM_URI));
  void bridge.subscribe(CRAM_URI, () => void refresh(CRAM_URI));
  void bridge.subscribe(VSRAM_URI, () => void refresh(VSRAM_URI));
  void bridge.subscribe(VDP_REGS_URI, () => void refresh(VDP_REGS_URI));
  void refresh(VRAM_URI);
  void refresh(CRAM_URI);
  void refresh(VSRAM_URI);
  void refresh(VDP_REGS_URI);
}

function planesHtml(): string {
  // Inline JS — no external CDN. Decodes 4bpp tiles, palettes via CRAM, applies whole-plane scroll only (M4).
  return `<!doctype html><html><head><style>
body{font-family:sans-serif;margin:0;padding:8px;color:#ddd;background:#1e1e1e}
.tabs{display:flex;gap:4px;margin-bottom:8px}
.tab{padding:4px 10px;background:#333;cursor:pointer;border:1px solid #444;border-radius:3px}
.tab.active{background:#0a84ff}
canvas{image-rendering:pixelated;border:1px solid #444;display:block;background:#000}
.note{font-size:11px;color:#888;margin-top:6px}
.empty{padding:20px;color:#888;font-style:italic}
</style></head><body>
<div class="tabs">
  <div class="tab active" data-p="A">Plane A</div>
  <div class="tab" data-p="B">Plane B</div>
  <div class="tab" data-p="W">Window</div>
</div>
<div id="status" class="empty">Load a ROM to see plane data</div>
<canvas id="cv" width="512" height="256"></canvas>
<div class="note">TODO: per-line/per-tile scroll modes — M4 implements whole-plane scroll only. Click a tile to copy its index.</div>
<script>
const vscode = acquireVsCodeApi();
let vram=null, cram=null, vsram=null, regs=[], plane='A';
const cv=document.getElementById('cv'), ctx=cv.getContext('2d');
const u8=b=>Uint8Array.from(atob(b),c=>c.charCodeAt(0));
function decodePal(b){if(!b)return null;const r=u8(b),o=[];for(let i=0;i+1<r.length;i+=2){const w=(r[i]<<8)|r[i+1];o.push([((w>>1)&7)<<5,((w>>5)&7)<<5,((w>>9)&7)<<5]);}return o;}
function planeSize(reg10){const hs=reg10&3, vs=(reg10>>4)&3;const m=v=>v===0?32:v===1?64:v===3?128:32;return [m(hs),m(vs)];}
function planeAddr(reg2){return (reg2&0x38)<<10;}
function planeBaddr(reg4){return (reg4&7)<<13;}
function windowAddr(reg3){return (reg3&0x3E)<<10;}
function hscrollAddr(reg0d){return (reg0d&0x3F)<<10;}
function readWord(buf,off){return ((buf[off]||0)<<8)|(buf[off+1]||0);}
function render(){
  const st=document.getElementById('status');
  if(!vram||!cram||!regs||regs.length<24){st.style.display='block';st.textContent='Load a ROM to see plane data';cv.style.display='none';return;}
  st.style.display='none';cv.style.display='block';
  const pal=decodePal(cram)||[];
  let baseAddr,sw,sh;
  if(plane==='A'){baseAddr=planeAddr(regs[2]);[sw,sh]=planeSize(regs[16]);}
  else if(plane==='B'){baseAddr=planeBaddr(regs[4]);[sw,sh]=planeSize(regs[16]);}
  else {baseAddr=windowAddr(regs[3]);sw=64;sh=32;}
  const W=sw*8,H=sh*8;cv.width=W;cv.height=H;
  // whole-plane HScroll
  let hs=0,vs=0;
  if(plane!=='W'){
    const hsAddr=hscrollAddr(regs[13]);
    const hmode=regs[11]&3;
    if(hmode===0&&vram.length>=hsAddr+4){
      hs=readWord(vram,hsAddr+(plane==='A'?0:2))&0x3FF;
    }
    const vmode=(regs[11]>>2)&1;
    if(vmode===0&&vsram&&vsram.length>=4){
      vs=readWord(vsram,plane==='A'?0:2)&0x3FF;
    }
  }
  const im=ctx.createImageData(W,H);
  for(let ty=0;ty<sh;ty++){
    for(let tx=0;tx<sw;tx++){
      const offs=baseAddr+(ty*sw+tx)*2;
      if(offs+1>=vram.length)continue;
      const w=(vram[offs]<<8)|vram[offs+1];
      const tile=w&0x7FF;
      const palIdx=(w>>13)&3;
      const hflip=(w>>11)&1, vflip=(w>>12)&1;
      const tileBase=tile*32;
      for(let py=0;py<8;py++){
        for(let px=0;px<8;px++){
          const sx=hflip?7-px:px, sy=vflip?7-py:py;
          const byte=vram[tileBase+sy*4+(sx>>1)]||0;
          const ci=(sx&1)?(byte&15):((byte>>4)&15);
          if(ci===0)continue;
          let dx=(tx*8+px-hs)%W; if(dx<0)dx+=W;
          let dy=(ty*8+py+vs)%H; if(dy<0)dy+=H;
          const c=pal[palIdx*16+ci]||[0,0,0];
          const p=(dy*W+dx)*4;
          im.data[p]=c[0];im.data[p+1]=c[1];im.data[p+2]=c[2];im.data[p+3]=255;
        }
      }
    }
  }
  ctx.putImageData(im,0,0);
}
cv.addEventListener('click',e=>{
  if(!vram||regs.length<24)return;
  const r=cv.getBoundingClientRect();
  const x=Math.floor((e.clientX-r.left)*(cv.width/r.width));
  const y=Math.floor((e.clientY-r.top)*(cv.height/r.height));
  const tx=(x>>3),ty=(y>>3);
  let baseAddr,sw;
  if(plane==='A'){baseAddr=planeAddr(regs[2]);sw=planeSize(regs[16])[0];}
  else if(plane==='B'){baseAddr=planeBaddr(regs[4]);sw=planeSize(regs[16])[0];}
  else {baseAddr=windowAddr(regs[3]);sw=64;}
  const offs=baseAddr+(ty*sw+tx)*2;
  if(offs+1<vram.length){
    const w=(vram[offs]<<8)|vram[offs+1];
    vscode.postMessage({type:'tile_click',tile:(w&0x7FF)});
  }
});
document.querySelectorAll('.tab').forEach(t=>t.addEventListener('click',()=>{
  document.querySelectorAll('.tab').forEach(x=>x.classList.remove('active'));
  t.classList.add('active');plane=t.dataset.p;render();
}));
addEventListener('message',e=>{
  const m=e.data;
  if(m.type==='vram')vram=u8(m.data);
  else if(m.type==='cram')cram=m.data;
  else if(m.type==='vsram')vsram=u8(m.data);
  else if(m.type==='regs')regs=m.regs||[];
  render();
});
</script></body></html>`;
}
