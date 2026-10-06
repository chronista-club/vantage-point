import { EditorHostProvider, createComponentFieldResolver, useEditorHost, type EditorField, type EditorHost, type ThemeId } from '@chronista-club/creo-ui-editor-host'
import { Show, createEffect, createSignal, getOwner, onCleanup, onMount, untrack } from 'solid-js'
import { AppTokenBinds } from './app-fields'
import { EditorPanel, type EditorSection } from './EditorPanel'
import { EDITOR_CSS } from './styles'
import { RUNTIME_NAMESPACE, clearRuntimeStorage, isThemeId, readThemeLibrary, themeBase, themeValues, writeThemeLibrary, type StoragePort, type ThemeLibrary, type Values } from './theme-store'

const MANAGED_VAR = /^--(?:color-(?:brand|surface)-|typography-|layout-gap-sibling$|_|sb-|lg-|chat-text-|vp-resizer-breathe-)/
function clearEditorStyles(): void {
  const style = document.documentElement.style
  for (const name of Array.from(style)) if (MANAGED_VAR.test(name)) style.removeProperty(name)
}

function validValue(field: EditorField, value: unknown): boolean {
  if (field.type === 'number') return typeof value === 'number' && Number.isFinite(value) &&
    (field.constraints?.min === undefined || value >= field.constraints.min) && (field.constraints?.max === undefined || value <= field.constraints.max)
  if (field.type === 'boolean') return typeof value === 'boolean'
  if (typeof value !== 'string') return false
  if (field.type === 'select') return field.constraints?.options?.includes(value) ?? false
  if (field.type === 'color') return !value || CSS.supports('color', value)
  return true
}

/** Recreate only the editor host on theme change: 0.8.1 caches the initial color palette. */
export function VpEditor() {
  // The palette must be in CSSOM before appFields/provider capture their initial colors.
  const style = document.createElement('style')
  style.textContent = EDITOR_CSS
  document.head.append(style)
  onCleanup(() => style.remove())
  let storage: StoragePort | undefined
  try { storage = window.localStorage } catch { /* unavailable in some WebViews */ }
  const current = document.documentElement.dataset.theme
  const initial = readThemeLibrary(storage, isThemeId(current) ? current : 'contrast-dark')
  const [library, setLibrary] = createSignal(initial)
  const [section, setSection] = createSignal<EditorSection>('THEME')
  const [picking, setPicking] = createSignal(false)
  const [storageError, setStorageError] = createSignal(false)
  const [mount, setMount] = createSignal({ id: initial.selected, base: themeBase(initial), open: false })
  let activeHost: EditorHost | undefined

  const persist = (next: ThemeLibrary) => {
    setLibrary(next)
    setStorageError(!writeThemeLibrary(storage, next))
  }
  const prepare = (base: ThemeId) => {
    clearEditorStyles()
    clearRuntimeStorage(storage)
    document.documentElement.dataset.theme = base
    document.documentElement.dataset.vpEditorTheme = base
  }
  prepare(themeBase(initial))

  const activate = (id: string) => {
    const next = { ...library(), selected: id }
    if (!isThemeId(id) && !next.custom.some(theme => theme.id === id)) return
    const open = activeHost?.mode() === 'on'
    setPicking(false)
    persist(next)
    prepare(themeBase(next))
    setMount({ id, base: themeBase(next), open })
  }
  const save = (name: string): string | undefined => {
    const trimmed = name.trim()
    if (!trimmed || trimmed.length > 80) return '名前を 1〜80 文字で入力してください。'
    if (library().custom.some(theme => theme.name === trimmed)) return '同じ名前があります。別の名前にするか「変更を保存」を使ってください。'
    const id = `custom:${crypto.randomUUID()}`
    const values = { ...themeValues(library()) }
    const custom = { id, name: trimmed, base: themeBase(library()), values }
    persist({ ...library(), custom: [...library().custom, custom], drafts: { ...library().drafts, [id]: values } })
    activate(id)
    return undefined
  }
  const updateSaved = () => persist({ ...library(), custom: library().custom.map(theme => theme.id === library().selected ? { ...theme, values: { ...themeValues(library()) } } : theme) })
  const restoreSaved = () => {
    const id = library().selected
    const values = library().custom.find(theme => theme.id === id)?.values ?? {}
    persist({ ...library(), drafts: { ...library().drafts, [id]: { ...values } } })
    activate(id)
  }
  return <>
    <Show when={mount()} keyed>{profile => <EditorHostProvider config={{
      initialMode: profile.open ? 'on' : 'off', localStorageNamespace: RUNTIME_NAMESPACE,
      // The provider's picker is active only during an explicit pick gesture.
      selectionRoot: () => picking() ? document.body : null,
    }}>
      <AppTokenBinds />
      <EditorSession library={library} persist={persist} picking={picking} setPicking={setPicking}
        section={section} setSection={setSection} storageError={storageError}
        activate={activate} save={save} updateSaved={updateSaved} restoreSaved={restoreSaved}
        expose={host => { activeHost = host }} />
    </EditorHostProvider>}</Show>
  </>
}

type SessionProps = {
  library: () => ThemeLibrary; persist(library: ThemeLibrary): void;
  picking: () => boolean; setPicking(picking: boolean): void;
  section: () => EditorSection; setSection(section: EditorSection): void;
  storageError: () => boolean; activate(id: string): void; save(name: string): string | undefined;
  updateSaved(): void; restoreSaved(): void; expose(host: EditorHost): void;
}

function EditorSession(props: SessionProps) {
  const host = useEditorHost()
  const resolver = createComponentFieldResolver({ host, owner: getOwner(), root: () => document.body })
  const bridgeWindow = window as unknown as { vpEditorHost?: EditorHost }
  bridgeWindow.vpEditorHost = host
  props.expose(host)
  onCleanup(() => { if (bridgeWindow.vpEditorHost === host) delete bridgeWindow.vpEditorHost })
  const seen = new Set<string>()
  let restoring = false
  const restoreFields = () => {
    const fields = host.fields()
    untrack(() => {
      const values = themeValues(props.library())
      restoring = true
      try {
        const added = fields.filter(field => !seen.has(field.id))
        // Initialize the whole family first: a default surface field removes its inline color.
        // Doing this between saved hue/color writes would erase an earlier restored value.
        for (const field of added) {
          seen.add(field.id)
          host.setValue(field.id, field.initial)
        }
        for (const field of added) {
          const value = values[field.id]
          if (validValue(field, value)) host.setValue(field.id, value)
        }
      } finally { restoring = false }
    })
  }
  createEffect(restoreFields)
  onCleanup(host.onAnyChange((id, value) => {
    if (restoring) return
    const field = host.getField(id)
    if (!field || !validValue(field, value)) return
    const library = props.library()
    const values: Values = { ...themeValues(library) }
    if (value === field.initial) delete values[id]
    else values[id] = value as Values[string]
    if (id.startsWith('color.surface.')) {
      // Hue/chroma and individual surface controls share CSS properties. Capture their
      // effective colors so either edit order (including a per-color reset) round-trips.
      const adjusted = ['color.surface.hue', 'color.surface.chroma'].some(key => host.getValue(key) !== host.getField(key)?.initial)
      const computed = getComputedStyle(document.documentElement)
      const colors = host.fields().filter(item => item.id.startsWith('color.surface.') && item.type === 'color')
        .map(item => ({ field: item, value: computed.getPropertyValue(`--${item.id.replaceAll('.', '-')}`).trim() }))
      for (const color of colors) {
        if (!color.value) continue
        host.setValue(color.field.id, color.value, { silent: true })
        if (!adjusted && color.value === color.field.initial) delete values[color.field.id]
        else values[color.field.id] = color.value
      }
    }
    props.persist({ ...library, drafts: { ...library.drafts, [library.selected]: values } })
  }))

  // Restore lazy component fields as their elements appear, without requiring a pick first.
  onMount(() => {
    const restoreComponents = () => {
      const values = themeValues(props.library())
      const pending = resolver.knobs().filter(knob => Object.hasOwn(values, knob.id) && !host.getField(knob.id))
      for (const id of new Set(pending.map(knob => knob.component))) {
        if (resolver.components().some(component => component.id === id)) resolver.selectComponent(id)
      }
    }
    restoreComponents()
    const observer = new MutationObserver(restoreComponents)
    observer.observe(document.body, { childList: true, subtree: true })
    onCleanup(() => observer.disconnect())
  })
  createEffect(() => {
    if (host.mode() === 'off') props.setPicking(false)
    if (host.selection()) {
      props.setPicking(false)
      host.setHover(null)
    }
  })
  onMount(() => {
    const cancelPick = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && props.picking()) {
        event.preventDefault()
        event.stopImmediatePropagation()
        props.setPicking(false)
        host.setHover(null)
      }
    }
    window.addEventListener('keydown', cancelPick, true)
    onCleanup(() => window.removeEventListener('keydown', cancelPick, true))
  })
  return <EditorPanel {...props} host={host} resolver={resolver} />
}
