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
    window.entries=window.initialEntries??[{id:'a',url:'http://localhost:12889/',label:'Editor preview'}];
    window.ipc={postMessage:m=>{
      const req=JSON.parse(m); window.sent.push(req);
      if(req.t!=='local_urls:request')return;
      const action=req.payload;
      if(action.action==='save'&&!window.failSave) window.entries=action.entries;
      queueMicrotask(()=>window.vpSidebarDispatch({t:'local_urls:result',req:req.req,
        payload:action.action==='probe'?{probe:{state:'refused'}}:{entries:window.entries},
        error:action.action==='load'&&window.failLoad?'読み込めません':action.action==='save'&&window.failSave?'保存できません':undefined}));
    }};
    const [addRequest,setAddRequest]=createSignal(0); window.addUrl=()=>setAddRequest(n=>n+1);
    window.dispose=render(()=>createComponent(LocalUrls,{repoPath:'/repo',address:'vp/lane/demo',get addRequest(){return addRequest()}}),document.body);
  `,loader:'ts',resolveDir:fileURLToPath(new URL('.',import.meta.url))},bundle:true,write:false,format:'iife',platform:'browser',plugins:[solidPlugin()],define:{'process.env.NODE_ENV':'"production"'}})
  bundle=result.outputFiles[0].text
})
async function fixture(empty=false,failLoad=false) {
  const win=new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent:any[];failSave:boolean;failLoad:boolean;entries:any[];dispose():void;addUrl():void;initialEntries?:any[]}
  windows.push(win);win.failLoad=failLoad;if(empty)win.initialEntries=[];win.eval(bundle);await win.happyDOM.waitUntilComplete();return win
}
async function flush(win: Window) { await win.happyDOM.waitUntilComplete() }
function button(win:Window,text:string) {const b=[...win.document.querySelectorAll('button')].find(b=>b.textContent===text);expect(b,`button ${text}`).toBeDefined();return b!}
function input(win:Window,label:string,value:string){const el=win.document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;expect(el).not.toBeNull();el.value=value;el.dispatchEvent(new win.Event('input',{bubbles:true}))}
afterEach(async()=>{for(const w of windows.splice(0))await w.happyDOM.abort()})
describe('registered local URLs',()=>{
  it('retains load error recovery when the add action is requested before registrations are available',async()=>{
    const win=await fixture(true,true);win.addUrl();await flush(win);
    expect(win.document.body.textContent).toContain('読み込めません');
    expect(button(win,'保存').disabled).toBe(true);
    win.failLoad=false;button(win,'再読み込み').click();await flush(win);
    expect(button(win,'保存').disabled).toBe(false);
  })

  it('hides the empty section, supports first registration and hides again after deleting the last URL',async()=>{
    const win=await fixture(true);
    expect(win.document.querySelector('.vp-local-urls')).toBeNull();
    win.addUrl();await flush(win);
    expect(win.document.querySelector('details')?.open).toBe(true);
    button(win,'キャンセル').click();await flush(win);
    expect(win.document.querySelector('.vp-local-urls')).toBeNull();
    win.addUrl();input(win,'URL','http://localhost:12889');input(win,'用途','First');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(1);expect(win.document.body.textContent).toContain('First');
    button(win,'削除').click();await flush(win);
    expect(win.entries).toHaveLength(0);expect(win.document.querySelector('.vp-local-urls')).toBeNull();
    expect(win.sent.every(r=>r.t==='local_urls:request')).toBe(true);
  })

  it('loads the lane registrations, opens the exact id and distinguishes refused from untested',async()=>{
    const win=await fixture();expect(win.document.body.textContent).toContain('Editor preview');expect(win.document.body.textContent).toContain('未確認');
    button(win,'Editor preview').click();await flush(win);
    expect(win.sent.at(-1)).toMatchObject({t:'local_urls:request',path:'/repo',address:'vp/lane/demo',payload:{action:'open',id:'a'}});
    button(win,'確認').click();await flush(win);expect(win.document.body.textContent).toContain('接続拒否');expect(win.document.body.textContent).not.toContain('停止中');
  })
  it('adds edits and removes multiple registrations without selecting the lane',async()=>{
    const win=await fixture();button(win,'URLを追加').click();input(win,'URL','http://127.0.0.1:12890');input(win,'用途','API preview');button(win,'保存').click();await flush(win);
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
