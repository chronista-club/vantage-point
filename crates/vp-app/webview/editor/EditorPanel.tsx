import { THEME_IDS, THEME_INFO, exportSnapshot, type ComponentFieldResolver, type ComponentTreeNode, type EditorField, type EditorHost } from '@chronista-club/creo-ui-editor-host'
import { For, Show, createEffect, createMemo, createSignal, onCleanup } from 'solid-js'
import { themeValues, type ThemeLibrary } from './theme-store'

export type EditorSection = 'COMPONENT' | 'CHAT' | 'SIDEBAR' | 'THEME'
const SECTIONS: EditorSection[] = ['THEME', 'SIDEBAR', 'CHAT', 'COMPONENT']
const GROUP_NAMES: Record<string, string> = { Global: '配色・Boardの文字', Surface: '背景と面' }
const FIELD_NAMES: Record<string, string> = {
  'sb.text.hint': 'Lane名・メニュー・メモ入力',
  'sb.text.meta': '空欄の案内・接続情報',
  'sb.text.micro': 'ブランチ名・状態・見出し',
  'chat.text.body': '返信の本文', 'chat.text.meta': '接続・ツールの状態', 'chat.text.micro': 'ツール詳細の見出し',
  'typography.scale': 'Board・共通部品の文字倍率', 'color.brand.hue': 'アクセントの色相', 'color.brand.chroma': 'アクセントの鮮やかさ',
  'color.surface.hue': '背景の色相', 'color.surface.chroma': '背景の鮮やかさ', 'layout.gap.sibling': '部品の間隔',
  'typography.size.xs': '文字サイズ XS', 'typography.size.s': '文字サイズ S', 'typography.size.m': '文字サイズ M',
  'typography.size.l': 'Boardの本文（Markdown）', 'typography.size.xl': '文字サイズ XL',
  'color.surface.bg.base': '画面の地', 'color.surface.bg.subtle': '控えめな背景', 'color.surface.bg.emphasis': '強調する背景',
  'color.surface.surface': 'カード・面', 'color.surface.border': '境界線', 'color.surface.border.subtle': '控えめな境界線',
  'color.surface.scrim': '背景の暗幕', 'color.surface.scrim.modal': 'モーダルの暗幕',
}

function FieldControl(props: { field: EditorField; host: EditorHost }) {
  const field = () => props.field
  const value = createMemo(() => props.host.getValue(field().id))
  const label = () => FIELD_NAMES[field().id] ?? field().label
  const setNumber = (raw: string) => {
    if (!raw.trim()) return
    const n = Number(raw)
    if (!Number.isFinite(n)) return
    props.host.setValue(field().id, Math.min(field().constraints?.max ?? Infinity, Math.max(field().constraints?.min ?? -Infinity, n)))
  }
  const [invalid, setInvalid] = createSignal(false)
  const setColor = (raw: string) => {
    const valid = CSS.supports('color', raw)
    setInvalid(!valid)
    if (valid) props.host.setValue(field().id, raw)
  }
  const hex = () => {
    const raw = String(value() ?? '#000000')
    if (/^#[\da-f]{6}$/i.test(raw)) return raw
    if (/^#[\da-f]{3}$/i.test(raw)) return '#' + raw.slice(1).split('').map(c => c + c).join('')
    // Canvas converts OKLCH/RGB to an sRGB picker color; the text control keeps full precision.
    try {
      const context = document.createElement('canvas').getContext('2d')
      if (context) {
        context.fillStyle = raw
        context.fillRect(0, 0, 1, 1)
        return '#' + Array.from(context.getImageData(0, 0, 1, 1).data).slice(0, 3).map(c => c.toString(16).padStart(2, '0')).join('')
      }
    } catch { /* text editing remains available */ }
    return '#000000'
  }
  return <div class="vp-editor-field" data-field={field().id}>
    <div class="vp-editor-field-head"><label for={`vp-field-${field().id}`}>{label()}</label>
      <button type="button" class="vp-editor-reset" aria-label={`${label()}を初期値に戻す`} title="初期値に戻す"
        disabled={value() === field().initial} onClick={() => { setInvalid(false); props.host.setValue(field().id, field().initial) }}>↺</button>
    </div>
    <Show when={field().type === 'number'}>
      <div class="vp-editor-number">
        <input type="range" aria-label={`${label()}のスライダー`} min={field().constraints?.min ?? 0} max={field().constraints?.max ?? 100}
          step={field().constraints?.step ?? 'any'} value={Number(value())} onInput={event => setNumber(event.currentTarget.value)} />
        <input id={`vp-field-${field().id}`} type="number" min={field().constraints?.min} max={field().constraints?.max}
          step={field().constraints?.step ?? 'any'} value={Number(value())} onInput={event => setNumber(event.currentTarget.value)} />
        <span>{field().constraints?.unit}</span>
      </div>
    </Show>
    <Show when={field().type === 'color'}>
      <div class="vp-editor-color">
        <input type="color" aria-label={`${label()}のカラーピッカー`} value={hex()} onInput={event => setColor(event.currentTarget.value)} />
        <input id={`vp-field-${field().id}`} type="text" value={String(value() ?? '')} spellcheck={false} aria-invalid={invalid()}
          onInput={event => setColor(event.currentTarget.value)} />
      </div>
      <Show when={invalid()}><small role="alert">有効な色を入力してください。</small></Show>
    </Show>
    <Show when={field().type === 'boolean'}><input id={`vp-field-${field().id}`} type="checkbox" checked={Boolean(value())} onChange={event => props.host.setValue(field().id, event.currentTarget.checked)} /></Show>
    <Show when={field().type === 'select'}><select id={`vp-field-${field().id}`} value={String(value())} onChange={event => props.host.setValue(field().id, event.currentTarget.value)}><For each={field().constraints?.options}>{option => <option value={option}>{option}</option>}</For></select></Show>
    <Show when={field().type === 'string' || field().type === 'readonly-text'}><input id={`vp-field-${field().id}`} value={String(value() ?? '')} readOnly={field().type === 'readonly-text'} onChange={event => props.host.setValue(field().id, event.currentTarget.value)} /></Show>
  </div>
}

function FieldGroups(props: { fields: EditorField[]; host: EditorHost }) {
  const groups = createMemo(() => {
    const groups = new Map<string, EditorField[]>()
    for (const field of props.fields) {
      const name = GROUP_NAMES[field.group ?? ''] ?? field.group ?? '調整'
      groups.set(name, [...(groups.get(name) ?? []), field])
    }
    return [...groups]
  })
  return <For each={groups()}>{([name, fields]) => <details class="vp-editor-group" open>
    <summary>{name}<span>{fields.length}</span></summary>
    <For each={fields}>{field => <FieldControl field={field} host={props.host} />}</For>
  </details>}</For>
}

function ComponentTree(props: { nodes: ComponentTreeNode[]; choose(node: ComponentTreeNode): void }) {
  return <ul class="vp-editor-tree"><For each={props.nodes}>{node => <li>
    <button type="button" onClick={() => props.choose(node)}>{node.label}<span>{node.count > 1 ? `×${node.count}` : ''}</span></button>
    <Show when={node.children.length}><ComponentTree nodes={node.children} choose={props.choose} /></Show>
  </li>}</For></ul>
}

type PanelProps = {
  host: EditorHost; resolver: ComponentFieldResolver; library: () => ThemeLibrary;
  picking: () => boolean; setPicking(picking: boolean): void;
  section: () => EditorSection; setSection(section: EditorSection): void;
  storageError: () => boolean; activate(id: string): void; save(name: string): string | undefined;
  updateSaved(): void; restoreSaved(): void;
}
export function EditorPanel(props: PanelProps) {
  const { host, resolver } = props
  const [name, setName] = createSignal('')
  const [saveError, setSaveError] = createSignal('')
  const [notice, setNotice] = createSignal('')
  const [tree, setTree] = createSignal<ComponentTreeNode[]>([])
  const [rect, setRect] = createSignal<DOMRect | null>(null)
  const highlighted = () => props.picking() ? host.hover() : host.selection()
  // ResizeObserver refreshes selection geometry; keep controls mounted while typing.
  const selectedFields = createMemo(() => host.fields().filter(field => host.selection()?.fieldIds.includes(field.id)), undefined, {
    equals: (previous, next) => previous.length === next.length && previous.every((field, index) => field === next[index]),
  })
  const sidebarFields = () => host.fields().filter(field => field.id.startsWith('sb.') && field.id !== 'sb.text.base')
  const chatFields = () => host.fields().filter(field => field.id.startsWith('chat.'))
  const themeFields = () => host.fields().filter(field => !field.id.startsWith('sb.') && !field.id.startsWith('chat.') && field.scope !== 'component' &&
    !['typography.size.xs', 'typography.size.s', 'typography.size.m', 'typography.size.xl', 'layout.gap.sibling'].includes(field.id))
  const custom = () => props.library().custom.find(theme => theme.id === props.library().selected)
  const changed = () => Object.keys(themeValues(props.library())).length > 0
  const chooseTab = (section: EditorSection) => { props.setSection(section); props.setPicking(false); host.setHover(null) }
  const chooseNode = (node: ComponentTreeNode) => {
    const fields = resolver.register(resolver.match(node.element), node.element)
    host.select({ targetId: node.label, componentId: node.componentId, fieldIds: fields, element: node.element, rect: node.element.getBoundingClientRect() })
  }
  createEffect(() => {
    const selection = highlighted()
    const update = () => setRect(selection?.element?.isConnected ? selection.element.getBoundingClientRect() : null)
    update()
    const observer = new ResizeObserver(update)
    if (selection?.element) observer.observe(selection.element)
    window.addEventListener('resize', update)
    window.addEventListener('scroll', update, true)
    onCleanup(() => { observer.disconnect(); window.removeEventListener('resize', update); window.removeEventListener('scroll', update, true) })
  })
  let panel: HTMLElement | undefined
  createEffect(() => {
    if (host.mode() !== 'on') return
    const previous = document.activeElement as HTMLElement | null
    queueMicrotask(() => panel?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus())
    onCleanup(() => { if (previous?.isConnected && panel?.contains(document.activeElement)) previous.focus() })
  })
  return <Show when={host.mode() === 'on'}>
    <div data-editor-layer class="vp-editor-overlay">
      <Show when={rect()}>{bounds => <div class="vp-editor-outline" style={{ left: `${bounds().left}px`, top: `${bounds().top}px`, width: `${bounds().width}px`, height: `${bounds().height}px` }}>
        <span>{highlighted()?.targetId}</span>
      </div>}</Show>
      <aside ref={panel} class="vp-editor" role="dialog" aria-modal="false" aria-label="Vantage Point Editor">
        <header class="vp-editor-header"><div><small>VANTAGE POINT</small><h2>Editor</h2></div>
          <button type="button" aria-label="Editor を閉じる" title="閉じる（Ctrl+Shift+E）" onClick={() => host.disable()}>×</button>
        </header>
        <div class="vp-editor-tabs" role="tablist" aria-label="調整する範囲" aria-orientation="horizontal">
          <For each={SECTIONS}>{section => <button type="button" id={`vp-editor-tab-${section}`} role="tab" aria-selected={props.section() === section}
            aria-controls="vp-editor-content" tabIndex={props.section() === section ? 0 : -1} onClick={() => chooseTab(section)}
            onKeyDown={event => {
              const index = SECTIONS.indexOf(section)
              const next = event.key === 'ArrowRight' ? SECTIONS[(index + 1) % SECTIONS.length] : event.key === 'ArrowLeft' ? SECTIONS[(index + SECTIONS.length - 1) % SECTIONS.length] : event.key === 'Home' ? SECTIONS[0] : event.key === 'End' ? SECTIONS[SECTIONS.length - 1] : undefined
              if (next) { event.preventDefault(); chooseTab(next); document.getElementById(`vp-editor-tab-${next}`)?.focus() }
            }}>{section}</button>}</For>
        </div>
        <div class="vp-editor-content" id="vp-editor-content" role="tabpanel" aria-labelledby={`vp-editor-tab-${props.section()}`}>
          <Show when={props.section() === 'COMPONENT'}>
            <div class="vp-editor-intro"><h3>画面の部品を調整</h3><p>調整したい場所を画面から選びます。</p>
              <button type="button" class="vp-editor-primary" aria-pressed={props.picking()} onClick={() => {
                if (props.picking()) { props.setPicking(false); host.setHover(null) }
                else { host.clearSelection(); props.setPicking(true) }
              }}>{props.picking() ? '選択をやめる' : '画面から選ぶ'}</button>
              <Show when={props.picking()}><p role="status">画面の部品をクリック。Esc で取り消せます。</p></Show>
            </div>
            <Show when={host.selection()} fallback={<p class="vp-editor-empty">まだ選択していません。SIDEBAR と THEME は、選択せずに調整できます。</p>}>
              <div class="vp-editor-selection"><strong>{host.selection()?.targetId}</strong><button type="button" onClick={() => host.clearSelection()}>選択解除</button>
                <p>{host.selection()?.componentId ? '同じ種類のすべての部品に反映します。枠は選んだ場所を示しています。' : '選択した対象に登録された設定を調整します。'}</p>
              </div>
              <Show when={selectedFields().length} fallback={<p class="vp-editor-empty">この部品には調整項目がありません。別の部品を選んでください。</p>}><FieldGroups fields={selectedFields()} host={host} /></Show>
            </Show>
            <details class="vp-editor-discovery" onToggle={event => { if (event.currentTarget.open) setTree(resolver.tree()) }}>
              <summary>一覧から探す</summary><p>いま画面にある部品の階層です。</p>
              <ComponentTree nodes={tree()} choose={chooseNode} />
            </details>
          </Show>
          <Show when={props.section() === 'SIDEBAR'}>
            <div class="vp-editor-intro"><h3>サイドバー全体</h3><p>選択は不要です。文字、背景、選択行と状態ポイントを調整します。</p></div>
            <FieldGroups fields={sidebarFields()} host={host} />
          </Show>
          <Show when={props.section() === 'CHAT'}>
            <div class="vp-editor-intro"><h3>Chatの見た目</h3><p>返信、自分のメッセージ、ツールや状態表示の文字を調整します。すべてのChatに反映します。</p></div>
            <FieldGroups fields={chatFields()} host={host} />
          </Show>
          <Show when={props.section() === 'THEME'}>
            <div class="vp-editor-intro"><h3>アプリ全体のテーマ</h3><p>配色と、Boardの文字を調整します。端末やBoard内のWebページは対象外です。</p></div>
            <label class="vp-editor-label">テーマ<select aria-label="テーマを選ぶ" value={props.library().selected} onChange={event => props.activate(event.currentTarget.value)}>
              <optgroup label="プリセット"><For each={THEME_IDS}>{id => <option value={id}>{THEME_INFO[id].name}</option>}</For></optgroup>
              <Show when={props.library().custom.length}><optgroup label="カスタム"><For each={props.library().custom}>{theme => <option value={theme.id}>{theme.name}</option>}</For></optgroup></Show>
            </select></label>
            <div class="vp-editor-save">
              <p>このテーマでの調整は自動で保持します。名前を付けると、選び直して使えます。</p>
              <form onSubmit={event => { event.preventDefault(); const error = props.save(name()); setSaveError(error ?? ''); if (!error) { setName(''); setNotice('カスタムテーマを保存しました。') } }}>
                <input aria-label="カスタムテーマ名" placeholder="例：夜の作業台" maxLength={80} value={name()} onInput={event => setName(event.currentTarget.value)} />
                <button type="submit" disabled={!name().trim()}>名前を付けて保存</button>
              </form>
              <Show when={custom()}><button type="button" onClick={() => { props.updateSaved(); setNotice('変更を保存しました。') }}>変更を保存</button></Show>
              <button type="button" disabled={!changed()} onClick={props.restoreSaved}>{custom() ? '保存した状態に戻す' : 'プリセットの初期値に戻す'}</button>
              <Show when={saveError()}><p role="alert">{saveError()}</p></Show><Show when={notice()}><p role="status">{notice()}</p></Show>
            </div>
            <FieldGroups fields={themeFields()} host={host} />
          </Show>
          <details class="vp-editor-export"><summary>調整値を書き出す</summary><p>AI への受け渡しやソースへの反映に使えます。</p><textarea aria-label="調整値 JSON" readOnly value={exportSnapshot(host, { format: 'json', onlyChanged: true })} /></details>
        </div>
        <footer><span>Ctrl+Shift+E · 開閉</span><span>Esc · 選択解除／閉じる</span></footer>
        <Show when={props.storageError()}><p class="vp-editor-storage-error" role="alert">保存先を利用できません。この画面を閉じる前に調整値を書き出してください。</p></Show>
      </aside>
    </div>
  </Show>
}
