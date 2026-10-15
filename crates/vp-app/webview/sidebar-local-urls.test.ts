// mem_1CfkeiUePgFsbYoGtTeDpq
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLInputElement } from 'happy-dom'
import { fileURLToPath } from 'node:url'
let bundle: string
const windows: Window[]=[]
beforeAll(async () => {
  const result=await build({stdin:{contents:`
    import {render} from 'solid-js/web';
    import {createComponent,createSignal} from 'solid-js';
    import {LocalUrls} from './src/sidebar/LocalUrls';
    import {installIpcBridge} from './src/sidebar/ipc';
    import {openSidebarDispatch} from './src/sidebar/dispatch';
    openSidebarDispatch(); installIpcBridge();
    window.sent=[]; window.failSave=false;
    window.setInterval=(callback)=>{window.refreshUrls=callback;return 42}; window.clearInterval=()=>{};
    window.entries=window.initialEntries??[{id:'a',url:'http://localhost:12889/',label:'Editor preview'}];
    window.ipc={postMessage:m=>{
      const req=JSON.parse(m); window.sent.push(req);
      if(req.t!=='local_urls:request')return;
      const action=req.payload;
      const conflict=action.action==='save'&&JSON.stringify(action.expected)!==JSON.stringify(window.entries);
      if(action.action==='save'&&!window.failSave&&!conflict) window.entries=action.entries;
      queueMicrotask(()=>window.vpSidebarDispatch({t:'local_urls:result',req:req.req,
        payload:action.action==='probe'||action.action==='probe_url'?{probe:{state:'refused'}}:{entries:window.entries},
        error:conflict?'別の画面で更新されました':action.action==='load'&&window.failLoad?'読み込めません':action.action==='save'&&window.failSave?'保存できません':undefined}));
    }};
    const [active,setActive]=createSignal(!window.initiallyHidden); window.setActive=setActive;
    const [address,setAddress]=createSignal('vp/lane/demo'); window.setAddress=setAddress;
    const [discovered,setDiscovered]=createSignal(window.initialDiscovered??[]); window.setDiscovered=setDiscovered;
    const [pr,setPr]=createSignal(null); window.setPr=setPr;
    const [addRequest,setAddRequest]=createSignal(0); window.addUrl=()=>setAddRequest(n=>n+1);
    window.dispose=render(()=>createComponent(LocalUrls,{repoPath:'/repo',get address(){return address()},get active(){return active()},get discovered(){return discovered()},get pr(){return pr()},get addRequest(){return addRequest()}}),document.body);
  `,loader:'ts',resolveDir:fileURLToPath(new URL('.',import.meta.url))},bundle:true,write:false,format:'iife',platform:'browser',plugins:[solidPlugin()],define:{'process.env.NODE_ENV':'"production"'}})
  bundle=result.outputFiles[0].text
})
async function fixture(empty=false,failLoad=false,hidden=false,discovered:string[]=[]) {
  const win=new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent:any[];failSave:boolean;failLoad:boolean;entries:any[];dispose():void;addUrl():void;refreshUrls():void;initialEntries?:any[];initialDiscovered:string[];initiallyHidden:boolean;setActive(value:boolean):void;setAddress(value:string):void;setDiscovered(urls:string[]):void;setPr(pr:any):void}
  windows.push(win);win.failLoad=failLoad;win.initiallyHidden=hidden;win.initialDiscovered=discovered;if(empty)win.initialEntries=[];win.eval(bundle);await win.happyDOM.waitUntilComplete();return win
}
async function flush(win: Window) { await win.happyDOM.waitUntilComplete() }
function button(win:Window,text:string) {const b=[...win.document.querySelectorAll('button')].find(b=>b.textContent===text);expect(b,`button ${text}`).toBeDefined();return b!}
function input(win:Window,label:string,value:string){const el=win.document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;expect(el).not.toBeNull();el.value=value;el.dispatchEvent(new win.Event('input',{bubbles:true}))}
afterEach(async()=>{for(const w of windows.splice(0))await w.happyDOM.abort()})
describe('registered local URLs',()=>{
  it('deduplicates discovered registrations before automatically probing',async()=>{
    const win=await fixture(false,false,false,['http://localhost:12889/']);
    expect(win.document.querySelector('.vp-discovered-url')).toBeNull();
    expect(win.sent.filter(r=>r.payload?.action==='probe_url')).toHaveLength(0);
  })
  it('requests only the viewed lane while the Lane section is visible',async()=>{
    const win=await fixture(true,false,true);
    expect(win.sent).toHaveLength(0);
    win.setActive(true);await flush(win);
    expect(win.sent).toHaveLength(1);
    expect(win.sent[0].address).toBe('vp/lane/demo');
    win.setAddress('vp/second');await flush(win);
    expect(win.sent).toHaveLength(2);
    expect(win.sent[1].address).toBe('vp/second');
    win.setActive(false);win.refreshUrls();await flush(win);
    expect(win.sent).toHaveLength(2);
    win.setAddress('vp/third');await flush(win);
    expect(win.sent).toHaveLength(2);
  })
  it('shows discovered URL, shared PR and saved entries in order; saves only on registration',async()=>{
    const win=await fixture();
    win.setDiscovered(['http://localhost:5173/']);
    win.setPr({number:42,title:'Lane links',url:'https://github.com/org/repo/pull/42'});
    await flush(win);
    expect(win.entries).toHaveLength(1);
    expect(win.sent.some(r=>r.payload?.action==='save')).toBe(false);
    const text=win.document.body.textContent!;
    expect(text.indexOf('http://localhost:5173/')).toBeLessThan(text.indexOf('#42'));
    expect(text.indexOf('#42')).toBeLessThan(text.indexOf('Editor preview'));
    button(win,'登録').click();input(win,'名前','dev');input(win,'用途','Dev server');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(2);
    expect(win.entries[1]).toMatchObject({id:'dev',url:'http://localhost:5173/'});
    expect(win.sent.filter(r=>r.t==='lane:prs_fetch')).toHaveLength(0);
  })
  it('reflects external named registrations but preserves a draft and rejects stale saves',async()=>{
    const win=await fixture(true);
    win.entries=[{id:'preview',url:'http://localhost:5173',label:'From CLI'}];
    win.refreshUrls();await flush(win);
    expect(win.document.body.textContent).toContain('From CLI');
    button(win,'編集').click();input(win,'用途','draft');
    win.entries=[{id:'preview',url:'http://localhost:5174',label:'From MCP'}];
    win.refreshUrls();await flush(win);
    expect(win.document.querySelector<HTMLInputElement>('[aria-label="用途"]')!.value).toBe('draft');
    button(win,'保存').click();await flush(win);
    expect(win.document.body.textContent).toContain('別の画面で更新');
    expect(win.entries[0].label).toBe('From MCP');
    button(win,'キャンセル').click();button(win,'再読み込み').click();await flush(win);
    expect(win.document.body.textContent).toContain('From MCP');
    win.entries=[];win.refreshUrls();await flush(win);
    expect(win.document.querySelector('.vp-local-url-entry')).toBeNull();
    expect(win.sent.some(r=>r.payload.action==='probe')).toBe(false);
  })

  it('retains load error recovery when the add action is requested before registrations are available',async()=>{
    const win=await fixture(true,true);win.addUrl();await flush(win);
    expect(win.document.body.textContent).toContain('読み込めません');
    expect(button(win,'保存').disabled).toBe(true);
    win.failLoad=false;button(win,'再読み込み').click();await flush(win);
    expect(button(win,'保存').disabled).toBe(false);
  })

  it('keeps the first-registration entry point in the Lane section, including after deletion',async()=>{
    const win=await fixture(true);
    expect(button(win,'URLを追加').disabled).toBe(false);
    win.addUrl();await flush(win);
    expect(win.document.querySelector('.vp-local-url-form')).not.toBeNull();
    button(win,'キャンセル').click();await flush(win);
    expect(win.document.querySelector('.vp-local-url-form')).toBeNull();
    win.addUrl();input(win,'名前','preview');input(win,'URL','http://localhost:12889');input(win,'用途','First');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(1);expect(win.document.body.textContent).toContain('First');
    button(win,'削除').click();await flush(win);
    expect(win.entries).toHaveLength(0);expect(button(win,'URLを追加').disabled).toBe(false);
    expect(win.sent.every(r=>r.t==='local_urls:request')).toBe(true);
  })

  it('loads the lane registrations, opens the exact id and distinguishes refused from untested',async()=>{
    const win=await fixture();expect(win.document.body.textContent).toContain('Editor preview');expect(win.document.body.textContent).toContain('未確認');
    button(win,'Editor preview').click();await flush(win);
    expect(win.sent.at(-1)).toMatchObject({t:'local_urls:request',path:'/repo',address:'vp/lane/demo',payload:{action:'open',id:'a'}});
    button(win,'確認').click();await flush(win);expect(win.document.body.textContent).toContain('接続拒否');expect(win.document.body.textContent).not.toContain('停止中');
  })
  it('adds edits and removes multiple registrations without selecting the lane',async()=>{
    const win=await fixture();button(win,'URLを追加').click();input(win,'名前','api');input(win,'URL','http://127.0.0.1:12890');input(win,'用途','API preview');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(2);expect(win.document.body.textContent).toContain('API preview');
    button(win,'編集').click();input(win,'用途','New editor');button(win,'保存').click();await flush(win);expect(win.document.body.textContent).toContain('New editor');
    button(win,'削除').click();await flush(win);expect(win.entries).toHaveLength(1);expect(win.entries[0].label).toBe('API preview');
    expect(win.sent.every(r=>r.t==='local_urls:request')).toBe(true)
  })
  it('keeps the form and persisted entries when save fails',async()=>{
    const win=await fixture();win.failSave=true;button(win,'編集').click();input(win,'用途','unsaved');button(win,'保存').click();await flush(win);
    expect(win.document.body.textContent).toContain('保存できません');expect(win.entries[0].label).toBe('Editor preview');expect(win.document.querySelector<HTMLInputElement>('[aria-label="用途"]')!.value).toBe('unsaved');
  })
})
