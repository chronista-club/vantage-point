// mem_1Cf1r1bEcTcknGH3Xk3naa — capture belongs to its chosen Atlas, not the active Project.
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLSelectElement, type HTMLTextAreaElement, type HTMLButtonElement } from 'happy-dom'

it('requires an explicit Atlas and keeps the draft when Projects change', async () => {
  const bundle = await build({stdin:{contents:`
    import {render} from 'solid-js/web'
    import {BucketList} from './src/sidebar/actions-panel/BucketList'
    import {emptyState,applySidebarState} from './src/sidebar/store'
    import {setActionPersist,commitActions,applyActionsFromDaemon} from './src/sidebar/actions-panel/store'
    import {runCaptureMode} from './src/sidebar/actions/handlers'
    const base=emptyState()
    base.activity.auth_targets={creo:'valid'}
    base.activity.actions_rev=1
    base.activity.actions_scope='account-a'
    base.activity.actions_atlases=[{id:'atlas-personal',name:'Personal',path:'/Personal',writable:true},{id:'atlas-other',name:'別件',path:'/別件',writable:true}]
    applySidebarState(base)
    applyActionsFromDaemon([],1,'account-a')
    window.sent=[];setActionPersist(payload=>window.sent.push(payload))
    window.switchProject=()=>applySidebarState({...base,processes:[{path:'/project-b',name:'project-b'}]})
    window.showLocked=()=>commitActions([{id:'mem-locked',text:'保護されたメモ',atlas_id:'atlas-other',kind:'idea',locked:true,bucket:'ideas',order:'a'}])
    window.showRows=()=>commitActions([{id:'mem-a',text:'A',atlas_id:'atlas-other',bucket:'ideas',order:'a'},{id:'mem-b',text:'B',atlas_id:'atlas-other',bucket:'ideas',order:'b'}])
    window.captureShortcut=runCaptureMode
    window.changeAccount=()=>applySidebarState({...base,activity:{...base.activity,actions_scope:'account-b'}})
    render(()=>BucketList(),document.body)
  `,resolveDir:process.cwd(),loader:'tsx'},bundle:true,write:false,format:'iife',conditions:['browser'],plugins:[solidPlugin()]})
  const window = new Window()
  try {
    window.eval(bundle.outputFiles[0].text)
    const text = window.document.querySelector<HTMLTextAreaElement>('[aria-label="メモ"]')
    const atlas = window.document.querySelector<HTMLSelectElement>('[aria-label="保存先 Atlas"]')
    expect(text).not.toBeNull()
    expect(atlas).not.toBeNull()
    const legacy = window.document.querySelector<HTMLButtonElement>('[aria-label="以前のACTIONSを取り込む"]')
    expect(legacy).not.toBeNull()
    legacy!.click()
    expect((window as unknown as {sent:unknown[]}).sent.at(-1)).toMatchObject({scope:'account-a',import_legacy:true})
    const save = window.document.querySelector<HTMLButtonElement>('[aria-label="メモを保存"]')!
    text!.value='別件の思いつき';text!.dispatchEvent(new window.Event('input',{bubbles:true}))
    expect(save.disabled).toBe(true)
    atlas!.value='atlas-other';atlas!.dispatchEvent(new window.Event('change',{bubbles:true}))
    const app=window as unknown as {switchProject:()=>void;sent:Array<{items:unknown[]}>}
    app.switchProject()
    expect(text!.value).toBe('別件の思いつき')
    expect(atlas!.value).toBe('atlas-other')
    expect(save.disabled).toBe(false)
    save.click()
    expect(app.sent.at(-1)?.items).toEqual(expect.arrayContaining([expect.objectContaining({text:'別件の思いつき',atlas_id:'atlas-other',kind:null})]))
    const row = window.document.querySelector('[data-vp-act-row]')
    expect(row).not.toBeNull()
    expect(row?.closest('details:not([open])')).toBeNull()
    expect(row?.textContent).toContain('/別件')
    expect(row?.querySelector<HTMLButtonElement>('.vp-act-del')?.disabled).toBe(true)
    ;(window as unknown as {showRows:()=>void}).showRows()
    const original = window.document.querySelector<HTMLTextAreaElement>('[data-vp-act-row="mem-a"] textarea')!
    original.focus()
    original.dispatchEvent(new window.KeyboardEvent('keydown',{key:'ArrowDown',altKey:true,shiftKey:true,bubbles:true}))
    await Promise.resolve()
    expect(window.document.activeElement).toBe(original)
    expect(original.closest('[data-vp-act-row]')?.getAttribute('data-vp-act-row')).toBe('mem-a')
    ;(window as unknown as {showLocked:()=>void}).showLocked()
    expect(window.document.querySelector<HTMLTextAreaElement>('.vp-act-text')?.readOnly).toBe(true)
    expect(window.document.querySelector<HTMLButtonElement>('.vp-act-check')?.disabled).toBe(true)
    ;(window as unknown as {captureShortcut:()=>void}).captureShortcut()
    window.dispatchEvent(new window.KeyboardEvent('keydown',{key:'1',bubbles:true}))
    await Promise.resolve()
    expect(window.document.activeElement).toBe(text)
    text!.value='A の下書き';text!.dispatchEvent(new window.Event('input',{bubbles:true}))
    ;(window as unknown as {changeAccount:()=>void}).changeAccount()
    expect(text!.value).toBe('')
    expect(atlas!.value).toBe('')
  } finally {await window.happyDOM.close()}
},20000)
