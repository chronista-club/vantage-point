import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLElement as DomHTMLElement } from 'happy-dom'
import type { LaneHeaderApi } from './LaneHeader'
import { fileURLToPath } from 'node:url'

// build.mjs と同じく実 Solid コンポーネントをコンパイルする。DOM click では
// WebKit の compositor の hit test を再現できないため、背後の terminal canvas に
// クリックが届いた問題の clipping 境界も配置の条件として検証する。
type ProbeWindow = Window & { sent: unknown[]; header: LaneHeaderApi }
let bundle: string
const windows: Window[] = []
beforeAll(async () => {
  const result = await build({
    stdin: {
      contents: `
        import { mountLaneHeader, LANE_HEADER_CSS } from './LaneHeader';
        import { noteSessionList } from './console';
        const lane = 'vp/lane/main';
        noteSessionList(lane, 39, [
          { key: 35, agent: 'claude', root: true, engine_session_id: 'claude-thread' },
          { key: 39, agent: 'codex', root: false, engine_session_id: 'codex-thread' },
          { key: 40, agent: 'shell', root: false }
        ]);
        const style = document.createElement('style');
        style.textContent = LANE_HEADER_CSS;
        document.head.append(style);
        window.sent = [];
        window.ipc = { postMessage: message => window.sent.push(JSON.parse(message)) };
        window.header = mountLaneHeader(document.getElementById('lane-header'), { headerState: () => ({}) });
        window.header.setLane({ addr: lane, sessionId: 'claude-thread', agent: 'claude' });
      `,
      resolveDir: fileURLToPath(new URL('.', import.meta.url)),
      loader: 'tsx',
    },
    bundle: true,
    write: false,
    format: 'iife',
    platform: 'browser',
    plugins: [solidPlugin()],
    define: { 'process.env.NODE_ENV': '"production"' },
  })
  bundle = result.outputFiles[0].text
})

function fixture() {
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } }) as ProbeWindow
  windows.push(win)
  win.document.body.innerHTML = '<div id="pane-lane"><div id="lane-header" style="height:30px;overflow:hidden"></div><canvas id="terminal"></canvas></div>'
  win.eval(bundle)
  win.document.querySelector<DomHTMLElement>('.eh-session')!.click()
  return win
}

afterEach(async () => {
  for (const win of windows.splice(0)) await win.happyDOM.abort()
})

describe('root picker interaction above terminal panes', () => {
  it('keeps the menu outside the clipped header and sends the chosen Codex session once', () => {
    const win = fixture()
    const menu = win.document.querySelector('.eh-root-picker')!
    expect(menu.closest('#lane-header')).toBeNull()
    expect(menu.closest('#pane-lane')).not.toBeNull()
    const row = menu.querySelectorAll('button')[1]
    expect(win.getComputedStyle(row).display).toBe('flex')
    row.click()
    expect(win.sent).toEqual([{ t: 'console:switch_root', lane: 'vp/lane/main', session: 39 }])
    expect(win.document.querySelector('.eh-root-picker')).toBeNull()
  })

  it('does not send a switch for an unknown engine or the current root', () => {
    const win = fixture()
    win.document.querySelectorAll('.eh-rp-row')[2].dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
    expect(win.sent).toEqual([])
    expect(win.document.querySelector('.eh-root-picker')).not.toBeNull()
    win.document.querySelector<DomHTMLElement>('.eh-rp-row')!.click()
    expect(win.sent).toEqual([])
    expect(win.document.querySelector('.eh-root-picker')).toBeNull()
  })

  it('removes the detached menu on outside click and when leaving the lane', () => {
    const win = fixture()
    win.document.getElementById('terminal')!.dispatchEvent(new win.MouseEvent('click', { bubbles: true }))
    expect(win.document.querySelector('.eh-root-picker')).toBeNull()
    win.document.querySelector<DomHTMLElement>('.eh-session')!.click()
    win.header.setLane(null)
    expect(win.document.querySelector('.eh-root-picker')).toBeNull()
    expect(win.sent).toEqual([])
  })
})
