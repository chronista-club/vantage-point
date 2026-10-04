import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type ErrorEvent } from 'happy-dom'

const setup = `
import {installLanePanes, laneScope, boardKeyOf} from './lane-panes'
import {layoutEngine} from './layout-host'
import {installCodeView} from './code-view'
import {openDispatch, installDispatch} from './dispatch'
openDispatch(); installDispatch({})
import {installBoardView} from './board-view'
const container=document.createElement('div'); document.body.append(container)
window.sent=[]; window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}
window.api=installLanePanes({container,hostOf:id=>document.getElementById(id),mountChat:()=>()=>{},mountTermPlate:()=>()=>{}})
window.code=installCodeView()
const boardHost=document.createElement('div'); boardHost.id='lane-board'; container.append(boardHost)
const boardController=installBoardView({board:boardHost,workbench:container,handle:document.createElement('button'),formBtn:document.createElement('button')})
window.board={...boardController,setActiveLane:lane=>boardController.setActiveLane(boardKeyOf(lane))}
window.roster=(lane,keys=[1,2])=>document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane,sessions:keys.map(key=>({key,agent:'codex',mode:'gui'}))}}))
window.current=lane=>layoutEngine.current(laneScope(lane))
window.seed=(lane,layout)=>layoutEngine.update(laneScope(lane),()=>layout)
`
async function boot(saved?: unknown) {
 const r=await build({stdin:{contents:setup,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()]})
 const w=new Window(); w.addEventListener('error', e => { throw (e as ErrorEvent).error }); w.eval(r.outputFiles[0].text)
 if (saved !== false) (w as any).vpDispatch({ t: 'pane:stow_restore', payload: saved ?? {} })
 return w
}
it('再起動と遅い roster 到着をまたいで、lane ごとのしまった状態と元の配分を復元する',async()=>{
 const a=await boot()
 let b: Window | undefined
 try {
  a.eval(`roster('a/root'); api.setActiveLane('a/root'); seed('a/root',{structure:{columns:[{panes:['chat-session-2']},{panes:['chat-session-1']}]},attention:{'chat-session-1':3,'chat-session-2':7}}); api.stowPane('chat-session-2')`)
  const sent=(a as any).sent.filter((m:any)=>m.t==='pane:stow')
  expect(sent.length).toBeGreaterThan(0)
  b=await boot({[sent.at(-1).lane]:sent.at(-1).state})
  b.eval(`api.setActiveLane('a/root'); api.focusPane('chat-session-2',false); roster('a/root')`)
  expect((b as any).current('a/root').attention).toEqual({'chat-session-1':3,'chat-session-2':0})
  b.eval(`roster('b/root'); api.setActiveLane('b/root')`)
  expect((b as any).current('b/root').attention['chat-session-2']).toBeGreaterThan(0)
  b.eval(`api.setActiveLane('a/root'); api.focusPane('chat-session-2',false); api.unstowPane('chat-session-2')`)
  expect((b as any).current('a/root')).toEqual({structure:{columns:[{panes:['chat-session-2']},{panes:['chat-session-1']}]},attention:{'chat-session-1':3,'chat-session-2':7}})
  expect((b as any).sent.at(-1).state.shares).toEqual({})
 } finally {await a.happyDOM.close(); await b?.happyDOM.close()}
},20000)

it('しまった session の mode 切替と削除を保存に反映する',async()=>{
 const w=await boot()
 try {
  w.eval(`roster('a/root'); api.setActiveLane('a/root'); api.stowPane('chat-session-2'); document.dispatchEvent(new CustomEvent('vp:session-mode',{detail:{lane:'a/root',session:2,mode:'tui'}}))`)
  expect((w as any).sent.at(-1).state.shares).toEqual({'term-session-2':1})
  w.eval(`roster('a/root',[1])`)
  expect((w as any).sent.at(-1).state.shares).toEqual({})
 } finally {await w.happyDOM.close()}
},20000)

it('しまった Board / Code は所有者ごと復元され、lane を往復しても rail に残る',async()=>{
 const a=await boot(); let b: Window | undefined
 try {
  a.eval(`roster('a/root'); api.setActiveLane('a/root'); code.openFor('a/root'); board.setActiveLane('a/root'); board.toggleOpen(); board.toggleForm(); api.stowPane('lane-code'); api.stowPane('lane-board')`)
  const saved=(a as any).sent.at(-1)
  b=await boot({[saved.lane]:saved.state})
  b.eval(`roster('a/root'); api.setActiveLane('a/root'); code.setActiveLane('a/root'); board.setActiveLane('a/root')`)
  expect((b as any).current('a/root').attention['lane-board']).toBe(0)
  expect((b as any).current('a/root').attention['lane-code']).toBe(0)
  b.eval(`api.setActiveLane('b/root'); code.setActiveLane('b/root'); board.setActiveLane('b/root'); api.setActiveLane('a/root'); code.setActiveLane('a/root'); board.setActiveLane('a/root')`)
  expect((b as any).current('a/root').attention['lane-code']).toBe(0)
  b.eval(`code.toggle()`)
  expect((b as any).sent.at(-1).state.shares['lane-code']).toBeUndefined()
 } finally {await a.happyDOM.close(); await b?.happyDOM.close()}
},20000)


it('復元通知より先に roster が届いても保存を上書きせず、後着の復元を適用する', async () => {
 const w=await boot(false)
 try {
  w.eval(`roster('a/root'); api.setActiveLane('a/root'); roster('a/root')`)
  expect((w as any).sent).toEqual([])
  ;(w as any).vpDispatch({ t:'pane:stow_restore', payload:{'a/root': {version:1,layout:{structure:{columns:[{panes:['chat-session-1']},{panes:['chat-session-2']}]},attention:{'chat-session-1':2,'chat-session-2':0}},shares:{'chat-session-2':5}}}})
  w.eval(`api.focusPane('chat-session-2',false)`)
  expect((w as any).current('a/root').attention['chat-session-2']).toBe(0)
  expect((w as any).sent).toEqual([])
 } finally {await w.happyDOM.close()}
},20000)
