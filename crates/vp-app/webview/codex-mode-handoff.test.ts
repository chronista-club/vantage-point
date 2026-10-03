import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement } from 'happy-dom'

it('Codex Console の切替は確認まで送らず、キャンセルと他の session を区別する', async () => {
  const result = await build({stdin:{contents:`
    import {installChatView, requestSessionMode} from './chatview'
    import * as handoff from './codex-mode-handoff'
    window.output=handoff.observeConsoleOutput
    installChatView({attachRenderer:()=>{}})
    window.requests=[]
    document.addEventListener('vp:mode-switch-request',e=>window.requests.push(e.detail))
    window.switchMode=requestSessionMode
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'handoff/main',focused:1,sessions:[
      {key:1,agent:'codex',mode:'tui',root:true},
      {key:2,agent:'claude',mode:'tui',root:false},
      {key:3,agent:'codex',mode:'gui',root:false}
    ]}}))
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()]})
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const app=window as unknown as {requests:unknown[];switchMode:(lane:string,session:number,mode:string)=>void;output:(lane:string,session:number,bytes:Uint8Array,live:boolean)=>void}
    app.switchMode('handoff/main',1,'gui')
    expect(app.requests).toHaveLength(0)
    const dialog=window.document.querySelector('dialog')!
    expect(dialog).not.toBeNull()
    expect(dialog.textContent).toContain('中断')
    dialog.querySelector<HTMLButtonElement>('[data-handoff-cancel]')!.click()
    expect(app.requests).toHaveLength(0)
    expect(window.document.querySelector('dialog')).toBeNull()
    app.switchMode('handoff/main',1,'gui')
    app.switchMode('handoff/main',1,'gui')
    expect(window.document.querySelectorAll('dialog')).toHaveLength(1)
    window.document.querySelector<HTMLButtonElement>('[data-handoff-confirm]')!.click()
    expect(app.requests).toEqual([{lane:'handoff/main',session:1,target:'gui'}])
    app.switchMode('handoff/main',2,'gui')
    app.switchMode('handoff/main',3,'tui')
    expect(app.requests).toHaveLength(3)
    expect(window.document.querySelector('dialog')).toBeNull()
    app.switchMode('handoff/main',1,'gui')
    window.document.querySelector<HTMLButtonElement>('[data-handoff-wait]')!.click()
    expect(window.document.body.textContent).toContain('応答完了を待っています')
    const bytes=(s:string)=>new TextEncoder().encode(s)
    app.output('handoff/main',1,bytes('\x1b]9;old\x07'),false)
    app.output('handoff/main',2,bytes('\x1b]9;other session\x07'),true)
    app.output('other/main',1,bytes('\x1b]9;other lane\x07'),true)
    expect(app.requests).toHaveLength(3)
    app.output('handoff/main',1,bytes('\x1b]'),true)
    app.output('handoff/main',1,bytes('9;complete'),true)
    expect(app.requests).toHaveLength(3)
    app.output('handoff/main',1,bytes('\x07'),true)
    expect(app.requests).toHaveLength(4)
    expect(window.document.querySelector('dialog')).toBeNull()
    app.switchMode('handoff/main',1,'gui')
    window.document.querySelector<HTMLButtonElement>('[data-handoff-wait]')!.click()
    window.document.querySelector<HTMLButtonElement>('[data-handoff-cancel]')!.click()
    app.output('handoff/main',1,bytes('\x1b]9;late\x07'),true)
    expect(app.requests).toHaveLength(4)
    app.switchMode('handoff/main',1,'gui')
    window.document.querySelector<HTMLButtonElement>('[data-handoff-wait]')!.click()
    window.document.dispatchEvent(new window.CustomEvent('vp:conversation-sessions',{detail:{lane:'handoff/main',focused:1,sessions:[
      {key:1,agent:'codex',mode:'tui',engine_session_id:'different-thread',root:true}
    ]}}))
    app.output('handoff/main',1,bytes('\x1b]9;stale\x1b\\'),true)
    expect(app.requests).toHaveLength(4)
    expect(window.document.querySelector('dialog')).toBeNull()
  } finally {await window.happyDOM.close()}
},20000)
