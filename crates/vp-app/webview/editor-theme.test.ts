/** mem_1CfjZnPVNMbQrVauwF6W4C — app Editor, theme isolation and named persistence. */
import { fileURLToPath } from 'node:url'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window } from 'happy-dom'
import { afterEach, beforeAll, expect, it } from 'vitest'

let bundle: string
const windows: Array<Window & { dispose?: () => void }> = []
beforeAll(async () => {
  const result = await build({ stdin: { contents: `
    import { render } from 'solid-js/web';
    import { createComponent } from 'solid-js';
    import { VpEditor } from './editor/VpEditor';
    window.dispose = render(() => createComponent(VpEditor, {}), document.getElementById('editor-root'));
  `, loader: 'tsx', resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle: true, write: false, format: 'iife', platform: 'browser', plugins: [solidPlugin()], define: { 'process.env.NODE_ENV': '"production"' } })
  bundle = result.outputFiles[0].text
})

function fixture(storage: Record<string, string> = {}) {
  const win = new Window({ url: 'http://localhost:12889', settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as Window & { vpEditorHost: any; dispose(): void }
  windows.push(win)
  for (const [key, value] of Object.entries(storage)) win.localStorage.setItem(key, value)
  win.document.documentElement.dataset.theme = 'contrast-dark'
  win.document.head.innerHTML = `<style>
    :root { --color-surface-bg-base: #111111; --color-surface-surface: oklch(0.2 0.05 280); --color-text-primary: #eeeeee; --color-brand-primary: oklch(70% .1 140); }
    :root[data-theme="mint-light"] { --color-surface-bg-base: #fafafa; --color-surface-surface: #ffffff; --color-text-primary: #222222; --color-brand-primary: oklch(50% .1 160); }
    .creo-btn { padding: var(--_btn__pad-x, 12px); }
  </style>`
  win.document.body.innerHTML = '<main><button id="sample" class="creo-btn">Sample</button></main><div id="editor-root"></div>'
  win.eval(bundle)
  win.vpEditorHost.enable()
  return win
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0))
function button(win: Window, text: string) {
  const el = Array.from(win.document.querySelectorAll('button')).find(el => el.textContent?.trim() === text)
  expect(el, `button: ${text}`).toBeDefined()
  return el!
}
function click(win: Window, el: { dispatchEvent(event: any): boolean }) {
  el.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
}
afterEach(async () => { for (const win of windows.splice(0)) { win.dispose?.(); await win.happyDOM.abort() } })

it('offers horizontal THEME / SIDEBAR / CHAT / COMPONENT with controls grouped by their target', async () => {
  const win = fixture()
  await tick()
  expect(Array.from(win.document.querySelectorAll('[role="tab"]')).map(el => el.textContent)).toEqual(['THEME', 'SIDEBAR', 'CHAT', 'COMPONENT'])
  expect(win.document.querySelector('[role=tablist]')?.getAttribute('aria-orientation')).toBe('horizontal')
  expect(win.document.querySelector('[role=tab][aria-selected=true]')?.textContent).toBe('THEME')
  button(win, 'THEME').dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }))
  expect(win.document.querySelector('[role=tab][aria-selected=true]')?.textContent).toBe('SIDEBAR')
  click(win, button(win, 'SIDEBAR'))
  expect(win.vpEditorHost.selection()).toBeNull()
  expect(win.document.querySelector('[data-field="sb.text.base"]')).toBeNull()
  expect(win.document.querySelector('[data-field="sb.text.hint"]')).not.toBeNull()
  expect(win.document.querySelector('[data-field="sb.selection.bg"]')).not.toBeNull()
  expect(win.document.querySelector('[data-field="sb.activity.working"]')).not.toBeNull()
  expect(win.vpEditorHost.getField('sb.conn.photon.period')).toBeUndefined()
  click(win, button(win, 'THEME'))
  expect(win.document.querySelector('[data-field="typography.scale"]')).not.toBeNull()
  expect(win.document.querySelector('[data-field="typography.size.l"]')).not.toBeNull()
  for (const id of ['typography.size.xs', 'typography.size.s', 'typography.size.m', 'typography.size.xl', 'layout.gap.sibling']) {
    expect(win.document.querySelector(`[data-field="${id}"]`)).toBeNull()
    expect(win.vpEditorHost.getField(id)).toBeDefined() // Compatibility for existing saved themes.
  }
  expect(win.document.querySelector('[data-field="chat.text.body"]')).toBeNull()
  expect(win.document.querySelector('[data-field="shell.resizer.breathe.ms"]')).not.toBeNull()
  click(win, button(win, 'SIDEBAR'))
  button(win, 'SIDEBAR').dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }))
  expect(win.document.querySelector('[role=tab][aria-selected=true]')?.textContent).toBe('CHAT')
  for (const id of ['body', 'user', 'tool', 'meta', 'micro']) {
    expect(win.document.querySelector(`[data-field="chat.text.${id}"]`)).not.toBeNull()
  }
  expect(win.document.querySelector('[data-field="typography.scale"]')).toBeNull()
  expect(win.document.querySelector('[data-field="sb.text.hint"]')).toBeNull()
  button(win, 'CHAT').dispatchEvent(new win.KeyboardEvent('keydown', { key: 'End', bubbles: true }))
  expect(win.document.querySelector('[role=tab][aria-selected=true]')?.textContent).toBe('COMPONENT')
  button(win, 'COMPONENT').dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }))
  expect(win.document.querySelector('[role=tab][aria-selected=true]')?.textContent).toBe('THEME')
})

it('saves a named theme, isolates A → B → A, and restores after reload', async () => {
  const win = fixture()
  await tick()
  click(win, button(win, 'THEME'))
  const picker = win.document.querySelector('[aria-label="テーマを選ぶ"]') as any
  expect(picker).not.toBeNull()
  win.vpEditorHost.setValue('sb.selection.bg', '#334455')
  const name = win.document.querySelector('[aria-label="カスタムテーマ名"]') as any
  name.value = '夜の作業台'
  name.dispatchEvent(new win.Event('input', { bubbles: true }))
  click(win, button(win, '名前を付けて保存'))
  await tick()
  const customId = (win.document.querySelector('[aria-label="テーマを選ぶ"]') as any).value
  expect(customId).toMatch(/^custom:/)
  const changeTheme = async (id: string) => {
    const select = win.document.querySelector('[aria-label="テーマを選ぶ"]') as any
    select.value = id
    select.dispatchEvent(new win.Event('change', { bubbles: true }))
    await tick()
  }
  await changeTheme('mint-light')
  expect(win.document.documentElement.dataset.theme).toBe('mint-light')
  expect(win.vpEditorHost.getValue('sb.selection.bg')).not.toBe('#334455')
  win.vpEditorHost.setValue('sb.selection.bg', '#ccbbaa')
  await changeTheme(customId)
  expect(win.vpEditorHost.getValue('sb.selection.bg')).toBe('#334455')
  const storage = Object.fromEntries(Array.from({ length: win.localStorage.length }, (_, i) => {
    const key = win.localStorage.key(i)!
    return [key, win.localStorage.getItem(key)!]
  }))
  const restored = fixture(storage)
  await tick()
  expect(restored.vpEditorHost.getValue('sb.selection.bg')).toBe('#334455')
  expect(restored.document.documentElement.style.getPropertyValue('--sb-selection-bg')).toBe('#334455')
})

it('starts picking explicitly and lets normal content clicks through after choosing', async () => {
  const win = fixture()
  await tick()
  let clicks = 0
  win.document.querySelector('#sample')!.addEventListener('click', () => clicks++)
  click(win, win.document.querySelector('#sample')!)
  expect(clicks).toBe(1)
  click(win, button(win, 'COMPONENT'))
  click(win, button(win, '画面から選ぶ'))
  click(win, win.document.querySelector('#sample')!)
  await tick()
  expect(clicks).toBe(1)
  expect(win.vpEditorHost.selection()?.componentId).toBe('btn')
  expect(win.document.body.textContent).toContain('同じ種類のすべての部品')
  click(win, win.document.querySelector('#sample')!)
  expect(clicks).toBe(2)
})

it('ignores broken storage and invalid field values while keeping legacy user adjustments', async () => {
  const broken = fixture({ 'vp:editor:themes:v1': '{broken' })
  await tick()
  expect(broken.vpEditorHost.getValue('sb.text.base')).toBe(13)
  expect(broken.document.querySelector('[role="dialog"]')).not.toBeNull()
  const legacyKey = '@chronista-club/creo-ui-editor-host:field:typography.scale'
  const legacy = fixture({ [legacyKey]: '1.12' })
  await tick()
  expect(legacy.vpEditorHost.getValue('typography.scale')).toBe(1.12)
  expect(legacy.localStorage.getItem(legacyKey)).toBe('1.12')
  const invalid = fixture({ 'vp:editor:themes:v1': JSON.stringify({ version: 1, selected: 'contrast-dark', custom: [], drafts: { 'contrast-dark': { 'sb.text.base': -200, 'sb.selection.bg': 123 } } }) })
  await tick()
  expect(invalid.vpEditorHost.getValue('sb.text.base')).toBe(13)
  expect(invalid.vpEditorHost.getValue('sb.selection.bg')).toBe('#151c27')
})

it('restores a saved component adjustment when that component appears later', async () => {
  const win = fixture({ 'vp:editor:themes:v1': JSON.stringify({ version: 1, selected: 'contrast-dark', custom: [], drafts: { 'contrast-dark': { 'late.pad.x': 24 } } }) })
  const style = win.document.createElement('style')
  style.textContent = '.creo-late { padding: var(--_late__pad-x, 12px); }'
  win.document.head.append(style)
  await tick()
  expect(win.vpEditorHost.getField('late.pad.x')).toBeUndefined()
  const component = win.document.createElement('button')
  component.className = 'creo-late'
  win.document.querySelector('main')!.append(component)
  await tick()
  expect(win.vpEditorHost.getValue('late.pad.x')).toBe(24)
  expect(win.document.documentElement.style.getPropertyValue('--_late__pad-x')).toBe('24px')
})

it('cancels pick with Escape, then clears selection and closes without losing adjustments', async () => {
  const win = fixture()
  await tick()
  const escape = () => win.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
  click(win, button(win, 'COMPONENT'))
  click(win, button(win, '画面から選ぶ'))
  escape()
  expect(win.vpEditorHost.mode()).toBe('on')
  expect(button(win, '画面から選ぶ')).toBeDefined()
  click(win, button(win, 'COMPONENT'))
  click(win, button(win, '画面から選ぶ'))
  click(win, win.document.querySelector('#sample')!)
  await tick()
  win.vpEditorHost.setValue('sb.text.base', 15)
  escape()
  expect(win.vpEditorHost.selection()).toBeNull()
  expect(win.vpEditorHost.mode()).toBe('on')
  escape()
  expect(win.vpEditorHost.mode()).toBe('off')
  win.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'E', ctrlKey: true, shiftKey: true, bubbles: true }))
  expect(win.vpEditorHost.mode()).toBe('on')
  expect(win.vpEditorHost.getValue('sb.text.base')).toBe(15)
})

it('adapts activity colors for a light sidebar and restores the dark defaults', async () => {
  const win = fixture()
  await tick()
  const dark = win.vpEditorHost.getValue('sb.activity.working')
  click(win, button(win, 'THEME'))
  const picker = win.document.querySelector('[aria-label="テーマを選ぶ"]') as any
  picker.value = 'mint-light'
  picker.dispatchEvent(new win.Event('change', { bubbles: true }))
  await tick()
  expect(win.vpEditorHost.getValue('sb.activity.working')).not.toBe(dark)
  const next = win.document.querySelector('[aria-label="テーマを選ぶ"]') as any
  next.value = 'contrast-dark'
  next.dispatchEvent(new win.Event('change', { bubbles: true }))
  await tick()
  expect(win.vpEditorHost.getValue('sb.activity.working')).toBe(dark)
})

it('uses the restored light palette as the sidebar controls initial values on first mount', async () => {
  const win = fixture({ 'vp:editor:themes:v1': JSON.stringify({ version: 1, selected: 'mint-light', custom: [], drafts: {} }) })
  await tick()
  expect(win.vpEditorHost.getValue('sb.activity.working')).toBe('#35638d')
  expect(win.vpEditorHost.getValue('sb.background')).toBe('#fafafa')
})

it('applies a typed color during input so the preview does not wait for blur', async () => {
  const win = fixture()
  await tick()
  click(win, button(win, 'SIDEBAR'))
  const input = win.document.getElementById('vp-field-sb.background') as any
  input.value = '#334455'
  input.dispatchEvent(new win.Event('input', { bubbles: true }))
  expect(win.vpEditorHost.getValue('sb.background')).toBe('#334455')
})


it('restores the rendered surface palette after hue, individual color and reset operations', async () => {
  const win = fixture()
  await tick()
  win.vpEditorHost.setValue('color.surface.hue', 180)
  const palette = () => win.document.documentElement.style.getPropertyValue('--color-surface-surface')
  expect(palette()).toContain('180')
  const snapshot = () => Object.fromEntries(Array.from({ length: win.localStorage.length }, (_, i) => {
    const key = win.localStorage.key(i)!
    return [key, win.localStorage.getItem(key)!]
  }))
  const restored = fixture(snapshot())
  await tick()
  expect(restored.document.documentElement.style.getPropertyValue('--color-surface-surface')).toBe(palette())
  win.vpEditorHost.setValue('color.surface.surface', '#abcdef')
  win.vpEditorHost.setValue('color.surface.hue', 220)
  const edited = fixture(snapshot())
  await tick()
  expect(edited.document.documentElement.style.getPropertyValue('--color-surface-surface')).toBe(palette())
  win.vpEditorHost.setValue('color.surface.surface', win.vpEditorHost.getField('color.surface.surface').initial)
  const reset = fixture(snapshot())
  await tick()
  expect(reset.document.documentElement.style.getPropertyValue('--color-surface-surface')).toBe(palette())
})


it('keeps a picked component input mounted and applies its value to every instance', async () => {
  const win = fixture()
  await tick()
  click(win, button(win, 'COMPONENT'))
  click(win, button(win, '画面から選ぶ'))
  click(win, win.document.querySelector('#sample')!)
  await tick()
  const input = win.document.getElementById('vp-field-btn.pad.x') as any
  win.vpEditorHost.select({ ...win.vpEditorHost.selection(), rect: win.document.querySelector('#sample')!.getBoundingClientRect() })
  expect(win.document.getElementById('vp-field-btn.pad.x')).toBe(input)
  input.value = '28'
  input.dispatchEvent(new win.Event('input', { bubbles: true }))
  await tick()
  expect(win.vpEditorHost.getValue('btn.pad.x')).toBe(28)
  expect(win.document.documentElement.style.getPropertyValue('--_btn__pad-x')).toBe('28px')
  expect(win.document.getElementById('vp-field-btn.pad.x')).toBe(input)
})
