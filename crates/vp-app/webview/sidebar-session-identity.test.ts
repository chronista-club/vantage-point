// mem_1CfkeiUePgFsbYoGtTeDpq
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window } from 'happy-dom'
import { fileURLToPath } from 'node:url'

let bundle: string
const windows: Window[] = []
beforeAll(async () => {
  const result = await build({
    stdin: { contents: `
      import { render } from 'solid-js/web';
      import { createComponent } from 'solid-js';
      import { LaneRow, SessionRow } from './src/sidebar/LaneRow';
      const sessions = [{ key: 58, agent: 'grok', mode: 'gui' }, { key: 57, agent: 'codex', mode: 'tui' }];
      const lane = { address: {repo:'vp', name:'demo', key:'vp/lane/demo'}, agent:'claude', cwd:'/repo/demo', pid:1, sessions:{root:58, focused:57, sessions} };
      window.sent = [];
      window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) };
      const host = document.createElement('div'); document.body.append(host);
      render(() => [createComponent(LaneRow,{lane,repoPath:'/repo'}),createComponent(SessionRow,{lane,repoPath:'/repo',session:sessions[1]})],host);
      const root = document.createElement('div'); root.id='root-session'; document.body.append(root);
      render(() => createComponent(SessionRow,{lane,repoPath:'/repo',session:sessions[0]}),root);
    `, resolveDir: fileURLToPath(new URL('.', import.meta.url)), loader:'tsx' },
    bundle:true, write:false, format:'iife', platform:'browser', plugins:[solidPlugin()],
    define:{'process.env.NODE_ENV':'"production"'},
  })
  bundle = result.outputFiles[0].text
})
function fixture() {
  const win = new Window({ settings:{enableJavaScriptEvaluation:true, suppressInsecureJavaScriptEnvironmentWarning:true} })
  windows.push(win)
  win.eval(bundle)
  ;(win as Window & {sent: unknown[]}).sent = [] // mount 時の URL 読取と操作を分ける
  return win as Window & {sent: unknown[]}
}
afterEach(async () => { for (const win of windows.splice(0)) await win.happyDOM.abort() })
describe('sidebar session identity', () => {
  it('uses the registry root agent between the state point and title', () => {
    const win = fixture()
    const row = win.document.querySelector('.vp-lane-row')!
    expect(row.querySelector('.vp-agent-icon')?.getAttribute('title')).toBe('Grok')
    expect(row.querySelector('.vp-agent-icon')?.nextElementSibling?.classList.contains('vp-lane-title')).toBe(true)
  })
  it('identifies the extra session and selects its exact key', () => {
    const win = fixture()
    const row = win.document.querySelector('.vp-session-row')!
    expect(row.querySelector('.vp-agent-icon')?.getAttribute('title')).toBe('Codex')
    expect(row.textContent).toContain('追加セッション')
    row.dispatchEvent(new win.MouseEvent('click', {bubbles:true}))
    expect(win.sent).toEqual([
      {t:'lane:select', path:'/repo', address:'vp/lane/demo'},
      {t:'conversation:session_focus', lane:'vp/lane/demo', session:57},
    ])
  })
  it('requires confirmation to close only the extra session and protects root', () => {
    const win = fixture()
    const close = win.document.querySelector('.vp-session-close')
    expect(close).not.toBeNull()
    close!.dispatchEvent(new win.MouseEvent('click', {bubbles:true}))
    expect(win.sent).toEqual([])
    close!.dispatchEvent(new win.MouseEvent('click', {bubbles:true}))
    expect(win.sent).toEqual([{t:'conversation:session_remove',lane:'vp/lane/demo',session:57}])
    expect(win.document.querySelector('#root-session .vp-session-close')).toBeNull()
  })
})
