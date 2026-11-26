// mem_1CeySwxuoVc17bGLnU5Np3 — 実行中 Enter は今伝える、Queue は明示操作。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLButtonElement } from 'happy-dom'

it('Codex Enter steers the displayed turn and the separate button queues the next input', async () => {
  const result = await build({ stdin: { contents: `
    import {installChatView} from './chatview'
    window.sent=[]
    window.ipc={postMessage: m=>window.sent.push(JSON.parse(m))}
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('queue-test/lead')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'queue-test/lead',focused:1,sessions:[{key:1,agent:'codex',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    const mount=document.createElement('div');document.body.append(mount)
    api.mountSession(mount,'queue-test/lead',1)
    window.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:'turn',ready:true,items:[],error:null},request_id:null,error:null},1)
    window.emit({kind:'message_chunk',text:'working'},1)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife', conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const window = new Window()
  try {
    evalDom(window, result.outputFiles[0].text)
    const input = window.document.querySelector('textarea')!
    input.value = '今の補足'
    dispatchDom(input, new window.Event('input', { bubbles: true }))
    dispatchDom(input, new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    const sent = (window as unknown as { sent: Array<Record<string, any>>; emit: (event: unknown, session: number) => void })
    const steer = sent.sent.find(m => m.t === 'conversation:codex_input')
    expect(steer).toMatchObject({ lane: 'queue-test/lead', session: 1, thread_id: 'thread', action: { kind: 'steer', turn_id: 'turn', text: '今の補足' } })
    callDom(window, () => sent.emit({ kind: 'codex_queue', queue: null, request_id: steer!.request_id, error: null }, 1))
    // Native turn continues while a question pauses the visual typing indicator.
    callDom(window, () => sent.emit({kind:'question',request_id:'question',questions:[]},1))
    expect(window.document.querySelector('.conversation-send')!.textContent).toContain('今伝える')
    input.value = '次の仕事'
    dispatchDom(input, new window.Event('input', { bubbles: true }))
    const queueButton = [...window.document.querySelectorAll<HTMLButtonElement>('button')].find(b => b.textContent?.includes('次に実行する'))
    expect(queueButton).toBeDefined()
    clickDom(queueButton!)
    expect(sent.sent.filter(m => m.t === 'conversation:codex_input').at(-1)).toMatchObject({ action: { kind: 'add', text: '次の仕事' } })
    const added = sent.sent.filter(m => m.t === 'conversation:codex_input').at(-1)!
    callDom(window, () => sent.emit({kind:'codex_queue',queue:null,request_id:added.request_id,error:null},1))
    callDom(window, () => sent.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:null,ready:true,items:[
      {id:'q1',client_id:'c1',text:'FIRST',editable:true},
      {id:'q2',client_id:'c2',text:'SECOND',editable:true}],error:null},request_id:null,error:null},1))
    callDom(window, () => sent.emit({kind:'turn_completed',session_id:'thread'},1))
    callDom(window, () => sent.emit({kind:'codex_config',config:{models:[{model:'test',label:'Test',efforts:['high'],default_effort:'high'}],model:'test',effort:'high',selection:null,error:null},request_id:null,error:null},1))
    expect(window.document.querySelector<HTMLButtonElement>('[aria-label="Codex model"]')!.disabled).toBe(true)
    const row = window.document.querySelector('[data-queued-id="q1"]')!
    expect(row).not.toBeNull()
    const edit = row.querySelector<HTMLButtonElement>('[data-queue-edit]')!
    clickDom(edit)
    const queuedText = row.querySelector('textarea')!
    queuedText.value='EDITED'
    dispatchDom(queuedText, new window.Event('input',{bubbles:true}))
    clickDom(row.querySelector<HTMLButtonElement>('[data-queue-save]')!)
    expect(sent.sent.at(-1)).toMatchObject({action:{kind:'update',id:'q1',text:'EDITED'}})
    callDom(window, () => sent.emit({kind:'codex_queue',queue:null,request_id:sent.sent.at(-1)!.request_id,error:null},1))
    clickDom(row.querySelector<HTMLButtonElement>('[data-queue-down]')!)
    expect(sent.sent.at(-1)).toMatchObject({action:{kind:'reorder',ids:['q2','q1']}})
    callDom(window, () => sent.emit({kind:'codex_queue',queue:null,request_id:sent.sent.at(-1)!.request_id,error:null},1))
    clickDom(row.querySelector<HTMLButtonElement>('[data-queue-delete]')!)
    expect(sent.sent.at(-1)).toMatchObject({action:{kind:'delete',id:'q1'}})
    callDom(window, () => sent.emit({kind:'codex_queue',queue:null,request_id:sent.sent.at(-1)!.request_id,error:null},1))
    clickDom(window.document.querySelector<HTMLButtonElement>('[data-queue-start]')!)
    expect(sent.sent.at(-1)).toMatchObject({action:{kind:'start',id:'q1'}})
    callDom(window, () => sent.emit({kind:'codex_queue',queue:null,request_id:sent.sent.at(-1)!.request_id,error:null},1))
    callDom(window, () => sent.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:null,ready:false,items:[],error:'一覧を取得できませんでした'},request_id:null,error:null},1))
    const refresh = window.document.querySelector<HTMLButtonElement>('[data-queue-refresh]')
    expect(refresh).not.toBeNull()
    clickDom(refresh!)
    expect(sent.sent.at(-1)).toMatchObject({action:{kind:'refresh'},thread_id:'thread',session:1})
    callDom(window, () => sent.emit({kind:'codex_queue',queue:null,request_id:sent.sent.at(-1)!.request_id,error:null},1))
    callDom(window, () => sent.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:'lost-turn',ready:true,items:[],error:null},request_id:null,error:null},1))
    callDom(window, () => sent.emit({kind:'engine_exited',message:'disconnected'},1))
    input.value = '再開する'
    dispatchDom(input, new window.Event('input', { bubbles: true }))
    dispatchDom(input, new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    expect(sent.sent.at(-1)).toMatchObject({t:'conversation:submit',session:1,prompt:'再開する'})
  } finally { await window.happyDOM.close() }
}, 20000)
