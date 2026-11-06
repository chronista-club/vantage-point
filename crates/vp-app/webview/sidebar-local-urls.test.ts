// mem_1CgBLigkfajejEpmcRn3v2 / mem_1CfkeiUePgFsbYoGtTeDpq / mem_1CgB31Xk419PtPkcK6yzLp / mem_1CgBHtLEoHBUibQLBU3vbs
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
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
    window.sent=[]; window.failSave=false; window.deferProbes=false; window.pendingProbes=[];
    window.replyProbe=(req,probe)=>window.vpSidebarDispatch({t:'local_urls:result',req:req.req,payload:{probe}});
    window.setInterval=(callback)=>{window.refreshUrls=callback;return 42}; window.clearInterval=()=>{};
    window.entries=window.initialEntries??[{id:'a',url:'http://localhost:12889/',label:'Editor preview'}];
    window.ipc={postMessage:m=>{
      const req=JSON.parse(m); window.sent.push(req);
      if(req.t!=='local_urls:request')return;
      const action=req.payload;
      if(window.deferProbes&&(action.action==='probe'||action.action==='probe_url')){window.pendingProbes.push(req);return;}
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
  `,loader:'ts',resolveDir:fileURLToPath(new URL('.',import.meta.url))},bundle:true,write:false,format:'iife',platform:'browser',preserveSymlinks:true,plugins:[solidPlugin()],define:{'process.env.NODE_ENV':'"production"'}})
  bundle=result.outputFiles[0].text
})
async function fixture(empty=false,failLoad=false,hidden=false,discovered:string[]=[],entries?:any[]) {
  const win=new Window({settings:{enableJavaScriptEvaluation:true,suppressInsecureJavaScriptEnvironmentWarning:true}}) as Window & {sent:any[];deferProbes:boolean;pendingProbes:any[];replyProbe(req:any,probe:any):void;failSave:boolean;failLoad:boolean;entries:any[];dispose():void;addUrl():void;refreshUrls():void;initialEntries?:any[];initialDiscovered:string[];initiallyHidden:boolean;setActive(value:boolean):void;setAddress(value:string):void;setDiscovered(urls:string[]):void;setPr(pr:any):void}
  windows.push(win);win.failLoad=failLoad;win.initiallyHidden=hidden;win.initialDiscovered=discovered;if(entries)win.initialEntries=entries;else if(empty)win.initialEntries=[];win.eval(bundle);await win.happyDOM.waitUntilComplete();return win
}
async function flush(win: Window) { await win.happyDOM.waitUntilComplete() }
function button(win:Window,text:string) {const b=[...win.document.querySelectorAll('button')].find(b=>b.textContent===text);expect(b,`button ${text}`).toBeDefined();return b!}
function input(win:Window,label:string,value:string){const el=win.document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;expect(el).not.toBeNull();el.value=value;el.dispatchEvent(new win.Event('input',{bubbles:true}))}
afterEach(async()=>{for(const w of windows.splice(0))await w.happyDOM.abort()})
describe('registered local URLs',()=>{
  it.each([false,true])('offers only name and URL, with an immutable name when editing=%s',async editing=>{
    const win=await fixture();
    button(win,editing?'編集':'URLを追加').click();await flush(win);
    const fields=[...win.document.querySelectorAll<HTMLInputElement>('.vp-local-url-form input')];
    expect(fields.map(field=>field.getAttribute('aria-label'))).toEqual(['名前','URL']);
    expect(fields[0].disabled).toBe(editing);
    expect(fields[0].value).toBe(editing?'a':'');
    expect(fields[1].disabled).toBe(false);
  })

  it('saves the fixed name as label when editing an existing custom label',async()=>{
    const win=await fixture();
    button(win,'編集').click();input(win,'URL','http://localhost:5175/');button(win,'保存').click();await flush(win);
    expect(win.entries[0]).toEqual({id:'a',url:'http://localhost:5175/',label:'a'});
  })
  it.each(['preview','Editor preview'])('shows the saved label in one row without a separate name (%s)',async label=>{
    const win=await fixture(false,false,false,[],[{id:'preview',url:'http://localhost:5173/',label}]);
    const entry=win.document.querySelector('.vp-local-url-entry')!;
    const row=entry.querySelector('.vp-local-url-row')!;
    expect(row).not.toBeNull();
    expect(row.querySelector('.vp-link-dot')).not.toBeNull();
    expect(row.querySelector('.vp-local-url-open')!.textContent).toBe(label);
    expect(row.querySelector('.vp-local-url-address')!.getAttribute('title')).toBe('http://localhost:5173/');
    expect(entry.children).toHaveLength(1);
    expect(entry.querySelector('.vp-local-url-name')).toBeNull();
    const actions=row.querySelector('.vp-local-url-actions')!;
    expect([...actions.querySelectorAll('button')].map(b=>b.textContent)).toEqual(['編集','削除']);
    button(win,'編集').focus();expect(win.document.activeElement).toBe(button(win,'編集'));
  })
  it('automatically probes registrations on visibility, changes and save, but not unchanged refreshes',async()=>{
    const win=await fixture(false,false,true);
    expect(win.sent).toHaveLength(0);
    win.setActive(true);await flush(win);
    const probes=()=>win.sent.filter(r=>r.payload?.action==='probe');
    expect(probes()).toHaveLength(1);
    win.refreshUrls();await flush(win);expect(probes()).toHaveLength(1);
    win.entries=[{id:'a',url:'http://localhost:5174/',label:'Changed'}];
    win.refreshUrls();await flush(win);expect(probes()).toHaveLength(2);
    button(win,'編集').click();input(win,'URL','http://localhost:5175/');button(win,'保存').click();await flush(win);
    expect(probes()).toHaveLength(3);
    win.setActive(false);win.refreshUrls();await flush(win);expect(probes()).toHaveLength(3);
    win.setActive(true);await flush(win);expect(probes()).toHaveLength(4);
  })
  it('shares one probe queue and discards a stale registration result before probing the new content',async()=>{
    const win=await fixture(false,false,true,['http://localhost:5173/']);
    win.deferProbes=true;win.setActive(true);await new Promise(resolve=>setImmediate(resolve));
    expect(win.pendingProbes.map(r=>r.payload)).toEqual([{action:'probe',id:'a'}]);
    win.entries=[{id:'a',url:'http://localhost:9000/',label:'Changed'}];
    win.refreshUrls();await new Promise(resolve=>setImmediate(resolve));
    expect(win.pendingProbes).toHaveLength(1);
    win.replyProbe(win.pendingProbes.shift(),{state:'responding',status:201});await new Promise(resolve=>setImmediate(resolve));
    expect(win.document.querySelector('.vp-local-url-entry .vp-link-dot')!.getAttribute('data-state')).toBe('unknown');
    expect(win.pendingProbes.map(r=>r.payload)).toEqual([{action:'probe',id:'a'}]);
    win.replyProbe(win.pendingProbes.shift(),{state:'responding',status:200});await new Promise(resolve=>setImmediate(resolve));
    expect(win.document.querySelector('.vp-local-url-entry .vp-link-dot')!.getAttribute('title')).toBe('応答あり · HTTP 200');
    expect(win.pendingProbes.map(r=>r.payload)).toEqual([{action:'probe_url',url:'http://localhost:5173/'}]);
    win.setActive(false);win.replyProbe(win.pendingProbes.shift(),{state:'refused'});await flush(win);
    expect(win.pendingProbes).toHaveLength(0);
  })
  it('discards old lane probes and stops queued work when hidden',async()=>{
    const win=await fixture(false,false,true,['http://localhost:5173/']);
    win.deferProbes=true;win.setActive(true);await new Promise(resolve=>setImmediate(resolve));
    expect(win.pendingProbes).toHaveLength(1);
    const old=win.pendingProbes.shift();
    win.setAddress('vp/second');await new Promise(resolve=>setImmediate(resolve));
    win.replyProbe(old,{state:'responding',status:201});await new Promise(resolve=>setImmediate(resolve));
    expect(win.document.querySelector('.vp-local-url-entry .vp-link-dot')!.getAttribute('data-state')).toBe('unknown');
    expect(win.pendingProbes).toHaveLength(1);
    expect(win.pendingProbes[0].address).toBe('vp/second');
    win.setActive(false);win.replyProbe(win.pendingProbes.shift(),{state:'refused'});await flush(win);
    expect(win.sent.filter(r=>r.payload?.action==='probe_url')).toHaveLength(0);
  })
  it('automatically probes a valid registration named constructor',async()=>{
    const win=await fixture(false,false,false,[],[{id:'constructor',label:'Preview',url:'http://localhost:5173/'}]);
    expect(win.sent.filter(r=>r.payload?.action==='probe').map(r=>r.payload.id)).toEqual(['constructor']);
    expect(win.document.querySelector('.vp-local-url-entry .vp-link-dot')!.getAttribute('data-state')).toBe('refused');
  })
  it('does not continue the shared probe queue after unmount',async()=>{
    const win=await fixture(false,false,true,['http://localhost:5173/']);
    win.deferProbes=true;win.setActive(true);await new Promise(resolve=>setImmediate(resolve));
    expect(win.pendingProbes).toHaveLength(1);
    win.dispose();win.replyProbe(win.pendingProbes.shift(),{state:'refused'});await flush(win);
    expect(win.pendingProbes).toHaveLength(0);
    expect(win.sent.filter(r=>r.payload?.action==='probe_url')).toHaveLength(0);
  })
  it.each([0,2,12])('opens the form before the lists with %s discovered URLs',async count=>{
    const urls=Array.from({length:count},(_,i)=>`http://localhost:${5100+i}/`);
    const win=await fixture(true,false,false,urls);
    if(count) button(win,'登録').click(); else button(win,'URLを追加').click();
    await flush(win);
    const form=win.document.querySelector('.vp-local-url-form');
    expect(form).not.toBeNull();
    expect(win.document.querySelector<HTMLInputElement>('[aria-label="URL"]')!.value).toBe(urls[0]??'');
    expect(win.document.querySelector('.vp-local-url-body')!.firstElementChild).toBe(form);
  })
  it('focuses the registration name and scrolls the form into view on each registration',async()=>{
    const win=await fixture(true,false,false,['http://localhost:5173/','http://localhost:5174/']);
    button(win,'登録').click();
    const form=win.document.querySelector('.vp-local-url-form')!;
    const scroll=vi.spyOn(form,'scrollIntoView');
    await flush(win);
    const name=win.document.querySelector<HTMLInputElement>('[aria-label="名前"]')!;
    expect(win.document.activeElement).toBe(name);
    expect(scroll).toHaveBeenCalledWith({block:'nearest'});
    button(win,'登録').click();await flush(win);
    expect(win.document.activeElement).toBe(name);
    expect(scroll).toHaveBeenCalledTimes(2);
  })
  it.each([5,6,12])('shows up to five of %s candidates and resets expansion on lane change',async count=>{
    const urls=Array.from({length:count},(_,i)=>`http://localhost:${5100+i}/`);
    const win=await fixture(true,false,false,urls);
    expect([...win.document.querySelectorAll('.vp-discovered-url .vp-local-url-open')].map(el=>el.textContent)).toEqual(urls.slice(0,5));
    if(count===5) {
      expect(win.document.body.textContent).not.toContain('さらに');
      return;
    }
    const expand=button(win,`さらに ${count-5} 件`);
    expect(expand.getAttribute('aria-expanded')).toBe('false');
    expand.click();await flush(win);
    expect(win.document.querySelectorAll('.vp-discovered-url')).toHaveLength(count);
    expect(button(win,'折りたたむ').getAttribute('aria-expanded')).toBe('true');
    win.setAddress('vp/second');await flush(win);
    expect(win.document.querySelectorAll('.vp-discovered-url')).toHaveLength(5);
    expect(button(win,`さらに ${count-5} 件`).getAttribute('aria-expanded')).toBe('false');
  })
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
  it('shows saved entries, shared PR and discovered URL in order; saves only on registration',async()=>{
    const win=await fixture();
    win.setDiscovered(['http://localhost:5173/']);
    win.setPr({number:42,title:'Lane links',url:'https://github.com/org/repo/pull/42'});
    await flush(win);
    expect(win.entries).toHaveLength(1);
    expect(win.sent.some(r=>r.payload?.action==='save')).toBe(false);
    const text=win.document.body.textContent!;
    expect(text.indexOf('Editor preview')).toBeLessThan(text.indexOf('#42'));
    expect(text.indexOf('#42')).toBeLessThan(text.indexOf('http://localhost:5173/'));
    button(win,'登録').click();input(win,'名前','dev');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(2);
    expect(win.entries[1]).toMatchObject({id:'dev',url:'http://localhost:5173/',label:'dev'});
    expect(win.sent.filter(r=>r.t==='lane:prs_fetch')).toHaveLength(0);
  })
  it('reflects external named registrations but preserves a draft and rejects stale saves',async()=>{
    const win=await fixture(true);
    win.entries=[{id:'preview',url:'http://localhost:5173',label:'From CLI'}];
    win.refreshUrls();await flush(win);
    expect(win.document.body.textContent).toContain('From CLI');
    button(win,'編集').click();input(win,'URL','http://localhost:5175/draft');
    win.entries=[{id:'preview',url:'http://localhost:5174',label:'From MCP'}];
    win.refreshUrls();await flush(win);
    expect(win.document.querySelector<HTMLInputElement>('[aria-label="URL"]')!.value).toBe('http://localhost:5175/draft');
    button(win,'保存').click();await flush(win);
    expect(win.document.body.textContent).toContain('別の画面で更新');
    expect(win.entries[0].label).toBe('From MCP');
    button(win,'キャンセル').click();button(win,'再読み込み').click();await flush(win);
    expect(win.document.body.textContent).toContain('From MCP');
    win.entries=[];win.refreshUrls();await flush(win);
    expect(win.document.querySelector('.vp-local-url-entry')).toBeNull();
    expect(win.sent.filter(r=>r.payload.action==='probe')).toHaveLength(2);
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
    win.addUrl();input(win,'名前','preview');input(win,'URL','http://localhost:12889');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(1);expect(win.document.body.textContent).toContain('preview');
    button(win,'削除').click();await flush(win);
    expect(win.entries).toHaveLength(0);expect(button(win,'URLを追加').disabled).toBe(false);
    expect(win.sent.every(r=>r.t==='local_urls:request')).toBe(true);
  })

  it('loads the lane registrations, opens the exact id and distinguishes refused from untested',async()=>{
    const win=await fixture();expect(win.document.body.textContent).toContain('Editor preview');
    const dot=win.document.querySelector('.vp-local-url-entry .vp-link-dot')!;
    expect(dot.getAttribute('data-state')).toBe('refused');
    expect(dot.getAttribute('aria-label')).toBe('接続拒否（停止の可能性）');
    button(win,'Editor preview').click();await flush(win);
    expect(win.sent.at(-1)).toMatchObject({t:'local_urls:request',path:'/repo',address:'vp/lane/demo',payload:{action:'open',id:'a'}});
    expect([...win.document.querySelectorAll('button')].some(b=>b.textContent==='確認')).toBe(false);
    expect(win.document.querySelector('.vp-local-url-state')).toBeNull();
  })
  it('adds edits and removes multiple registrations without selecting the lane',async()=>{
    const win=await fixture();button(win,'URLを追加').click();input(win,'名前','api');input(win,'URL','http://127.0.0.1:12890');button(win,'保存').click();await flush(win);
    expect(win.entries).toHaveLength(2);expect(win.document.body.textContent).toContain('api');
    button(win,'編集').click();input(win,'URL','http://localhost:12891/');button(win,'保存').click();await flush(win);expect(win.entries[0]).toEqual({id:'a',url:'http://localhost:12891/',label:'a'});
    button(win,'削除').click();await flush(win);expect(win.entries).toHaveLength(1);expect(win.entries[0].label).toBe('api');
    expect(win.sent.every(r=>r.t==='local_urls:request')).toBe(true)
  })
  it('keeps the form and persisted entries when save fails',async()=>{
    const win=await fixture();win.failSave=true;button(win,'編集').click();input(win,'URL','http://localhost:5175/unsaved');button(win,'保存').click();await flush(win);
    expect(win.document.body.textContent).toContain('保存できません');expect(win.entries[0].label).toBe('Editor preview');expect(win.document.querySelector<HTMLInputElement>('[aria-label="URL"]')!.value).toBe('http://localhost:5175/unsaved');
  })
})
