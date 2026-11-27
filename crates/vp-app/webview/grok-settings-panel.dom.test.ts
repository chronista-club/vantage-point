// Task mem_1ChKFNBY7VQYSwFseyxUs9 — 操作と中間 / 完了 roster の接続。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLSelectElement } from 'happy-dom'

it('切替全体の完了まで待ち、補正と失敗を engine の現在値とともに描く', async () => {
  const result = await build({ stdin: { contents: `
    import { createSignal } from 'solid-js'
    import { render } from '@solidjs/web'
    import { GrokSettingsPanel } from './grok-settings-panel'
    import { installConsole } from './console'
    const consoleApi = installConsole()
    window.ack = (detail) => consoleApi.handleEvent(detail.lane, {kind:'codex_config',config:null,request_id:detail.request_id,error:detail.error ?? null},detail.session)
    const [view,setView]=createSignal({options:[
      {id:'model',name:'Model',current_value:'model-a',choices:[{value:'model-a',name:'A'},{value:'model-b',name:'B'}]},
      {id:'reasoning_effort',name:'Effort',current_value:'xhigh',choices:[{value:'xhigh',name:'XHigh'},{value:'high',name:'High'}]}
    ]})
    const timeout=window.setTimeout.bind(window); window.setTimeout=(fn,ms)=>{if(ms===35000){window.expire=fn;return 0}return timeout(fn,ms)}
    window.sent=[]; window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}
    window.current=()=>JSON.parse(JSON.stringify(view())); window.update=setView
    render(()=> GrokSettingsPanel({lane:'test/lead',session:2,lc:{state:{streaming:false}},
      rosterEntry:()=>({agent:'grok',acp_config:view()})}),document.body)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false, format: 'iife',
    conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const win = new Window()
  try {
    evalDom(win, result.outputFiles[0].text)
    const app = win as unknown as {sent: any[]; ack: (detail: any) => void; expire: () => void; current: () => any; update: (value: any) => void}
    const select = (axis: string) => win.document.querySelector<HTMLSelectElement>(`[aria-label="grok ${axis}"]`)!
    const change = (axis: string, value: string) => {
      select(axis).value = value
      dispatchDom(select(axis), new win.Event('change', { bubbles: true }))
    }
    change('Model','model-b')
    const id = app.sent.at(-1).request_id
    expect(app.sent.at(-1)).toMatchObject({lane:'test/lead',session:2,settings:{grok:{model:'model-b'}}})
    expect(select('Model').disabled).toBe(true)
    const intermediate = callDom(app, () => app.current())
    intermediate.options[0].current_value = 'model-b'
    intermediate.options[1].choices = [{value:'high',name:'High'}]
    callDom(app, () => app.update(intermediate))
    expect(select('Model').disabled).toBe(true)
    expect(select('Effort').value).toBe('xhigh')
    expect(select('Effort').textContent).toContain('候補外')
    callDom(app, () => app.expire())
    expect(win.document.body.textContent).toContain('切替結果を確認できません')
    const done = callDom(app, () => app.current())
    done.options[1].current_value = 'high'
    done.notice = {message:'effort を xhigh → high に揃えました',occurred_at_ms:1}
    callDom(app, () => app.update(done))
    callDom(app, () => app.ack({lane:'test/lead',session:2,request_id:id}))
    expect(select('Model').disabled).toBe(false)
    expect(select('Effort').value).toBe('high')
    expect(win.document.body.textContent).not.toContain('切替結果を確認できません')
    expect(win.document.body.textContent).toContain('effort を xhigh → high に揃えました')
    expect(win.document.querySelector('time')?.getAttribute('datetime')).toBe('1970-01-01T00:00:00.001Z')
    expect(win.document.querySelector('time')?.textContent).toBe(new Date(1).toLocaleTimeString())
    change('Model','model-a')
    const failed = callDom(app, () => app.current())
    failed.error = 'engine rejected model'
    // RPC の拒否が roster より先に来ても、選択を engine の既知の値に戻す。
    expect(select('Model').disabled).toBe(true)
    callDom(app, () => app.ack({lane:'test/lead',session:99,request_id:app.sent.at(-1).request_id}))
    expect(select('Model').disabled).toBe(true)
    callDom(app, () => app.ack({lane:'test/lead',session:2,request_id:app.sent.at(-1).request_id,error:'engine rejected model'}))
    expect(select('Model').disabled).toBe(false)
    expect(select('Model').value).toBe('model-b')
    expect(win.document.body.textContent).toContain('engine rejected model')
    callDom(app, () => app.update(failed))
  } finally { await win.happyDOM.close() }
}, 20_000)
