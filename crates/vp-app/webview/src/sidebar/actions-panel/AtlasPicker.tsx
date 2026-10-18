import { createMemo, createSignal, createUniqueId, For, onCleanup, Show } from 'solid-js'
import { Portal } from 'solid-js/web'
import type { ActionAtlas } from '../../generated/ActionAtlas'
import { buildAtlasTree } from './atlas-tree'

type Atlas = ActionAtlas
/** Read-only counterpart of Creo AtlasRailPeek's LayerRow; no placement/edit controls. */
export function AtlasPicker(props: { atlases: Atlas[]; settings?: unknown; selected: string; onSelect: (id: string) => void }) {
  const uid = createUniqueId()
  const [open,setOpen] = createSignal(false)
  const [active,setActive] = createSignal(0)
  const [position,setPosition] = createSignal({left:0,top:0,width:260,height:300})
  let trigger!: HTMLButtonElement
  let list: HTMLDivElement | undefined
  const tree = createMemo(()=>buildAtlasTree(props.atlases,props.settings))
  const rows = createMemo(()=>{
    const result = tree().rows
    const selected = props.atlases.find(a=>a.id===props.selected)
    // Hidden is a view preference, never an instruction to silently change the destination.
    return selected && !result.some(r=>r.node.id===selected.id)
      ? [...result,{node:selected,depth:0,parent:'root'}] : result
  })
  const selected = () => props.atlases.find(a=>a.id===props.selected)
  const close = (focus = true) => {setOpen(false); if(focus) trigger.focus()}
  const place = () => {
    const r = trigger.getBoundingClientRect()
    const width = Math.min(Math.max(r.width,260),window.innerWidth-16)
    const below = window.innerHeight-r.bottom-8
    const height = Math.min(320,Math.max(below,r.top-8))
    setPosition({left:Math.max(8,Math.min(r.left,window.innerWidth-width-8)),top:below>=height?r.bottom+4:Math.max(8,r.top-height-4),width,height})
  }
  const show = () => {
    place();setActive(Math.max(0,rows().findIndex(r=>r.node.id===props.selected)));setOpen(true)
    queueMicrotask(()=>list?.focus())
  }
  const select = (index: number) => {
    const node=rows()[index]?.node
    if (!node?.writable) return
    props.onSelect(node.id);close()
  }
  const outside = (event: PointerEvent) => {
    const target=event.target as Node
    if(open() && !trigger.contains(target) && !list?.contains(target)) close(false)
  }
  const reposition = () => {if(open()) place()}
  document.addEventListener('pointerdown',outside)
  window.addEventListener('resize',reposition)
  onCleanup(()=>{document.removeEventListener('pointerdown',outside);window.removeEventListener('resize',reposition)})
  const key = (event: KeyboardEvent) => {
    if(event.key==='Escape') {event.preventDefault();event.stopPropagation();close();return}
    if(event.key==='Tab') {close();return}
    if(event.key==='Enter' || event.key===' ') {event.preventDefault();select(active());return}
    const end=rows().length-1
    const next=event.key==='ArrowDown'?Math.min(active()+1,end):event.key==='ArrowUp'?Math.max(active()-1,0):event.key==='Home'?0:event.key==='End'?end:null
    if(next===null) return
    event.preventDefault();setActive(next)
    document.getElementById(`${uid}-${next}`)?.scrollIntoView?.({block:'nearest'})
  }
  return <>
    <style>{PICKER_CSS}</style>
    <button ref={trigger} type="button" class="vp-atlas-trigger" aria-label="保存先 Atlas" aria-haspopup="listbox"
      aria-expanded={open()} aria-controls={open()?uid:undefined}
      onClick={()=>open()?close():show()} onKeyDown={e=>{if(e.key==='ArrowDown'||e.key==='ArrowUp'){e.preventDefault();show()}}}>
      {selected()?.path || selected()?.name || (props.selected?'Atlas 未確認':'Atlas を選択')}
    </button>
    <Show when={tree().invalid}><span class="vp-act-status" role="status">Creo の並び設定を読めませんでした</span></Show>
    <Show when={open()}><Portal>
      <div ref={list} id={uid} class="vp-atlas-popover" role="listbox" aria-label="保存先 Atlas の一覧" tabindex="-1"
        aria-activedescendant={rows().length?`${uid}-${active()}`:undefined}
        style={{left:`${position().left}px`,top:`${position().top}px`,width:`${position().width}px`,'max-height':`${position().height}px`}}
        onKeyDown={key}>
        <For each={rows()}>{(row,index)=><div id={`${uid}-${index()}`} role="option" data-atlas-id={row.node.id}
          aria-selected={row.node.id===props.selected} aria-disabled={!row.node.writable}
          class="vp-atlas-option" classList={{'is-active':index()===active(),'is-selected':row.node.id===props.selected}}
          style={{'padding-left':`${12+row.depth*16}px`}}
          title={[row.node.source==='shared'?`共有${row.node.owner?` · ${row.node.owner}`:''}`:row.node.path,!row.node.writable?'読み取り専用':null].filter(Boolean).join(' · ')}
          onPointerMove={()=>setActive(index())} onClick={()=>select(index())}>
          <span>{row.node.name}</span><Show when={row.node.id===props.selected}><small>選択中</small></Show>
        </div>}</For>
        <Show when={!rows().length}><div class="vp-act-status">Atlas がありません</div></Show>
      </div>
    </Portal></Show>
  </>
}
const PICKER_CSS=`
.vp-atlas-trigger{flex:1;min-width:0;text-align:left;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.vp-atlas-popover{font-family:var(--vp-font-sans,var(--typography-family-sans,system-ui));letter-spacing:normal;position:fixed;z-index:10000;box-sizing:border-box;overflow-y:auto;padding:8px 0;border:1px solid var(--lg-mute-2,#38525b);border-radius:6px;background:var(--lg-panel,#0A0E15);color:var(--lg-hot,#EAFBFF);box-shadow:0 6px 24px #0005;outline:none}
.vp-atlas-option{display:flex;align-items:center;gap:8px;padding:6px 12px;font-size:12px;line-height:16px;border-radius:6px;color:var(--lg-mute,#5C7A85);cursor:pointer}
.vp-atlas-option>span{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.vp-atlas-option.is-active{background:var(--lg-hover,#ffffff0c);color:var(--lg-hot,#EAFBFF)}
.vp-atlas-option.is-selected{background:color-mix(in srgb,var(--lg-cyan,#40d9ec) 14%,transparent);color:var(--lg-cyan,#40d9ec);font-weight:500}
.vp-atlas-option[aria-disabled=true]{opacity:.5;cursor:default}
.vp-atlas-option small{font-size:10px;letter-spacing:.05em;white-space:nowrap}
`
