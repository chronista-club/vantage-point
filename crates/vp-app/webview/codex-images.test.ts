// mem_1Cf4jdULqRjEXawJPqbxfS — Codex への画像転送と失敗時の添付復元。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window, type HTMLButtonElement } from 'happy-dom'

it.each(['steer', 'add'])('images survive Codex %s rejection', async (kind) => {
  const result=await build({stdin:{contents:`
    import {installChatView} from './chatview'
    window.sent=[]; window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('images/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'images/main',focused:1,sessions:[{key:1,agent:'codex',kind:'chat',root:true,image_capable:true,model_choices:[],permission_choices:[]}]}}))
    api.mountSession(document.body,'images/main',1)
    window.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:'turn',ready:true,items:[],error:null},request_id:null,error:null},1)
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],preserveSymlinks: true, plugins:[solidPlugin()]})
  const window=new Window()
  try {
    evalDom(window, result.outputFiles[0].text)
    const input=window.document.querySelector('textarea')!
    const file=new window.File(['hello'],'test.png',{type:'image/png'})
    const event=new window.Event('paste',{bubbles:true,cancelable:true})
    Object.defineProperty(event,'clipboardData',{value:{items:[{kind:'file',type:'image/png',getAsFile:()=>file}]}})
    dispatchDom(input, event)
    await new Promise(resolve=>setTimeout(resolve,20))
    expect(event.defaultPrevented).toBe(true)
    input.value='describe'; dispatchDom(input, new window.Event('input',{bubbles:true}))
    if(kind==='steer') dispatchDom(input, new window.KeyboardEvent('keydown',{key:'Enter',bubbles:true}))
    else clickDom([...window.document.querySelectorAll<HTMLButtonElement>('button')].find(b=>b.textContent?.includes('次に実行する'))!)
    const app=window as unknown as {sent:any[];emit:(event:any,session:number)=>void}
    const request=app.sent.at(-1)
    expect(request.action).toMatchObject({kind,text:'describe',images:[{media_type:'image/png',data:'aGVsbG8='}]})
    callDom(app, () => app.emit({kind:'codex_queue',queue:null,request_id:request.request_id,error:'rejected'},1))
    ;clickDom([...window.document.querySelectorAll<HTMLButtonElement>('button')].find(b=>b.textContent==='入力を戻す')!)
    expect(input.value).toBe('describe')
    expect(window.document.querySelectorAll('.conversation-attachment')).toHaveLength(1)
  } finally {await window.happyDOM.close()}
},20000)
