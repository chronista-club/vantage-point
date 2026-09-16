// mem_1Cf4jdULqRjEXawJPqbxfS — 明示選択だけで権限を変更する。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement } from 'happy-dom'

it('loads choices without changing permissions and waits for native confirmation', async () => {
  const result = await build({stdin:{contents:`
    import {installChatView} from './chatview'
    window.sent=[]; window.ipc={postMessage:m=>window.sent.push(JSON.parse(m))}
    const api=installChatView({attachRenderer:(lane,fn)=>window.emit=fn})
    api.showLane('permissions/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions',{detail:{lane:'permissions/main',focused:1,sessions:[{key:1,agent:'codex',kind:'chat',root:true,model_choices:[],permission_choices:[]}]}}))
    api.mountSession(document.body,'permissions/main',1)
    window.config={models:[],model:'fixture',effort:null,selection:null,error:null,runtime:{approval:'on-request',reviewer:'user',preset:'standard',sandbox:'workspaceWrite',network_access:false,writable_roots:['/project'],profile:':workspace',mode:'default'},permission_choices:[
      {id:'standard',label:'標準',description:'作業範囲内の編集',disabled_reason:null},
      {id:'auto-review',label:'代わりに承認',description:'自動レビュー',disabled_reason:null},
      {id:'full-access',label:'フルアクセス',description:'制限を外して実行',disabled_reason:null},
      {id:'profile:locked',label:'locked',description:'管理設定',disabled_reason:'管理設定により選択できません。'}]}
    window.emit({kind:'codex_config',config:window.config,request_id:null,error:null},1)
    window.queue={thread_id:'thread',turn_id:null,ready:true,items:[],error:null}
    window.emit({kind:'codex_queue',queue:window.queue,request_id:null,error:null},1)
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()]})
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const app=window as unknown as {sent:any[];config:any;queue:any;emit:(event:any,session:number)=>void}
    const button=(label:string)=>Array.from(window.document.querySelectorAll('button')).find(b=>b.textContent?.includes(label))!
    const requests=()=>app.sent.filter(m=>m.t==='conversation:codex_input')
    const ack=()=>app.emit({kind:'codex_queue',queue:null,request_id:requests().at(-1).request_id,error:null},1)
    expect(requests()).toHaveLength(0)
    const badge=window.document.querySelector<HTMLButtonElement>('[aria-label="Codex permissions"]')!
    expect(badge).not.toBeNull()
    expect(badge.textContent).toContain('標準')
    badge.click()
    expect(requests().at(-1).action.kind).toBe('permission_options')
    expect(requests().some(m=>m.action.kind==='permissions')).toBe(false)
    ack()
    expect(button('locked').disabled).toBe(true)
    button('代わりに承認').click()
    expect(requests().at(-1)).toMatchObject({session:1,thread_id:'thread',action:{kind:'permissions',choice:'auto-review'}})
    expect(badge.textContent).toContain('標準')
    const next={...app.config,runtime:{...app.config.runtime,reviewer:'auto_review',preset:'auto-review'}}
    app.emit({kind:'codex_config',config:next,request_id:null,error:null},2)
    expect(badge.textContent).toContain('標準')
    app.emit({kind:'codex_config',config:next,request_id:null,error:null},1)
    expect(badge.textContent).toContain('代わりに承認')
    ack()
    badge.click(); ack()
    const before=requests().length
    button('フルアクセス').click()
    expect(requests()).toHaveLength(before)
    expect(window.document.body.textContent).toContain('この会話')
    button('フルアクセスに切り替える').click()
    expect(requests().at(-1).action).toMatchObject({kind:'permissions',choice:'full-access',confirmed:true})
    ack()
    app.emit({kind:'codex_queue',queue:{...app.queue,items:[{id:'q',text:'next',client_id:'c',editable:true}]},request_id:null,error:null},1)
    expect(badge.disabled).toBe(true)
  } finally {await window.happyDOM.close()}
},20000)
