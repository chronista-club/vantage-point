/**
 * 「画面から選ぶ」の picker — picking 中だけ Content の click を奪って `host.select`、hover を
 * `host.setHover` に写す。
 *
 * editor-host 0.9.0 で click 捕捉の主語が provider 内部の picking（`useEditorPicking`）に移り、
 * その切替 API は npm には export されていない（EditorLayer 専用）。vp-app は独自の panel
 * （THEME / SIDEBAR / CHAT / MIDI / COMPONENT）を持つので、0.8.x と同じ挙動を自前で持つ。
 * 中身は editor-host `selection.ts` の findSelectable / onMouseOver / onClick の写しで、
 * `root` が null の間（= picking していない）は何もしない。
 */
import { componentDisplayName, type ComponentFieldResolver, type ComponentKnob, type EditorHost, type SelectionInfo } from '@chronista-club/creo-ui-editor-host'

export interface Found {
  element: HTMLElement
  /** 明示 bind の field id（`data-editor-fields`）。無ければ null */
  explicitIds: string[] | null
  /** 逆引きでヒットした knob（この時点では未 register） */
  knobs: ComponentKnob[]
  componentId: string | null
}

/** 逆引きに使う resolver の面（test で stub しやすいように必要な 3 つだけ） */
export type PickResolver = Pick<ComponentFieldResolver, 'match' | 'register' | 'componentIdOf'>

function parseFieldIds(el: Element): string[] | null {
  const raw = (el as HTMLElement).dataset?.editorFields
  if (!raw) return null
  const ids = raw.split(',').map(s => s.trim()).filter(Boolean)
  return ids.length > 0 ? ids : null
}

/**
 * 祖先方向へ辿って編集対象を決める（純 calculation）。明示 bind > 逆引きヒット > component では
 * あるがノブ無し、の優先順。root の外は選択対象にしない（root が無ければ常に null = fail-closed）。
 */
export function findSelectable(el: Element | null, root: Element | null, resolver: PickResolver): Found | null {
  if (!root || !el || !root.contains(el)) return null
  let cur: Element | null = el
  let emptyComponent: Found | null = null
  while (cur && cur !== root.parentElement) {
    const explicitIds = parseFieldIds(cur)
    if (explicitIds) return { element: cur as HTMLElement, explicitIds, knobs: [], componentId: resolver.componentIdOf(cur) }
    const knobs = resolver.match(cur)
    if (knobs.length > 0) return { element: cur as HTMLElement, explicitIds: null, knobs, componentId: resolver.componentIdOf(cur) }
    if (!emptyComponent) {
      const componentId = resolver.componentIdOf(cur)
      if (componentId) emptyComponent = { element: cur as HTMLElement, explicitIds: null, knobs: [], componentId }
    }
    cur = cur.parentElement
  }
  return emptyComponent
}

/** panel ヘッダに出す「何を選んでいるか」の表示名 */
export function selectableIdOf(found: Found): string {
  const el = found.element
  const explicit = el.dataset.editorSelectableId
  if (explicit) return explicit
  if (found.componentId) return componentDisplayName(found.componentId)
  if (el.id) return `#${el.id}`
  if (found.explicitIds && found.explicitIds.length > 0) return found.explicitIds.join(',')
  return el.tagName.toLowerCase()
}

function infoOf(found: Found, fieldIds: string[]): SelectionInfo {
  return {
    targetId: selectableIdOf(found),
    fieldIds,
    componentId: found.componentId ?? undefined,
    element: found.element,
    rect: found.element.getBoundingClientRect(),
  }
}

export interface PickerOptions {
  host: EditorHost
  resolver: PickResolver
  /** 選択の scope。null = picking していない（何もしない） */
  root: () => Element | null
  /** 選び終えたとき（picking を解除する契機） */
  onSelected?: () => void
  /** Editor 自身の UI（panel）か。true なら選択対象にせず、click も奪わない */
  isChrome?: (el: Element) => boolean
}

/** picker を document に取り付ける。返り値を呼ぶと外す */
export function installPicker(opts: PickerOptions): () => void {
  if (typeof document === 'undefined' || typeof window === 'undefined') return () => {}
  const { host, resolver } = opts
  const inChrome = (el: Element | null): boolean => {
    let cur: Element | null = el
    while (cur) {
      if ((cur as HTMLElement).dataset?.editorLayer !== undefined) return true
      if (opts.isChrome?.(cur)) return true
      cur = cur.parentElement
    }
    return false
  }
  const onMouseOver = (e: MouseEvent): void => {
    const root = opts.root()
    if (!root || host.mode() !== 'on') return
    const target = e.target as Element | null
    if (inChrome(target)) { host.setHover(null); return }
    const found = findSelectable(target, root, resolver)
    host.setHover(found ? infoOf(found, found.explicitIds ?? found.knobs.map(k => k.id)) : null)
  }
  const onClick = (e: MouseEvent): void => {
    const root = opts.root()
    if (!root || host.mode() !== 'on') return
    const target = e.target as Element | null
    if (inChrome(target)) return
    const found = findSelectable(target, root, resolver)
    if (!found) { host.clearSelection(); return } // 背景クリック = 選択解除（click は奪わない）
    // picking 中は Content の click を奪う（link 誤作動を防ぐ）。逆引き分はここで初めて register する
    e.preventDefault()
    e.stopPropagation()
    const fieldIds = found.explicitIds ?? (found.knobs.length > 0 ? resolver.register(found.knobs, found.element) : [])
    host.select(infoOf(found, fieldIds))
    host.setHover(null)
    opts.onSelected?.()
  }
  document.addEventListener('mouseover', onMouseOver)
  document.addEventListener('click', onClick, true) // capture で Content click を先取り
  return () => {
    document.removeEventListener('mouseover', onMouseOver)
    document.removeEventListener('click', onClick, true)
  }
}
