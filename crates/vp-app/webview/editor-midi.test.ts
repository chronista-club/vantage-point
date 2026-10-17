/** mem_1CfvxBJqJvVjZk5FEqZ98b — Editor の MIDI タブ（LPD8 ピンの色・未選択の明るさ）。保存先は session（IPC）。 */
import { fileURLToPath } from 'node:url'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window } from 'happy-dom'
import { afterEach, beforeAll, expect, it } from 'vitest'
import { createRoot } from 'solid-js'
import { DEFAULT_PIN_COLORS, defaultPins, withDimGain } from './midi-pins'
import { createMidiSettings } from './editor/midi-settings'

let bundle: string
type Win = Window & { vpEditorHost: any; dispose(): void; sent: { t: string; settings?: string }[] }
const windows: Win[] = []
beforeAll(async () => {
  const result = await build({ stdin: { contents: `
    import { render } from 'solid-js/web';
    import { createComponent } from 'solid-js';
    import { VpEditor } from './editor/VpEditor';
    window.dispose = render(() => createComponent(VpEditor, {}), document.getElementById('editor-root'));
  `, loader: 'tsx', resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle: true, write: false, format: 'iife', platform: 'browser', plugins: [solidPlugin()], define: { 'process.env.NODE_ENV': '"production"' } })
  bundle = result.outputFiles[0].text
})
afterEach(async () => { for (const win of windows.splice(0)) { win.dispose?.(); await win.happyDOM.abort() } })

const a = { path: '/vp', address: 'vp/root' }
function fixture(native?: unknown) {
  const win = new Window({ url: 'http://localhost:12889', settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as Win
  windows.push(win)
  win.sent = []
  ;(win as any).ipc = { postMessage: (m: string) => win.sent.push(JSON.parse(m)) }
  if (native !== undefined) (win as any).__vpMidiSettings = native
  win.document.documentElement.dataset.theme = 'contrast-dark'
  win.document.body.innerHTML = '<div id="editor-root"></div>'
  win.eval(bundle)
  win.vpEditorHost.enable()
  return win
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0))
const q = (win: Window, sel: string) => win.document.querySelector(sel) as any
function openMidi(win: Window) {
  const tab = Array.from(win.document.querySelectorAll('[role=tab]')).find(el => el.textContent === 'MIDI')!
  tab.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
}
function input(win: Window, el: any, value: string) {
  el.value = value
  el.dispatchEvent(new win.Event('input', { bubbles: true }))
}
const native = (win: Window, detail: unknown) => win.dispatchEvent(new win.CustomEvent('vp:midi-settings', { detail: detail as object | null }))
const lastSaved = (win: Win) => JSON.parse(win.sent.filter(m => m.t === 'midi:navigation').at(-1)!.settings!)

it('shows 8 pin colors and the unselected brightness without a selection, restoring an old 4-pin session', async () => {
  const win = fixture({ slots: [a, null, null, null], colors: ['#112233', '#445566', '#778899', '#aabbcc'] })
  await tick()
  openMidi(win)
  expect(win.vpEditorHost.selection()).toBeNull()
  for (let i = 0; i < 8; i++) {
    expect(q(win, `[data-midi-field="pin.${i + 1}.color"] input[type=text]`).value)
      .toBe(['#112233', '#445566', '#778899', '#aabbcc', ...DEFAULT_PIN_COLORS.slice(4)][i])
  }
  expect(q(win, '[data-midi-field="dim"] input[type=number]').value).toBe('25')
  expect(win.sent).toEqual([]) // 開いただけでは保存しない
})

it('does not write before the window settings arrive (would overwrite saved pins with defaults)', async () => {
  const win = fixture()
  await tick()
  openMidi(win)
  const dim = q(win, '[data-midi-field="dim"] input[type=number]')
  expect(dim.disabled).toBe(true)
  input(win, dim, '60')
  expect(win.sent).toEqual([])
  native(win, null) // 新しい window（保存なし）
  await tick()
  expect(q(win, '[data-midi-field="dim"] input[type=number]').disabled).toBe(false)
})

it('sends a color change over midi:navigation, keeping pins and brightness, and never into the theme store', async () => {
  const win = fixture({ slots: [a, null, null, null, null, null, null, null], colors: [...DEFAULT_PIN_COLORS], dimGain: 0.4 })
  await tick()
  openMidi(win)
  input(win, q(win, '[data-midi-field="pin.6.color"] input[type=text]'), '#010203')
  const saved = lastSaved(win)
  expect(saved.colors[5]).toBe('#010203')
  expect(saved.slots[0]).toEqual(a)
  expect(saved.dimGain).toBe(0.4)
  input(win, q(win, '[data-midi-field="pin.6.color"] input[type=text]'), 'not-a-color')
  expect(win.sent.filter(m => m.t === 'midi:navigation')).toHaveLength(1)
  expect(win.localStorage.getItem('vp:editor:themes:v1') ?? '').not.toContain('#010203')
})

it('sends the brightness as dimGain and resets each item to its default', async () => {
  const win = fixture({ slots: Array(8).fill(null), colors: [...DEFAULT_PIN_COLORS], dimGain: 0.25 })
  await tick()
  openMidi(win)
  const reset = () => q(win, '[data-midi-field="dim"] .vp-editor-reset')
  expect(reset().disabled).toBe(true)
  input(win, q(win, '[data-midi-field="dim"] input[type=number]'), '40')
  expect(lastSaved(win).dimGain).toBe(0.4)
  expect(reset().disabled).toBe(false)
  reset().dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
  expect(lastSaved(win).dimGain).toBe(0.25)
  input(win, q(win, '[data-midi-field="pin.1.color"] input[type=text]'), '#000000')
  q(win, '[data-midi-field="pin.1.color"] .vp-editor-reset').dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
  expect(lastSaved(win).colors[0]).toBe(DEFAULT_PIN_COLORS[0])
})

it('follows changes made in the sidebar, and ignores a late echo of an older Editor value', async () => {
  const base = { slots: Array(8).fill(null), colors: [...DEFAULT_PIN_COLORS], dimGain: 0.25 }
  const win = fixture(base)
  await tick()
  openMidi(win)
  const dim = () => q(win, '[data-midi-field="dim"] input[type=number]').value
  // sidebar の右クリック「色の変更」→ Rust → 配り直し
  native(win, { ...base, colors: ['#abcdef', ...base.colors.slice(1)] })
  await tick()
  expect(q(win, '[data-midi-field="pin.1.color"] input[type=text]').value).toBe('#abcdef')
  // スライダーを動かし続けている間に、古い値の echo が後から届く
  input(win, q(win, '[data-midi-field="dim"] input[type=number]'), '40')
  input(win, q(win, '[data-midi-field="dim"] input[type=number]'), '50')
  native(win, JSON.parse(win.sent[0].settings!))
  await tick()
  expect(dim()).toBe('50')
  native(win, JSON.parse(win.sent[1].settings!))
  await tick()
  expect(dim()).toBe('50')
  native(win, { ...base, dimGain: 0.1 })
  await tick()
  expect(dim()).toBe('10')
})

it('the store itself refuses to write until settings arrive, even if a control slips through', () => {
  const sent: string[] = []
  const win = Object.assign(new EventTarget(), { ipc: { postMessage: (m: string) => sent.push(m) } }) as any
  createRoot(dispose => {
    const midi = createMidiSettings(win)
    midi.update(withDimGain(defaultPins(), 0.9))
    expect(sent).toEqual([])
    expect(midi.settings().dimGain).toBe(0.25)
    win.dispatchEvent(new CustomEvent('vp:midi-settings', { detail: null }))
    midi.update(withDimGain(midi.settings(), 0.9))
    expect(JSON.parse(JSON.parse(sent[0]).settings).dimGain).toBe(0.9)
    dispose()
  })
})
