// mem_1CeySwxuoVc17bGLnU5Np3 — native settings remain the source of displayed mode and permissions.
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLSelectElement } from 'happy-dom'

it('shows effective permissions and confirms mode only from native settings', async () => {
  const result = await build({stdin:{contents:`
    import {installChatView} from './chatview'
    window.sent=[]; window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('modes/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'modes/main',focused:1,sessions:[{key:1,agent:'codex',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    api.mountSession(document.body,'modes/main',1)
    window.config={models:[],model:'fixture',effort:null,selection:null,error:null,runtime:{approval:'on-request',sandbox:'workspaceWrite',network_access:false,writable_roots:['/project'],profile:':workspace',mode:null}}
    window.emit({kind:'codex_config',config:window.config,request_id:null,error:null},1)
    window.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:null,ready:true,items:[],error:null},request_id:null,error:null},1)
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()]})
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    expect(window.document.body.textContent).toContain('on-request')
    expect(window.document.body.textContent).toContain('/project')
    const select=window.document.querySelector<HTMLSelectElement>('select[aria-label="Codex mode"]')!
    expect(select.value).toBe('')
    select.value='plan'; select.dispatchEvent(new window.Event('change',{bubbles:true}))
    const app=window as unknown as {sent:any[];config:any;emit:(event:any,session:number)=>void}
    expect(app.sent.at(-1)).toMatchObject({t:'conversation:codex_input',session:1,thread_id:'thread',action:{kind:'mode',mode:'plan'}})
    expect(select.value).toBe('')
    app.emit({kind:'codex_config',config:{...app.config,runtime:{...app.config.runtime,mode:'plan'}},request_id:null,error:null},1)
    expect(select.value).toBe('plan')
    app.emit({kind:'codex_queue',queue:null,request_id:app.sent.at(-1).request_id,error:null},1)
    app.emit({kind:'codex_queue',queue:{thread_id:'thread',turn_id:null,ready:true,items:[{id:'q',text:'next',client_id:'c',editable:true}],error:null},request_id:null,error:null},1)
    expect(select.disabled).toBe(true)
  } finally {await window.happyDOM.close()}
},20000)
