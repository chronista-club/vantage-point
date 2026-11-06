// mem_1Cg1m3LoeDu62UCKVT5UcA — the real Solid components in a DOM.
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLButtonElement, type HTMLTextAreaElement } from 'happy-dom'

it('shows the Creo tree in a keyboard popover and refreshes once without losing user work', async () => {
  const bundle=await build({stdin:{contents:`
    import {render} from 'solid-js/web'
    import {BucketList} from './src/sidebar/actions-panel/BucketList'
    import {emptyState,applySidebarState} from './src/sidebar/store'
    import {openSidebarDispatch} from './src/sidebar/dispatch'
    import {installIpcBridge} from './src/sidebar/ipc'
    import {commitActions,actions} from './src/sidebar/actions-panel/store'
    window.sent=[];window.ipc={postMessage:msg=>window.sent.push(JSON.parse(msg))}
    openSidebarDispatch();installIpcBridge()
    const base=emptyState()
    base.activity.auth_targets={creo:'valid'}
    base.activity.actions_scope='a';base.activity.actions_rev=1
    base.activity.actions_atlases=[
      {id:'parent',name:'親',path:'/親',writable:false},
      {id:'first',name:'先',path:'/親/先',parentId:'parent',writable:true},
      {id:'last',name:'後',path:'/親/後',parentId:'parent',writable:true,source:'shared',owner:'Owner'},
      {id:'hidden',name:'隠す',path:'/隠す',writable:true}]
    base.activity.actions_sidebar_settings={version:1,order:{parent:['last','first']},hidden:['hidden']}
    applySidebarState(base)
    window.hideSelected=()=>applySidebarState({...base,activity:{...base.activity,actions_sidebar_settings:{version:1,hidden:['parent','hidden']}}})
    window.changeProject=()=>applySidebarState({...base,processes:[{path:'/other',name:'Other'}]})
    window.pending=()=>commitActions([{id:'act-pending',text:'保存待ち',atlas_id:'first',bucket:'ideas',order:'a'}])
    window.failed=()=>applySidebarState({...base,activity:{...base.activity,actions_error:"同じ失敗",actions_sidebar_error:"設定も失敗"}})
    window.rows=actions
    render(()=>BucketList(),document.body)
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],preserveSymlinks:true,plugins:[solidPlugin()]})
  const w=new Window()
  try {
    w.eval(bundle.outputFiles[0].text)
    const d=w.document
    const picker=d.querySelector<HTMLButtonElement>('button[aria-label="保存先 Atlas"]')
    expect(picker).not.toBeNull()
    picker!.click()
    const rows=()=>Array.from(d.querySelectorAll('[role="option"]'))
    expect(rows().map(r=>r.textContent?.trim())).toEqual(['親','後','先'])
    expect(rows()[0].getAttribute('aria-disabled')).toBe('true')
    expect(rows()[1].getAttribute('title')).toContain('共有 · Owner')
    expect(rows()[1].getAttribute('style')).toContain('28px')
    const list=d.querySelector('[role="listbox"]')!
    list.dispatchEvent(new w.KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}))
    list.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Enter',bubbles:true}))
    expect(picker!.textContent).toContain('/親/後')
    expect(d.querySelector('[role="listbox"]')).toBeNull()
    const text=d.querySelector<HTMLTextAreaElement>('[aria-label="メモ"]')!
    text.value='下書き';text.dispatchEvent(new w.Event('input',{bubbles:true}))
    const app=w as unknown as {sent:Array<{t:string;req:string}>;failed:()=>void;pending:()=>void;hideSelected:()=>void;changeProject:()=>void;rows:()=>unknown[];vpSidebarDispatch:(v:unknown)=>void}
    app.pending();app.changeProject()
    expect(text.value).toBe('下書き')
    expect(picker!.textContent).toContain('/親/後')
    app.hideSelected();picker!.click()
    expect(rows().map(r=>r.getAttribute('data-atlas-id'))).toContain('last')
    d.querySelector('[role="listbox"]')!.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Escape',bubbles:true}))
    expect(d.activeElement).toBe(picker)
    const reload=d.querySelector<HTMLButtonElement>('[aria-label="ACTIONs を再読み込み"]')!
    expect(reload).not.toBeNull()
    reload.click();reload.click()
    const requests=app.sent.filter(m=>m.t==='actions:refresh')
    expect(requests).toHaveLength(1)
    expect(reload.disabled).toBe(true)
    expect(d.querySelector('details')!.hasAttribute('open')).toBe(true)
    app.failed()
    app.vpSidebarDispatch({t:'actions:refresh_result',req:requests[0].req,error:'Creo に接続できません'})
    await Promise.resolve();await Promise.resolve()
    expect(reload.disabled).toBe(false)
    expect(d.querySelector('[role="status"]')!.textContent).toContain('Creo に接続できません')
    expect(d.querySelectorAll('[role="status"]')).toHaveLength(1)
    expect(text.value).toBe('下書き')
    expect(picker!.textContent).toContain('/親/後')
    expect(app.rows()).toEqual(expect.arrayContaining([expect.objectContaining({id:'act-pending'})]))
  } finally {await w.happyDOM.close()}
},20000)
