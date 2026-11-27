// Pane のしまうモード（creo mem_1CfbF4m1sGusje8oMouTu8）: 名札の「しまう」→ rail の縦中央 → click で戻す。
// 裁定（mako 2026-10-01）: しまう動詞は名札（pane 級）/ 戻すは rail のアイコン（しまった一覧 = lane 級）。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLButtonElement } from 'happy-dom'

async function bundle(contents: string) {
  const result = await build({ stdin: { contents, resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const window = new Window()
  evalDom(window, result.outputFiles[0].text)
  return window
}

it('名札の「しまう」ボタンは、その pane の host id を vp:pane-stow で知らせる（送信や閉じるはしない）', async () => {
  const window = await bundle(`
    import {installChatView} from './chatview'
    window.sent=[]
    window.ipc={postMessage: m=>window.sent.push(JSON.parse(m))}
    window.stowed=[]
    document.addEventListener('vp:pane-stow', e=>window.stowed.push(e.detail))
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('stow-test/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'stow-test/main',focused:1,sessions:[{key:1,agent:'claude',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    const mount=document.createElement('div');document.body.append(mount)
    api.mountSession(mount,'stow-test/main',1)
  `)
  try {
    const btn = window.document.querySelector<HTMLButtonElement>('button.conversation-session-plate-stow')!
    expect(btn).not.toBeNull()
    clickDom(btn)
    const h = window as unknown as { stowed: unknown[]; sent: Array<{ t: string }> }
    expect(h.stowed).toEqual([{ lane: 'stow-test/main', id: 'chat-session-1' }])
    expect(h.sent.some(m => m.t === 'conversation:session_remove')).toBe(false)
  } finally {
    await window.happyDOM.close()
  }
}, 20000)

it('rail の縦中央に、しまった pane のアイコンが並び、click で vp:pane-unstow を知らせる', async () => {
  const window = await bundle(`
    import {mountEdgeRail} from './EdgeRail'
    const root=document.createElement('div'); root.id='edge-rail'
    const newHost=document.createElement('div'); newHost.id='edge-rail-new-host'
    const stowHost=document.createElement('div'); stowHost.id='edge-rail-stow-host'
    root.append(newHost, stowHost); document.body.append(root)
    // 住人の host は表（edge-residents.ts）の id で引く。PR の host はこの台に無い（描画を飛ばす）
    window.rail=mountEdgeRail(root,(id)=>document.getElementById(id))
    window.rail.setLane('stow-test/main')
    window.unstowed=[]
    document.addEventListener('vp:pane-unstow', e=>window.unstowed.push(e.detail))
  `)
  try {
    const doc = window.document
    expect(doc.querySelectorAll('.rail-stowed').length).toBe(0)
    dispatchDom(doc, new window.CustomEvent('vp:stowed-panes', { detail: { lane: 'stow-test/main', panes: [
      { id: 'chat-session-1', kind: 'chat', label: 'claude#1', session: 1, agent: 'claude' },
      { id: 'lane-code', kind: 'code', label: 'Code' },
    ] } }))
    const items = doc.querySelectorAll<HTMLButtonElement>('.rail-stowed')
    expect(items.length).toBe(2)
    expect(items[0]!.getAttribute('data-label')).toContain('claude#1')
    clickDom(items[0]!)
    expect((window as unknown as { unstowed: unknown[] }).unstowed).toEqual([{ lane: 'stow-test/main', id: 'chat-session-1' }])
    // 別 lane の一覧は無視する（rail は表示 lane のもの）
    dispatchDom(doc, new window.CustomEvent('vp:stowed-panes', { detail: { lane: 'other/main', panes: [] } }))
    expect(doc.querySelectorAll('.rail-stowed').length).toBe(2)
    // lane 切替: rail は自分から一覧を要求する（lane-panes が先に流していても落とさない）。
    // 2026-10-02 実機: 切り替えて戻ると一覧が消え、しまった pane を戻せなくなった（順序依存）
    const h2 = window as unknown as { demanded: unknown[]; rail: { setLane(a: string | null): void } }
    h2.demanded = []
    doc.addEventListener('vp:stowed-panes-demand', e => h2.demanded.push((e as unknown as CustomEvent).detail))
    callDom(window, () => h2.rail.setLane('other/main'))
    expect(h2.demanded).toEqual([{ lane: 'other/main' }])
    expect(doc.querySelectorAll('.rail-stowed').length).toBe(0)
    dispatchDom(doc, new window.CustomEvent('vp:stowed-panes', { detail: { lane: 'other/main', panes: [
      { id: 'term-session-1', kind: 'term', label: 'cc#1', session: 1, agent: 'claude' },
    ] } }))
    expect(doc.querySelectorAll('.rail-stowed').length).toBe(1)
    callDom(window, () => h2.rail.setLane('stow-test/main'))
    dispatchDom(doc, new window.CustomEvent('vp:stowed-panes', { detail: { lane: 'stow-test/main', panes: [
      { id: 'chat-session-1', kind: 'chat', label: 'claude#1', session: 1, agent: 'claude' },
      { id: 'lane-code', kind: 'code', label: 'Code' },
    ] } }))
    expect(doc.querySelectorAll('.rail-stowed').length).toBe(2)
    // badge: しまった pane に動きがあれば点く
    dispatchDom(doc, new window.CustomEvent('vp:pane-activity', { detail: { lane: 'stow-test/main', id: 'lane-code' } }))
    expect(doc.querySelectorAll('.rail-stowed')[1]!.classList.contains('has-badge')).toBe(true)
    expect(doc.querySelectorAll('.rail-stowed')[0]!.classList.contains('has-badge')).toBe(false)
  } finally {
    await window.happyDOM.close()
  }
}, 20000)

it('chat に動きがあると vp:pane-activity（lane + host id）が流れる — rail の badge の供給元', async () => {
  const window = await bundle(`
    import {installChatView} from './chatview'
    window.ipc={postMessage: ()=>{}}
    window.activity=[]
    document.addEventListener('vp:pane-activity', e=>window.activity.push(e.detail))
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('act-test/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'act-test/main',focused:1,sessions:[{key:1,agent:'claude',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    const mount=document.createElement('div');document.body.append(mount)
    api.mountSession(mount,'act-test/main',1)
    window.emit({ kind: 'message_chunk', text: 'working' }, 1)
  `)
  try {
    const h = window as unknown as { activity: Array<{ lane: string; id: string }> }
    expect(h.activity.length).toBeGreaterThan(0)
    expect(h.activity[0]).toEqual({ lane: 'act-test/main', id: 'chat-session-1' })
  } finally {
    await window.happyDOM.close()
  }
}, 20000)
