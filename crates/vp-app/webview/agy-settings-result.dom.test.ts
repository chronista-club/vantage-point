// Task mem_1ChKFNBY7VQYSwFseyxUs9 — 共通 settings-result の追加が agy の完了判定を変えない。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLSelectElement } from 'happy-dom'

it('agy は共通 ACK で解除せず、従来どおり自身の roster 更新で完了・失敗を描く', async () => {
  const result = await build({ stdin: { contents: `
    import { createSignal } from 'solid-js'
    import { render } from '@solidjs/web'
    import { AgySettingsPanel } from './agy-settings-panel'
    import { installConsole } from './console'
    const [view,setView] = createSignal({options:[
      {id:'model',name:'Model',current_value:'model-a',choices:[{value:'model-a',name:'A'},{value:'model-b',name:'B'}]}
    ]})
    window.sent=[]; window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}
    window.current=()=>JSON.parse(JSON.stringify(view())); window.update=setView
    window.results=[]; window.addEventListener('vp:conversation-settings-result',e=>window.results.push(e.detail))
    const api=installConsole()
    window.ack=()=>api.handleEvent('test/lead',{kind:'codex_config',config:null,request_id:'grok-request',error:null},2)
    render(()=>AgySettingsPanel({lane:'test/lead',session:2,lc:{state:{streaming:false}},
      rosterEntry:()=>({agent:'agy',acp_config:view()})}),document.body)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife',
    conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const win = new Window()
  try {
    evalDom(win, result.outputFiles[0].text)
    const app = win as unknown as {sent: any[]; results: any[]; ack: () => void; current: () => any; update: (v: any) => void}
    const select = () => win.document.querySelector<HTMLSelectElement>('[aria-label="agy Model"]')!
    const change = (value: string) => {
      select().value = value
      dispatchDom(select(), new win.Event('change', {bubbles:true}))
    }
    change('model-b')
    expect(app.sent.at(-1)).toEqual({t:'conversation:set_settings',lane:'test/lead',session:2,settings:{agy:{model:'model-b'}}})
    expect(select().disabled).toBe(true)
    callDom(app, () => app.ack())
    expect(app.results).toEqual([{lane:'test/lead',session:2,request_id:'grok-request',error:null}])
    expect(select().disabled).toBe(true)
    callDom(app, () => app.update(app.current()))
    expect(select().disabled).toBe(true)
    const applied = callDom(app, () => app.current())
    applied.options[0].current_value = 'model-b'
    callDom(app, () => app.update(applied))
    expect(select().disabled).toBe(false)
    expect(select().value).toBe('model-b')
    change('model-a')
    callDom(app, () => app.ack())
    expect(select().disabled).toBe(true)
    const rejected = callDom(app, () => app.current())
    rejected.error = 'engine rejected model'
    callDom(app, () => app.update(rejected))
    expect(select().disabled).toBe(false)
    expect(select().value).toBe('model-b')
    expect(win.document.querySelector('.conversation-settings-error')?.getAttribute('title')).toBe('engine rejected model')
  } finally { await win.happyDOM.close() }
}, 20_000)
