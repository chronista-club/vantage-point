// mem_1Cfgup9xPfPHvwJU2gSDUM — 中央の設定から作業へ戻る。
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLElement, type HTMLInputElement } from 'happy-dom'
import { fileURLToPath } from 'node:url'

type Probe = Window & { sent: unknown[]; vpSettings: { open(): void; handleResult(s: unknown): void }; selectLane(lane: string): void; selectDevices(): void; dispose(): void }
let bundle: string
const windows: Window[] = []
beforeAll(async () => {
  const result = await build({
    stdin: {
      contents: `
        import { render } from 'solid-js/web';
        import { createComponent } from 'solid-js';
        import { SettingsPanel, SETTINGS_PANEL_CSS } from './src/sidebar/SettingsPanel';
        import { applySidebarState, emptyState } from './src/sidebar/store';
        window.sent = [];
        window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) };
        window.selectLane = lane => applySidebarState({ ...emptyState(), active_lane_address: lane });
        window.selectDevices = () => applySidebarState({ ...emptyState(), active_component: { repo_path: '/repos/vp', kind: 'devices' } });
        window.selectLane('vp/root');
        const style = document.createElement('style');
        style.textContent = SETTINGS_PANEL_CSS;
        document.head.append(style);
        window.dispose = render(() => createComponent(SettingsPanel, {}), document.getElementById('sidebar-root'));
      `,
      resolveDir: fileURLToPath(new URL('.', import.meta.url)), loader: 'tsx',
    }, bundle: true, write: false, format: 'iife', platform: 'browser',
    plugins: [solidPlugin()], define: { 'process.env.NODE_ENV': '"production"' },
  })
  bundle = result.outputFiles[0].text
})
function fixture() {
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as Probe
  windows.push(win)
  win.document.body.innerHTML = '<div id="sidebar-root"></div><div id="host"><div id="work"><textarea>draft</textarea></div><div id="already-inert" inert></div></div>'
  win.eval(bundle)
  return win
}
const snapshot = { developerMode: false, developerModeLocked: false, defaultRepoRoot: '/repos', resolvedRepoRoot: '/repos', daemonReachable: true, logLevel: 'info', idleTimeoutMinutes: 5, defaultAgent: 'claude', defaultModel: '', defaultAgentTakesModel: true }
afterEach(async () => { for (const win of windows.splice(0)) await win.happyDOM.abort() })

describe('settings in the main area', () => {
  it('opens in host and returns to the same workspace DOM, draft and focus', () => {
    const win = fixture()
    const work = win.document.getElementById('work')!
    const input = win.document.querySelector('textarea')!
    input.focus()
    win.vpSettings.open()
    const panel = win.document.querySelector('.vp-settings-panel')!
    expect(panel.closest('#host')).not.toBeNull()
    expect(work.hasAttribute('inert')).toBe(true)
    expect(panel.contains(win.document.activeElement)).toBe(true)
    expect(win.sent).toEqual([{ t: 'settings:fetch' }])
    const back = [...panel.querySelectorAll('button')].find(b => b.textContent.includes('作業に戻る'))!
    expect(back).toBeDefined()
    back.click()
    expect(win.document.querySelector('.vp-settings-panel')).toBeNull()
    expect(win.document.getElementById('work')).toBe(work)
    expect(input.value).toBe('draft')
    expect(win.document.activeElement).toBe(input)
    expect(work.hasAttribute('inert')).toBe(false)
    expect(win.document.getElementById('already-inert')!.hasAttribute('inert')).toBe(true)
  })
  it('ignores a same-lane push, closes on a lane switch and can reopen', () => {
    const win = fixture()
    const input = win.document.querySelector('textarea')!
    input.focus()
    win.vpSettings.open()
    win.selectLane('vp/root')
    expect(win.document.querySelector('.vp-settings-panel')).not.toBeNull()
    win.selectLane('vp/other')
    expect(win.document.activeElement).not.toBe(input)
    expect(win.document.querySelector('.vp-settings-panel')).toBeNull()
    expect(win.document.getElementById('work')!.hasAttribute('inert')).toBe(false)
    win.vpSettings.open()
    win.selectDevices()
    expect(win.document.querySelector('.vp-settings-panel')).toBeNull()
    win.vpSettings.open()
    win.document.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    expect(win.document.querySelector('.vp-settings-panel')).toBeNull()
  })
  it('keeps existing settings save messages and authoritative result updates', () => {
    const win = fixture()
    win.vpSettings.open()
    win.vpSettings.handleResult(snapshot)
    const input = win.document.querySelector<HTMLInputElement>('#vp-set-reporoot')!
    expect(input.closest('#host')).not.toBeNull()
    input.value = ' /new/repos '
    input.dispatchEvent(new win.Event('change', { bubbles: true }))
    expect(win.sent.at(-1)).toEqual({ t: 'settings:save', default_repo_root: '/new/repos' })
    win.vpSettings.handleResult({ ...snapshot, defaultRepoRoot: '/confirmed' })
    expect(input.value).toBe('/confirmed')
    win.document.querySelector<HTMLElement>('.vp-settings-toggle')!.click()
    expect(win.sent.at(-1)).toEqual({ t: 'settings:save', developer_mode: true })
  })
  it('restores inert state for new panes and cleans up on unmount', async () => {
    const win = fixture()
    win.vpSettings.open()
    const pane = win.document.createElement('div')
    win.document.getElementById('host')!.append(pane)
    await win.happyDOM.whenAsyncComplete()
    expect(pane.hasAttribute('inert')).toBe(true)
    win.dispose()
    expect(pane.hasAttribute('inert')).toBe(false)
    expect(win.document.querySelector('.vp-settings-panel')).toBeNull()
    expect(win.vpSettings).toBeUndefined()
  })
})
