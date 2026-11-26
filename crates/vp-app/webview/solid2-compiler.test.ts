import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { Window } from 'happy-dom'

it('compiles typed JSX with reactive classes and disposes settled effects', async () => {
  // compiler 未実装時も契約の失敗として示せるよう、動的に解決する。
  const plugin = await import('./solid-plugin.mjs').catch(() => null)
  expect(plugin, 'Solid 2 compiler integration must be available').not.toBeNull()
  const result = await build({
    stdin: {
      contents: `import { createSignal, flush, onSettled } from 'solid-js';
        import { render } from '@solidjs/web';
        const [active, setActive] = createSignal<boolean>(false);
        window.dispose = render(() => {
          onSettled(() => { window.live = true; return () => { window.live = false } });
          return <button class={['base', { active: active() }]} onClick={() => setActive(true)}>test</button>;
        }, document.body);
        window.flush = flush; flush();`,
      resolveDir: process.cwd(), loader: 'tsx',
    }, bundle: true, write: false, format: 'iife', platform: 'browser',
    plugins: [plugin!.solidPlugin()],
  })
  const win = new Window({ settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true } })
  try {
    win.eval(result.outputFiles[0].text)
    const button = win.document.querySelector('button')!
    expect(button.className).toBe('base')
    button.click()
    win.eval('flush()')
    expect(button.classList.contains('active')).toBe(true)
    expect(win.eval('live')).toBe(true)
    win.eval('dispose(); flush()')
    expect(win.eval('live')).toBe(false)
  } finally { await win.happyDOM.abort() }
})

it('preserves the original TypeScript in the bundle source map', async () => {
  const { solidPlugin } = await import('./solid-plugin.mjs')
  const source = `import { createSignal } from 'solid-js';
    const [count] = createSignal<number>(3);
    document.body.append(<span>{count()}</span>);`
  const result = await build({
    stdin: { contents: source, sourcefile: 'source-map.tsx', resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', platform: 'browser',
    sourcemap: 'external', outfile: '/tmp/vp-solid2-source-map.js', plugins: [solidPlugin()],
  })
  const map = JSON.parse(result.outputFiles.find(file => file.path.endsWith('.map'))!.text)
  const index = map.sources.findIndex((file: string) => file.endsWith('source-map.tsx'))
  expect(index).toBeGreaterThanOrEqual(0)
  expect(map.sourcesContent[index]).toBe(source)
})
