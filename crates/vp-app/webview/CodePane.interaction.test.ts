import { build } from 'esbuild'
import { Window } from 'happy-dom'
import { expect, it } from 'vitest'
import { solidTestPlugin, evalDom } from './test-utils/solid'
import { testPath } from './test-utils/paths'

it.each(['production', 'development'])('requests the reopened lane in the same native callback (%s)', async mode => {
  const result = await build({
    stdin: {
      contents: `import { mountCodePane } from './CodePane';
        import { installCodeView } from './code-view';
        window.pane = mountCodePane(document.body);
        window.view = installCodeView();`,
      loader: 'tsx', resolveDir: testPath(import.meta.url),
    },
    bundle: true, write: false, format: 'iife', platform: 'browser', preserveSymlinks: true,
    plugins: [solidTestPlugin()], conditions: mode === 'development' ? ['development'] : [],
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) },
  })
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } })
  try {
    evalDom(win, `window.sent=[]; window.ipc={postMessage: m => window.sent.push(JSON.parse(m))};`)
    evalDom(win, result.outputFiles[0].text)
    // entry.tsx と同じ順序。二つの呼び出しの間では scheduler を進めない。
    evalDom(win, `view.openFor('vp/one'); pane.setLane('vp/one'); view.setActiveLane('vp/one');`)
    expect(evalDom(win, 'sent')).toEqual([{ t: 'code:list', lane: 'vp/one' }])
    evalDom(win, `view.openFor('vp/two'); pane.setLane('vp/two'); view.setActiveLane('vp/two');`)
    expect(evalDom(win, 'sent')).toEqual([
      { t: 'code:list', lane: 'vp/one' }, { t: 'code:list', lane: 'vp/two' },
    ])
    evalDom(win, `pane.setLane(null); view.setActiveLane('vp/one');`)
    expect(evalDom(win, 'sent.length')).toBe(2)
  } finally { await win.happyDOM.abort() }
})
