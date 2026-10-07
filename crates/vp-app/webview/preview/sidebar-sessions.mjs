// セッションや daemon に接続しない表示・操作用 fixture。
// bun preview/sidebar-sessions.mjs 12891
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { fileURLToPath } from 'node:url'
const result = await build({
  stdin: { contents: `
    import { createComponent } from 'solid-js';
    import { render } from 'solid-js/web';
    import { Shell, SHELL_CSS } from './src/sidebar/Shell';
    import { applySidebarState, emptyState } from './src/sidebar/store';
    import { installIpcBridge } from './src/sidebar/ipc';
    import { openSidebarDispatch } from './src/sidebar/dispatch';
    import tokens from '@chronista-club/creo-ui/tokens.css';
    import components from '@chronista-club/creo-ui/components.css';
    import shim from '@chronista-club/creo-ui/token-shim.css';
    const style=document.createElement('style');style.textContent=tokens+components+shim+SHELL_CSS;document.head.append(style);
    openSidebarDispatch();installIpcBridge();
    const session=(key,agent,mode)=>({key,agent,mode,conversation:null,chat_capable:true,image_capable:false,model_choices:[],effort_choices:[],permission_choices:[],last_activity_at:null,last_response_at:null,settings:{}});
    const lane=(name,root,extra=[])=>({address:{repo:'vantage-point',name,key:'vantage-point/'+name},state:'running',agent:'claude',pid:42,cwd:'/preview'+(name==='main'?'':'/.vp/lanes/'+name),branch:name==='main'?'nightly':'wip/'+name,flow_state:'idle',sub_status:null,sessions:{root:root.key,focused:root.key,sessions:[root,...extra]}});
    let state={...emptyState(),processes:[{path:'/preview',name:'Vantage Point',state:'running',expanded:true,port:123}],lanes_by_repo:{'/preview':[lane('main',session(58,'claude','gui'),[session(57,'codex','tui')]),lane('sidebar-session-identity',session(60,'codex','gui')),lane('grok-study',session(61,'grok','tui'))]},active_lane_address:'vantage-point/sidebar-session-identity'};
    const key='vp-sidebar-session-preview-v1';
    let saved=JSON.parse(localStorage.getItem(key)||'null')||{'vantage-point/sidebar-session-identity':[{id:'editor',label:'Editor preview',url:location.origin+'/'}]};
    function feedback(text){document.getElementById('preview-status').textContent=text}
    window.ipc={postMessage(message){const m=JSON.parse(message);
      if(m.t==='local_urls:request'){
        let payload={entries:saved[m.address]||[]},error;const a=m.payload;
        if(a.action==='save'){
          try{for(const e of a.entries){const u=new URL(e.url);if(!['http:','https:'].includes(u.protocol)||!['localhost','127.0.0.1','[::1]'].includes(u.hostname)||u.username||u.password)throw Error('http(s) の loopback URL を入力してください');if(!e.label.trim())throw Error('用途を入力してください')}
          saved[m.address]=a.entries;localStorage.setItem(key,JSON.stringify(saved));payload={entries:a.entries};feedback('登録を保存しました（preview専用）')}catch(e){error=e.message}
        }
        if(a.action==='probe'){payload={probe:{state:'responding',status:200}};feedback('表示 fixture: HTTP 200（nativeの実測ではありません）')}
        if(a.action==='open'){const e=(saved[m.address]||[]).find(e=>e.id===a.id);if(e)window.open(e.url,'_blank','noopener');feedback('登録URLを開きました')}
        queueMicrotask(()=>window.vpSidebarDispatch({t:'local_urls:result',req:m.req,payload,error}));return;
      }
      if(m.t==='lane:select'){state={...state,active_lane_address:m.address};applySidebarState(state);feedback('Lane: '+m.address)}
      if(m.t==='conversation:session_focus')feedback('選択: '+m.lane+' #'+m.session);
      if(m.t==='conversation:session_remove'){
        state={...state,lanes_by_repo:{'/preview':state.lanes_by_repo['/preview'].map(l=>l.address.key===m.lane&&l.sessions.root!==m.session?{...l,sessions:{...l.sessions,sessions:l.sessions.sessions.filter(s=>s.key!==m.session)}}:l)}};applySidebarState(state);feedback('fixture #'+m.session+' を終了しました');
      }
    }};
    applySidebarState(state);render(()=>createComponent(Shell,{}),document.getElementById('sidebar-root'));
  `,loader:'ts',resolveDir:fileURLToPath(new URL('..',import.meta.url))},bundle:true,write:false,format:'iife',platform:'browser',plugins:[solidPlugin()],loader:{'.css':'text'},define:{'process.env.NODE_ENV':'"production"'},
})
const html=`<!doctype html><html lang="ja" data-theme="contrast-dark"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Lane Sessions — Preview</title><style>
html,body{margin:0;height:100%;background:var(--color-surface-bg-base,#111518);color:var(--color-text-primary,#e7edf0);font-family:system-ui,sans-serif}#app-shell{display:flex;height:100vh}#sidebar-root{width:330px;flex:none;border-right:1px solid #ffffff18}#host{padding:64px 48px;flex:1;overflow:auto}h1{font-size:28px;font-weight:500}p{line-height:1.9;color:var(--color-text-secondary,#a8b1bb)}small{letter-spacing:.08em;color:var(--color-text-tertiary,#718087)}#preview-status{margin-top:36px;padding:18px;border:1px solid #ffffff20;border-radius:10px;font-size:13px}button:focus-visible,summary:focus-visible{outline:2px solid #93c4f2;outline-offset:2px}@media(max-width:700px){#sidebar-root{width:290px}#host{padding:24px}}
</style><body><div id="app-shell"><div id="sidebar-root"></div><main id="host"><small>LANE / SESSIONS / LOCAL URL</small><h1>どの作業台で、誰が動いているか。</h1><p>Lane の代表 Agent と追加セッションを見分け、<br>使っているローカル URL を作業台ごとに残します。</p><p>Lane 行の右クリックから「ローカルURLを追加」を選べます。<br>登録後は「ローカル URL」の節で編集・削除を試せます。<br>追加セッションの × は、対象番号を確認してから終了します。</p><p>これは独立したプレビューです。実際の会話や daemon には接続しません。<br>到達状態は表示用 fixture、登録内容はこの preview 内だけに保存します。</p><div id="preview-status">操作を待っています</div></main></div><script src="/preview.js"></script></body></html>`
const server=Bun.serve({hostname:'127.0.0.1',port:Number(process.argv[2]??12891),fetch(request){const path=new URL(request.url).pathname;return path==='/preview.js'?new Response(result.outputFiles[0].text,{headers:{'Content-Type':'text/javascript; charset=utf-8'}}):path==='/'?new Response(html,{headers:{'Content-Type':'text/html; charset=utf-8'}}):new Response('Not found',{status:404})}})
console.log('Sidebar session preview: '+server.url)
