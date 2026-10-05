import { afterEach, beforeAll, expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window } from 'happy-dom'
import { fileURLToPath } from 'node:url'

let bundle: string
const windows: Window[] = []
beforeAll(async () => {
  const result = await build({ stdin: { contents: `
    import { render } from 'solid-js/web';
    import { createComponent } from 'solid-js';
    import { Shell } from './src/sidebar/Shell';
    import { applySidebarState, emptyState } from './src/sidebar/store';
    window.sent = [];
    window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) };
    const lane = (name, flow_state) => ({ address: { repo: 'project', name, key: 'project/' + name }, state: 'running', agent: 'codex', pid: 42, cwd: '/project', branch: 'wip/' + name, flow_state, sub_status: null, sessions: null });
    const base = { ...emptyState(), processes: [{ path: '/project', name: 'Project', state: 'running', expanded: true, port: 123 }], lanes_by_repo: { '/project': [lane('main', 'working'), lane('one', 'working'), lane('two', 'awaiting_user')] }, active_lane_address: 'project/main' };
    window.setRepo = patch => applySidebarState({ ...base, processes: [{ ...base.processes[0], ...patch }] });
    window.setRepo({});
    window.dispose = render(() => createComponent(Shell, {}), document.getElementById('sidebar-root'));
  `, loader: 'tsx', resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle: true, write: false, format: 'iife', platform: 'browser', plugins: [solidPlugin()], define: { 'process.env.NODE_ENV': '"production"' } })
  bundle = result.outputFiles[0].text
})
function fixture() {
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as Window & { sent: any[]; setRepo(patch: object): void; dispose(): void }
  windows.push(win)
  win.document.body.innerHTML = '<div id="sidebar-root"></div><div id="host"></div>'
  win.eval(bundle)
  return win
}
afterEach(async () => { for (const win of windows.splice(0)) await win.happyDOM.abort() })

it('keeps one project root, Japanese states and selectable lanes', () => {
  const win = fixture()
  const root = win.document.querySelector('.vp-proj-summary .vp-lane-row')!
  expect(root).not.toBeNull()
  expect(root.textContent).toContain('Project')
  expect(root.textContent).toContain('作業中')
  expect(win.document.querySelectorAll('.vp-lane-row')).toHaveLength(3)
  expect(win.document.querySelectorAll('.vp-lane-dot,.vp-lane-icon')).toHaveLength(0)
  expect(win.document.querySelector('.vp-proj-content')!.textContent).toContain('確認待ち')
  root.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
  expect(win.sent.at(-1)).toEqual({ t: 'lane:select', path: '/project', address: 'project/main' })
})
it('collapses children into one point per sub lane and preserves root selection', () => {
  const win = fixture()
  const toggle = win.document.querySelector('[aria-label="Project を折りたたむ"]')!
  expect(toggle).not.toBeNull()
  toggle.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
  expect(win.sent.at(-1)).toEqual({ t: 'process:toggle', path: '/project', expanded: false })
  win.setRepo({ expanded: false })
  expect(win.document.querySelector('.vp-proj-content')!.hasAttribute('hidden')).toBe(true)
  expect(win.document.querySelectorAll('.vp-sub-point')).toHaveLength(2)
  expect(win.document.querySelector('.vp-sub-points')!.getAttribute('aria-label')).toBe('sub lane 2個')
  win.setRepo({ expanded: true })
  expect(win.document.querySelectorAll('.vp-sub-point')).toHaveLength(0)
})
it('automatically groups stopped projects and returns starting ones to CURRENTs', () => {
  const win = fixture()
  expect(win.document.querySelector('[data-section="currents"] .vp-proj')).not.toBeNull()
  win.setRepo({ state: 'stopped', port: null })
  expect(win.document.querySelector('[data-section="currents"] .vp-proj')).toBeNull()
  expect(win.document.querySelector('[data-section="stopped"] .vp-proj')).not.toBeNull()
  win.setRepo({ state: 'starting' })
  expect(win.document.querySelector('[data-section="currents"] .vp-proj')).not.toBeNull()
})
it('keeps both session and project operations on the unified root menu', () => {
  const win = fixture()
  win.document.querySelector('.vp-proj-summary .vp-lane-row')!.dispatchEvent(new win.MouseEvent('contextmenu', { bubbles: true }))
  const menu = win.document.querySelector('.vp-ctx-menu')!
  expect(menu.textContent).toContain('Restart Session')
  expect(menu.textContent).toContain('Stop repo')
})
it('retains the ACTIONS capture draft across folding and keeps Creo ID out of the sidebar', () => {
  const win = fixture()
  const details = win.document.querySelector('details.vp-act-buckets') as any
  expect(details).not.toBeNull()
  const input = details.querySelector('textarea')
  input.value = '未送信のメモ'
  input.dispatchEvent(new win.Event('input', { bubbles: true }))
  details.open = false
  details.open = true
  expect(details.querySelector('textarea')).toBe(input)
  expect(input.value).toBe('未送信のメモ')
  expect(win.document.querySelector('.vp-creo-zone .vp-creo-id')).toBeNull()
})
