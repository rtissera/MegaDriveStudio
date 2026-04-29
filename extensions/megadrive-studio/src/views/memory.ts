// SPDX-License-Identifier: MIT
import * as vscode from "vscode";
import { McpBridge } from "../mcp.js";

let panel: vscode.WebviewPanel | undefined;
let pollTimer: NodeJS.Timeout | undefined;

interface MemReadResp {
  ok?: boolean;
  reason?: string;
  data?: string; // base64
  addr?: number;
  space?: string;
}

interface BpListItem { id: number; addr: number; kind?: string; space?: string; }

export function open68kMemory(ctx: vscode.ExtensionContext, bridge: McpBridge): void {
  if (panel) { panel.reveal(); return; }
  panel = vscode.window.createWebviewPanel(
    "megadriveStudio.memory",
    "Mega Drive — 68k Memory",
    vscode.ViewColumn.Active,
    { enableScripts: true, retainContextWhenHidden: true },
  );
  panel.onDidDispose(() => {
    panel = undefined;
    if (pollTimer) { clearInterval(pollTimer); pollTimer = undefined; }
  }, null, ctx.subscriptions);
  panel.webview.html = memoryHtml();

  let curAddr = 0xFF0000;
  let curSpace = "ram";
  let paused = false;

  const sendBps = async () => {
    if (!panel) return;
    const r = (await bridge.callTool("mega_list_breakpoints")) as
      | { breakpoints?: BpListItem[] } | BpListItem[] | undefined;
    const list: BpListItem[] = Array.isArray(r) ? r : (r?.breakpoints ?? []);
    panel.webview.postMessage({ type: "bps", bps: list });
  };

  const fetchAndPost = async (addr: number, space: string) => {
    if (!panel) return;
    const r = (await bridge.callTool("mega_read_memory", { addr, space, length: 0x400 })) as
      | MemReadResp | undefined;
    if (!r) {
      panel.webview.postMessage({ type: "error", message: "Debug API not available — rebuild the libretro core fork" });
      return;
    }
    if (r.ok === false) {
      panel.webview.postMessage({ type: "error", message: `Debug API not available — ${r.reason ?? "unknown"}` });
      return;
    }
    panel.webview.postMessage({ type: "mem", addr, space, data: r.data ?? "" });
    await sendBps();
  };

  const tickPaused = async () => {
    const s = (await bridge.callTool("mega_get_status")) as { paused?: boolean } | undefined;
    paused = !!s?.paused;
  };

  panel.webview.onDidReceiveMessage(async (msg) => {
    if (msg?.type === "navigate") {
      curAddr = (Number(msg.addr) >>> 0) & 0xFFFFFFFF;
      curSpace = String(msg.space ?? "ram");
      await fetchAndPost(curAddr, curSpace);
    } else if (msg?.type === "set_watch") {
      const addr = Number(msg.addr) >>> 0;
      const kind = String(msg.kind);
      const r = (await bridge.callTool("mega_set_breakpoint", { addr, kind, space: curSpace })) as
        | { ok?: boolean; reason?: string } | undefined;
      if (!r || r.ok === false) {
        void vscode.window.showWarningMessage(`Failed to set watchpoint: ${r?.reason ?? "debug API unavailable"}`);
      } else {
        void vscode.window.showInformationMessage(`Watchpoint (${kind}) set at 0x${addr.toString(16).toUpperCase()}`);
      }
      await sendBps();
    } else if (msg?.type === "goto_pc") {
      const r = (await bridge.callTool("mega_get_68k_registers")) as { pc?: number } | undefined;
      if (r?.pc !== undefined) {
        curAddr = r.pc >>> 0;
        panel?.webview.postMessage({ type: "set_addr", addr: curAddr });
        await fetchAndPost(curAddr, curSpace);
      } else {
        void vscode.window.showWarningMessage("PC unavailable — debug API not ready.");
      }
    }
  });

  pollTimer = setInterval(async () => {
    await tickPaused();
    if (!paused) await fetchAndPost(curAddr, curSpace);
  }, 1000);
  ctx.subscriptions.push({ dispose: () => { if (pollTimer) clearInterval(pollTimer); } });

  // initial load
  void fetchAndPost(curAddr, curSpace);
}

function memoryHtml(): string {
  return `<!doctype html><html><head><style>
body{font-family:'Courier New',monospace;margin:0;padding:8px;color:#ddd;background:#1e1e1e;font-size:12px}
.bar{display:flex;gap:6px;align-items:center;margin-bottom:8px;font-family:sans-serif}
.bar input{background:#252526;color:#ddd;border:1px solid #555;padding:2px 6px;font-family:monospace}
.bar select{background:#252526;color:#ddd;border:1px solid #555}
.bar button{background:#0a84ff;color:#fff;border:0;padding:3px 10px;cursor:pointer}
.row{white-space:pre}
.byte{display:inline-block;width:1.6em;text-align:center;cursor:context-menu}
.byte.bp-r{background:#604;color:#fff}
.byte.bp-w{background:#460;color:#fff}
.byte.bp-a{background:#646;color:#fff}
.addr{color:#888}
.ascii{color:#9c9}
.error{color:#f66;padding:6px;font-family:sans-serif}
#menu{position:absolute;background:#252526;border:1px solid #555;display:none;z-index:10}
#menu div{padding:4px 12px;cursor:pointer}
#menu div:hover{background:#0a84ff}
</style></head><body>
<div class="bar">
  <label>Addr <input id="addr" value="0xFF0000" size="10"/></label>
  <select id="space">
    <option value="rom">ROM</option>
    <option value="ram" selected>RAM</option>
    <option value="vram">VRAM</option>
    <option value="cram">CRAM</option>
    <option value="vsram">VSRAM</option>
    <option value="z80ram">Z80RAM</option>
  </select>
  <button id="go">Go</button>
  <button id="gopc" title="Right-click navigate to PC">Go to PC</button>
</div>
<div id="err" class="error" style="display:none"></div>
<div id="hex"></div>
<div id="menu">
  <div data-k="read">Set Watchpoint Read</div>
  <div data-k="write">Set Watchpoint Write</div>
  <div data-k="access">Set Watchpoint Access</div>
</div>
<script>
const vscode=acquireVsCodeApi();
let baseAddr=0xFF0000, bytes=null, bps=[], curSpace='ram';
const u8=b=>Uint8Array.from(atob(b),c=>c.charCodeAt(0));
function bpClass(addr){
  for(const b of bps){
    if((b.space||'ram')!==curSpace)continue;
    if((b.addr>>>0)===(addr>>>0)){
      if(b.kind==='read')return 'bp-r';
      if(b.kind==='write')return 'bp-w';
      if(b.kind==='access')return 'bp-a';
      return 'bp-a';
    }
  }
  return '';
}
function render(){
  if(!bytes){document.getElementById('hex').textContent='';return;}
  const out=[];
  for(let row=0;row<bytes.length;row+=16){
    const a=(baseAddr+row)>>>0;
    let line='<span class="addr">'+a.toString(16).toUpperCase().padStart(8,'0')+':</span> ';
    let ascii='';
    for(let i=0;i<16;i++){
      const off=row+i;
      if(off>=bytes.length){line+='   ';ascii+=' ';continue;}
      const v=bytes[off];
      const cls=bpClass(baseAddr+off);
      line+='<span class="byte '+cls+'" data-addr="'+(baseAddr+off)+'">'+v.toString(16).toUpperCase().padStart(2,'0')+'</span> ';
      ascii+=(v>=32&&v<127)?String.fromCharCode(v):'.';
    }
    line+=' <span class="ascii">'+ascii.replace(/[<&>]/g,c=>({"<":"&lt;","&":"&amp;",">":"&gt;"}[c]))+'</span>';
    out.push('<div class="row">'+line+'</div>');
  }
  document.getElementById('hex').innerHTML=out.join('');
}
const menu=document.getElementById('menu');let menuAddr=0;
document.getElementById('hex').addEventListener('contextmenu',e=>{
  const t=e.target.closest('.byte');if(!t)return;
  e.preventDefault();menuAddr=Number(t.dataset.addr);
  menu.style.display='block';menu.style.left=e.pageX+'px';menu.style.top=e.pageY+'px';
});
document.addEventListener('click',e=>{
  if(!menu.contains(e.target))menu.style.display='none';
});
menu.querySelectorAll('div').forEach(d=>d.addEventListener('click',()=>{
  vscode.postMessage({type:'set_watch',addr:menuAddr,kind:d.dataset.k});
  menu.style.display='none';
}));
document.getElementById('go').addEventListener('click',()=>{
  const a=parseInt(document.getElementById('addr').value.replace(/^[$0x]+/,''),16)>>>0;
  curSpace=document.getElementById('space').value;
  vscode.postMessage({type:'navigate',addr:a,space:curSpace});
});
document.getElementById('addr').addEventListener('keydown',e=>{
  if(e.key==='Enter')document.getElementById('go').click();
});
document.getElementById('space').addEventListener('change',()=>{
  curSpace=document.getElementById('space').value;
  document.getElementById('go').click();
});
document.getElementById('gopc').addEventListener('click',()=>vscode.postMessage({type:'goto_pc'}));
document.getElementById('addr').addEventListener('contextmenu',e=>{
  e.preventDefault();vscode.postMessage({type:'goto_pc'});
});
addEventListener('message',e=>{
  const m=e.data;
  if(m.type==='mem'){
    document.getElementById('err').style.display='none';
    baseAddr=m.addr>>>0;curSpace=m.space;
    bytes=m.data?u8(m.data):new Uint8Array(0);render();
  } else if(m.type==='bps'){bps=m.bps||[];render();}
  else if(m.type==='error'){
    document.getElementById('err').style.display='block';
    document.getElementById('err').textContent=m.message;
    document.getElementById('hex').innerHTML='';
  } else if(m.type==='set_addr'){
    document.getElementById('addr').value='0x'+(m.addr>>>0).toString(16).toUpperCase();
  }
});
</script></body></html>`;
}
