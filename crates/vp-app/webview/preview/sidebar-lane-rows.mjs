// mem_1Cg7MpHWBdf6u9CYjyEAaf — daemon に接続しない lane 行の表示 fixture。
import { build } from 'esbuild';
import { solidPlugin } from 'esbuild-plugin-solid';
import { fileURLToPath } from 'node:url';
const result = await build({stdin:{contents:`
import {render} from 'solid-js/web';
import {createComponent} from 'solid-js';
import {Shell,SHELL_CSS} from './src/sidebar/Shell';
import {applySidebarState,emptyState} from './src/sidebar/store';
import wire from './fixtures/lane-progress-wire.json';
import tokens from '@chronista-club/creo-ui/tokens.css';
import components from '@chronista-club/creo-ui/components.css';
import shim from '@chronista-club/creo-ui/token-shim.css';
const style=document.createElement('style');style.textContent=tokens+components+shim+SHELL_CSS;document.head.append(style);
const lanes=Object.values(wire);
lanes.forEach((lane,i)=>{lane.sessions.sessions[0].last_activity_at=Date.now()-i*180000;});
lanes[0].address={repo:'project',name:'main',key:'project/main'};
lanes[0].branch='nightly';
let state={...emptyState(),processes:[{path:'/project',name:'Vantage Point',state:'running',expanded:true}],lanes_by_repo:{'/project':lanes},active_lane_address:'project/lane/working'};
window.ipc={postMessage(raw){const m=JSON.parse(raw);if(m.t==='process:toggle')state.processes[0].expanded=m.expanded;if(m.t==='lane:select')state.active_lane_address=m.address;applySidebarState({...state});}};
applySidebarState(state);
render(()=>createComponent(Shell,{}),document.getElementById('sidebar-root'));
`,loader:'tsx',resolveDir:fileURLToPath(new URL('..',import.meta.url))},bundle:true,write:false,format:'iife',platform:'browser',plugins:[solidPlugin()],loader:{'.css':'text'},define:{'process.env.NODE_ENV':'"production"'}});
const html=`<!doctype html><html lang="ja" data-theme="contrast-dark"><meta charset="utf-8"><title>Lane rows preview</title><style>html,body{margin:0;height:100%;background:#111518;color:#cdd8dc;font-family:system-ui}body{display:flex}#sidebar-root{width:330px;height:100vh;flex:none;border-right:1px solid #ffffff20}#host{padding:50px}p{max-width:480px;line-height:1.8}</style><div id="sidebar-root"></div><main id="host"><h1>Lane の状態と進行</h1><p>Rust wire fixture を使う独立した表示確認です。実際の会話や daemon には接続しません。</p></main><script src="/preview.js"></script></html>`;
const server=Bun.serve({hostname:'127.0.0.1',port:Number(process.argv[2]??12892),fetch(request){return new Response(new URL(request.url).pathname==='/preview.js'?result.outputFiles[0].text:html,{headers:{'Content-Type':new URL(request.url).pathname==='/preview.js'?'text/javascript':'text/html'}});}});
console.log(server.url.href);
