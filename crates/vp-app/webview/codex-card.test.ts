import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement, type HTMLInputElement } from 'happy-dom'

it('MCP の空文字の選択肢と未回答を区別し、任意の選択を取り消せる', async () => {
  const result = await build({stdin:{contents:`
    import {render} from 'solid-js/web'
    import {createComponent} from 'solid-js'
    import {CodexInteractionCard} from './codex-interactions'
    window.answers=[]
    render(() => createComponent(CodexInteractionCard,{sending:false,request:{request_id:'enum',kind:'mcp_elicitation',title:'MCP',details:'',questions:[],can_accept:true,blocking:true,
      elicitation:{server_name:'test',message:'選択',url:null,fields:[{id:'field:choice',title:'選択',description:'',kind:'string',required:false,default_value:null,options:[{value:'',label:'空文字'},{value:'b',label:'B'}]}]}},
      respond:(id,behavior,answers)=>window.answers.push(answers)}),document.body)
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()]})
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const select = window.document.querySelector('select')!
    const send = window.document.querySelector<HTMLButtonElement>('.conversation-prompt-confirm')!
    select.value='0'; select.dispatchEvent(new window.Event('change',{bubbles:true})); send.click()
    select.value='-1'; select.dispatchEvent(new window.Event('change',{bubbles:true})); send.click()
    expect((window as unknown as {answers:unknown[]}).answers).toEqual([{'field:choice':''},{}])
  } finally {await window.happyDOM.close()}
},15000)

it('MCP の任意項目と false を区別し、URL を開いただけでは完了しない', async () => {
  const result = await build({
    stdin: { contents: `
      import { render } from 'solid-js/web'
      import { createComponent } from 'solid-js'
      import { CodexInteractionCard } from './codex-interactions'
      window.answers = []
      const base = {kind:'mcp_elicitation', title:'MCP',details:'',questions:[],can_accept:true,blocking:true}
      window.opened = []
      window.ipc = { postMessage: message => window.opened.push(JSON.parse(message)) }
      const respond = (id,behavior,answers) => window.answers.push({id,behavior,answers})
      render(() => createComponent(CodexInteractionCard, {sending:false,respond,request:{...base,request_id:'form',elicitation:{server_name:'documents',message:'設定',url:null,fields:[
        {id:'field:enabled',title:'有効化',description:'',kind:'boolean',required:true,options:[],default_value:null},
        {id:'field:note',title:'メモ',description:'',kind:'string',required:false,options:[],default_value:null}
      ]}}}), document.body)
      render(() => createComponent(CodexInteractionCard, {sending:false,respond,request:{...base,request_id:'url',elicitation:{server_name:'auth',message:'認証',url:'https://example.com/verify',fields:[]}}}), document.body)
    `, resolveDir:process.cwd(),loader:'tsx'}, bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()],
  })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const form = window.document.querySelector('#form')!
    expect(form.textContent).toContain('documents')
    const confirm = form.querySelector<HTMLButtonElement>('.conversation-prompt-confirm')!
    expect(confirm.disabled).toBe(true)
    const select = form.querySelector('select')!
    select.value = 'false'
    select.dispatchEvent(new window.Event('change',{bubbles:true}))
    confirm.click()
    expect((window as unknown as {answers:unknown[]}).answers).toEqual([{id:'form',behavior:'allow',answers:{'field:enabled':'false'}}])
    const url = window.document.querySelector('#url')!
    const done = url.querySelector<HTMLButtonElement>('.conversation-prompt-confirm')!
    expect(done.disabled).toBe(true)
    url.querySelector('a')!.click()
    expect((window as unknown as {opened:unknown[]}).opened).toEqual([{t:'open-url',url:'https://example.com/verify'}])
    expect((window as unknown as {answers:unknown[]}).answers).toHaveLength(1)
    url.querySelector<HTMLInputElement>('input[type=checkbox]')!.click()
    done.click()
    expect((window as unknown as {answers:unknown[]}).answers[1]).toEqual({id:'url',behavior:'allow',answers:{action:'accept'}})
    url.querySelector<HTMLButtonElement>('[data-mcp-cancel]')!.click()
    expect((window as unknown as {answers:unknown[]}).answers[2]).toEqual({id:'url',behavior:'allow',answers:{action:'cancel'}})
  } finally {await window.happyDOM.close()}
},15000)

it('独立権限は未選択から項目と期間を選び、明示送信する', async () => {
  const result = await build({
    stdin: { contents: `
      import { render } from 'solid-js/web'
      import { createComponent } from 'solid-js'
      import { CodexInteractionCard } from './codex-interactions'
      window.answers = []
      render(() => createComponent(CodexInteractionCard, { sending: false, request: {
        request_id: 'p', kind: 'permissions', title: '追加権限', details: '', can_accept: true, blocking: true,
        questions: [{id:'p0', header:'', question:'読み取り: /docs', options:[], is_secret:false},
          {id:'scope', header:'', question:'許可の期間', options:[], is_secret:false}]
      }, respond: (id, behavior, answers) => window.answers.push({id, behavior, answers}) }), document.body)
    `, resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', conditions: ['browser'], plugins: [solidPlugin()],
  })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const doc = window.document
    const checkbox = doc.querySelector<HTMLInputElement>('input[type=checkbox]')
    expect(checkbox).not.toBeNull()
    expect(checkbox!.checked).toBe(false)
    const confirm = doc.querySelector<HTMLButtonElement>('.conversation-prompt-confirm')!
    expect(confirm.disabled).toBe(true)
    checkbox!.click()
    expect((window as unknown as {answers:unknown[]}).answers).toEqual([])
    expect(doc.querySelector<HTMLInputElement>('input[value=turn]')!.checked).toBe(true)
    doc.querySelector<HTMLInputElement>('input[value=session]')!.click()
    confirm.click()
    expect((window as unknown as {answers:unknown[]}).answers).toEqual([{id:'p',behavior:'allow',answers:{p0:'allow',scope:'session'}}])
  } finally { await window.happyDOM.close() }
}, 15000)

it('拒否が turn 中断になる承認は、操作の意味を明示する', async () => {
  const result = await build({
    stdin: { contents: `
      import { render } from 'solid-js/web'
      import { createComponent } from 'solid-js'
      import { CodexInteractionCard } from './codex-interactions'
      window.answers = []
      render(() => createComponent(CodexInteractionCard, { sending: false, request: {
        request_id: 'approval', kind: 'command', title: '承認', details: '',
        can_accept: true, blocking: true, questions: [], cancel_on_deny: true
      }, respond: (id, behavior) => window.answers.push({id, behavior}) }), document.body)
    `, resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', conditions: ['browser'], plugins: [solidPlugin()],
  })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const button = window.document.querySelector<HTMLButtonElement>('.conversation-prompt-cancel')!
    expect(button.textContent).toBe('許可せずターンを中断')
    button.click()
    expect((window as unknown as { answers: unknown[] }).answers).toEqual([{id: 'approval', behavior: 'deny'}])
  } finally { await window.happyDOM.close() }
}, 15000)

it('生成した質問カードは選択・自由入力を保持し、送信操作だけで回答する', async () => {
  const result = await build({
    stdin: { contents: `
      import { render } from 'solid-js/web'
      import { createComponent } from 'solid-js'
      import { CodexInteractionCard } from './codex-interactions'
      window.answers = []
      render(() => createComponent(CodexInteractionCard, { sending: false, request: {
        request_id: 'q', kind: 'async_question', title: '質問', details: '', can_accept: true, blocking: false,
        questions: [{ id: '0', header: '', question: 'どちら？', is_secret: false,
          options: [{label:'A',description:''},{label:'B',description:''}] }]
      }, respond: (id, behavior, answers) => window.answers.push({id, behavior, answers}) }), document.body)
    `, resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', conditions: ['browser'], plugins: [solidPlugin()],
  })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const doc = window.document
    const confirm = doc.querySelector<HTMLButtonElement>('.conversation-prompt-confirm')!
    expect(confirm.disabled).toBe(true)
    doc.querySelector<HTMLButtonElement>('.conversation-prompt-opt')!.click()
    expect((window as unknown as { answers: unknown[] }).answers).toEqual([])
    expect(doc.querySelector<HTMLInputElement>('input')!.value).toBe('A')
    confirm.click()
    expect((window as unknown as { answers: unknown[] }).answers).toEqual([{ id: 'q', behavior: 'allow', answers: { '0': 'A' } }])
    const input = doc.querySelector<HTMLInputElement>('input')!
    input.value = '自由な回答'
    input.dispatchEvent(new window.Event('input', { bubbles: true }))
    confirm.click()
    expect((window as unknown as { answers: unknown[] }).answers[1]).toEqual({ id: 'q', behavior: 'allow', answers: { '0': '自由な回答' } })
  } finally {
    await window.happyDOM.close()
  }
}, 15000)
