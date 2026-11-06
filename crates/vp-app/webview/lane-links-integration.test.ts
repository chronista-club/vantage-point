// mem_1CgBHtLEoHBUibQLBU3vbs / mem_1Cg4Mh2158eTTSiPPymwWp — 実物の 2 bundle と同じ境界で要求数 / lane 追従を確かめる。
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLElement } from 'happy-dom'
import { fileURLToPath } from 'node:url'

let main: string, sidebar: string
const windows: Window[] = []
beforeAll(async () => {
  async function bundle(contents: string) {
    const result = await build({ stdin: { contents, loader: 'tsx', resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle: true, write: false, format: 'iife', platform: 'browser', preserveSymlinks: true, plugins: [solidPlugin()], define: { 'process.env.NODE_ENV': '"production"' } })
    return result.outputFiles[0].text
  }
  main = await bundle(`
    import {installRightSidebar} from './right-sidebar';
    import {discoveredLaneUrls} from './lane-link-discovery';
    window.discover=(lane,text)=>discoveredLaneUrls.feed(lane,'pty:1',text,true);
    window.rsb=installRightSidebar({log:document.querySelector('#log'),tabs:[],sections:[...document.querySelectorAll('.rsb-section')],laneBody:document.querySelector('#lane'),residents:[],prsBody:document.querySelector('#prs'),prsCount:null,prsRefresh:null});
    window.rsb.setLane('vp/one');
  `)
  sidebar = await bundle(`
    import {render} from 'solid-js/web';
    import {createComponent} from 'solid-js';
    import {LaneRow} from './src/sidebar/LaneRow';
    import {installLaneLinks} from './src/sidebar/lane-links';
    import {installIpcBridge} from './src/sidebar/ipc';
    import {openSidebarDispatch} from './src/sidebar/dispatch';
    import {applySidebarState,emptyState} from './src/sidebar/store';
    const lanes=['one','two','three','four'].map(name=>({address:{repo:'vp',name,key:'vp/'+name},agent:'codex',cwd:'/repo/.vp/lanes/'+name,pid:1}));
    openSidebarDispatch();installIpcBridge();
    applySidebarState({...emptyState(),lanes_by_repo:{'/repo':lanes},active_lane_address:'vp/one'});
    render(()=>lanes.map(lane=>createComponent(LaneRow,{lane,repoPath:'/repo'})),document.querySelector('#left'));
    installLaneLinks();
  `)
})

async function fixture(sidebarFirst = false) {
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as Window & { sent: any[]; rsb: any; discover(lane:string,text:string):void }
  windows.push(win)
  win.document.body.innerHTML = `<div id="left"></div><section class="rsb-section" data-section="lane"><button class="rsb-sec-head">Lane</button><div id="lane"></div></section><section class="rsb-section" data-section="prs"><button class="rsb-sec-head">PR</button><div id="prs"></div></section><section class="rsb-section" data-section="log"><button class="rsb-sec-head">Log</button><div id="log"></div></section>`
  win.eval(`window.sent=[];window.setInterval=()=>1;window.clearInterval=()=>{};window.ipc={postMessage:m=>{
    const request=JSON.parse(m);window.sent.push(request);
    if(request.t==='local_urls:request')queueMicrotask(()=>window.vpSidebarDispatch({t:'local_urls:result',req:request.req,payload:request.payload.action==='probe_url'?{probe:{state:'responding',status:200}}:{entries:[]}}));
  }};`)
  win.eval(sidebarFirst ? sidebar : main)
  win.eval(sidebarFirst ? main : sidebar)
  await win.happyDOM.waitUntilComplete()
  return win
}
function toggle(win: Window) { win.document.dispatchEvent(new win.CustomEvent('vp:right-sidebar-toggle')) }
function section(win: Window, name: string) { win.document.querySelector<HTMLElement>(`[data-section="${name}"] .rsb-sec-head`)!.click() }
afterEach(async () => { for (const win of windows.splice(0)) await win.happyDOM.abort() })

describe('Lane リンクの表示購読', () => {
  it.each([false,true])('4 lane 行からは取得せず、R を開いた時だけ選択 lane を要求する（sidebar first=%s）', async sidebarFirst => {
    const win = await fixture(sidebarFirst)
    expect(win.sent).toEqual([])
    toggle(win);await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.t==='local_urls:request')).toMatchObject([{address:'vp/one',path:'/repo',payload:{action:'load'}}])
    expect(win.document.querySelector('#left .vp-local-urls')).toBeNull()
    expect(win.document.querySelector('#lane .vp-local-urls')).not.toBeNull()
    win.rsb.setLane('vp/two');await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.t==='local_urls:request').map(r=>r.address)).toEqual(['vp/one','vp/two'])
    section(win,'lane');win.rsb.setLane('vp/three');await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.t==='local_urls:request')).toHaveLength(2)
    section(win,'lane');await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.t==='local_urls:request').at(-1).address).toBe('vp/three')
  })

  it('登録済みも表示中 lane だけ自動確認し、節を再表示すると再確認する', async () => {
    const win = await fixture()
    win.eval(`const original=window.ipc.postMessage;window.ipc.postMessage=m=>{
      const request=JSON.parse(m);
      if(request.t==='local_urls:request'&&['load','probe'].includes(request.payload.action)){
        window.sent.push(request);
        queueMicrotask(()=>window.vpSidebarDispatch({t:'local_urls:result',req:request.req,payload:request.payload.action==='probe'?{probe:{state:'responding',status:200}}:{entries:[{id:'preview',label:'preview',url:'http://localhost:5173/'}]}}));
      }else original(m);
    };`)
    expect(win.sent).toEqual([])
    toggle(win);await win.happyDOM.waitUntilComplete()
    const probes=()=>win.sent.filter(r=>r.payload?.action==='probe')
    expect(probes()).toMatchObject([{address:'vp/one',payload:{id:'preview'}}])
    expect(win.document.querySelector('#lane .vp-link-dot')!.getAttribute('title')).toBe('応答あり · HTTP 200')
    section(win,'lane');win.rsb.setLane('vp/two');await win.happyDOM.waitUntilComplete()
    expect(probes()).toHaveLength(1)
    section(win,'lane');await win.happyDOM.waitUntilComplete()
    expect(probes().map(r=>r.address)).toEqual(['vp/one','vp/two'])
  })

  it('PR poller を両節で共用し、非表示の検出 URL は probe しない', async () => {
    const win = await fixture()
    win.discover('vp/one','http://localhost:5173')
    win.discover('vp/two','http://localhost:5174')
    expect(win.sent).toEqual([])
    section(win,'prs');toggle(win);await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.t==='lane:prs_fetch')).toMatchObject([{lane:'vp/one'}])
    expect(win.sent.filter(r=>r.payload?.action==='probe_url').map(r=>r.payload.url)).toEqual(['http://localhost:5173/'])
    win.rsb.handlePrs('vp/one',{available:true,branch:'wip/one',prs:[{number:42,title:'Lane links',url:'https://github.com/org/repo/pull/42',head:'review/one'}]})
    expect(win.document.querySelector('#lane')!.textContent).toContain('#42')
    section(win,'prs');await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.t==='lane:prs_fetch')).toHaveLength(1)
    section(win,'lane');win.discover('vp/one','http://localhost:6000');await win.happyDOM.waitUntilComplete()
    expect(win.sent.filter(r=>r.payload?.action==='probe_url')).toHaveLength(1)
  })
})
