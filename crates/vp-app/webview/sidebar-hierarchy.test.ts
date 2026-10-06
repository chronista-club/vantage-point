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
    import { installConsole } from './console';
    import { runCaptureMode } from './src/sidebar/actions/handlers';
    import { applySidebarState, emptyState } from './src/sidebar/store';
    window.sent = [];
    window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) };
    const lane = (name, flow_state) => ({ address: { repo: 'project', name, key: 'project/' + name }, state: 'running', agent: 'codex', pid: 42, cwd: '/project', branch: 'wip/' + name, flow_state, sub_status: null, sessions: null });
    const base = { ...emptyState(), processes: [{ path: '/project', name: 'Project', state: 'running', expanded: true, port: 123 }], lanes_by_repo: { '/project': [lane('main', 'working'), lane('one', 'working'), lane('two', 'awaiting_user')] }, active_lane_address: 'project/main' };
    window.setRepo = patch => applySidebarState({ ...base, processes: [{ ...base.processes[0], ...patch }] });
    window.setRepo({});
    window.setResponse = (timestamp) => applySidebarState({ ...base, lanes_by_repo: { '/project': [{ ...base.lanes_by_repo['/project'][0], sessions: { root: 1, focused: 1, sessions: [{key: 1, agent:'codex', last_response_at: timestamp}, {key:2, agent:'codex', last_response_at: timestamp}] } }] } });
    window.con = installConsole();
    window.capture = runCaptureMode;
    window.dispose = render(() => createComponent(Shell, {}), document.getElementById('sidebar-root'));
  `, loader: 'tsx', resolveDir: fileURLToPath(new URL('.', import.meta.url)) }, bundle: true, write: false, format: 'iife', platform: 'browser', plugins: [solidPlugin()], define: { 'process.env.NODE_ENV': '"production"' } })
  bundle = result.outputFiles[0].text
})
function fixture() {
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as Window & { sent: any[]; setRepo(patch: object): void; dispose(): void; capture(): void; setResponse(timestamp: number | null): void; con: any }
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
  const stopped = win.document.querySelector('details[data-section="stopped"]') as any
  expect(stopped).not.toBeNull()
  expect(stopped.open).toBe(false)
  expect(stopped.querySelector('summary').textContent).toContain('PAUSED 1')
  stopped.open = true
  expect(stopped.querySelector('[aria-label="Project を再開"]')).not.toBeNull()
  win.setRepo({ state: 'stopped', port: null })
  expect(stopped.open).toBe(true)
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

it('reveals collapsed ACTIONS before keyboard capture focuses its memo', () => {
  const win = fixture()
  const details = win.document.querySelector('details.vp-act-buckets') as any
  details.open = false
  win.capture()
  win.dispatchEvent(new win.KeyboardEvent('keydown', { key: '1', bubbles: true }))
  expect(details.open).toBe(true)
  expect(win.document.activeElement).toBe(details.querySelector('textarea'))
})

it('pauses the whole project and offers resume after it stops', () => {
  const win = fixture()
  const pause = win.document.querySelector('[aria-label="Project を停止（sub lane を含む）"]')!
  expect(pause).not.toBeNull()
  pause.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
  expect(win.sent.at(-1)).toEqual({ t: 'process:stop', path: '/project' })
  expect(win.sent.some(m => m.t === 'lane:select')).toBe(false)
  win.setRepo({ state: 'stopped', port: null })
  const resume = win.document.querySelector('[aria-label="Project を再開"]')!
  expect(resume.closest('[data-section="stopped"]')).not.toBeNull()
  resume.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
  expect(win.sent.at(-1)).toEqual({ t: 'process:restart', path: '/project' })
})

it('shows pastel response freshness per session without inventing unknown times', () => {
  const win = fixture()
  expect(win.document.querySelectorAll('.vp-response-point')).toHaveLength(0)
  win.setResponse(Date.now() - 3 * 60_000)
  const points = win.document.querySelectorAll('.vp-response-point')
  expect(points).toHaveLength(2)
  expect(points[0].getAttribute('title')).toMatch(/最終応答から[23]分/)
  expect(points[0].getAttribute('aria-label')).toBe(points[0].getAttribute('title'))
  expect(points[0].getAttribute('data-freshness')).toBe('fresh')
  win.setResponse(Date.now() - 2 * 60 * 60_000)
  expect(win.document.querySelector('.vp-response-point')!.getAttribute('data-freshness')).toBe('old')
  win.setResponse(null)
  expect(win.document.querySelectorAll('.vp-response-point')).toHaveLength(0)
})

it('replaces the project arrow with an activity point and keeps Result visible', () => {
  const win = fixture()
  const point = () => win.document.querySelector('.vp-proj-toggle .vp-activity-point')!
  expect(point()).not.toBeNull()
  expect(win.document.querySelector('.vp-proj-toggle iconify-icon')).toBeNull()
  win.con.handleEvent('project/main', {kind:'thought_chunk', text:'thinking'}, 1)
  expect(point().getAttribute('data-activity')).toBe('thinking')
  win.con.handleEvent('project/main', {kind:'tool_call', id:'tool', name:'Read', input:{}}, 1)
  expect(point().getAttribute('data-activity')).toBe('working')
  win.con.handleEvent('project/main', {kind:'permission_request', request_id:'ask', tool_name:'Read', input:{}}, 1)
  expect(point().getAttribute('data-activity')).toBe('waiting')
  win.con.handleEvent('project/main', {kind:'turn_completed', session_id:'conv'}, 1)
  expect(point().getAttribute('data-activity')).toBe('completed')
  expect(point().getAttribute('title')).toContain('応答完了')
  expect(win.document.querySelector('.vp-proj-summary')!.textContent).not.toContain('作業中')
  win.con.handleEvent('project/one', {kind:'thought_chunk', text:'background'}, 1)
  expect(point().getAttribute('data-activity')).toBe('completed')
  win.con.handleEvent('project/main', {kind:'replay_start'}, 1)
  win.con.handleEvent('project/main', {kind:'thought_chunk', text:'old thought'}, 1)
  win.con.handleEvent('project/main', {kind:'replay_end',in_flight:false}, 1)
  expect(point().getAttribute('data-activity')).toBe('completed')
  point().parentElement!.dispatchEvent(new win.MouseEvent('click',{bubbles:true}))
  expect(win.sent.at(-1)).toEqual({t:'process:toggle',path:'/project',expanded:false})
})

it('keeps actionable Codex requests red through background work and Result', () => {
  const win = fixture()
  const point = () => win.document.querySelector('.vp-proj-toggle .vp-activity-point')!
  win.con.handleEvent('project/main', {kind:'codex_interactions', requests:[{request_id:'q', can_accept:true}]}, 1)
  win.con.handleEvent('project/main', {kind:'thought_chunk',text:'still thinking'}, 1)
  expect(point().getAttribute('data-activity')).toBe('waiting')
  win.con.handleEvent('project/main', {kind:'turn_completed',session_id:'conv'}, 1)
  expect(point().getAttribute('data-activity')).toBe('waiting')
  win.con.handleEvent('project/main', {kind:'codex_interactions',requests:[]}, 1)
  expect(point().getAttribute('data-activity')).toBe('completed')
})

it('uses the same adjustable activity token for the point and status text', () => {
  const win = fixture()
  const point = win.document.querySelector('[data-activity="working"]')!
  const label = win.document.querySelector('.vp-proj-summary .vp-lane-state')!
  expect(point.getAttribute('style')).toContain('--sb-activity-working')
  expect(label.getAttribute('style')).toContain('--sb-activity-working')
})
