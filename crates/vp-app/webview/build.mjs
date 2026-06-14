// vp-app web bundle builder.
//
// SolidJS の JSX は通常の jsx-automatic では不十分なため、esbuild-plugin-solid を
// 使って Babel SolidJS plugin 経由で compile する。
//
// 2 entry / 2 output (v1.0 柱 2 PR-1 で sidebar bundle を追加):
//   entry.tsx   → ../assets/editor-host.bundle.js  (main WebView)
//   sidebar.tsx → ../assets/sidebar.bundle.js      (sidebar WebView)
//
// mode:
//   (default)  prod build: minify + 2 bundle を 1 回ずつ build して exit。
//   --dev      no-minify + inline sourcemap で 1 回 build。
//   --watch    no-minify + inline sourcemap + esbuild context.watch() で常駐。
//              保存毎に ~0.5s rebuild。 `VP_WEBVIEW_DEV=<assets dir>` の vp-app と
//              組で frontend HMR ループ (cargo build 不要)。

import { build, context } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'

const isWatch = process.argv.includes('--watch')
const isDev = isWatch || process.argv.includes('--dev') // watch は常に dev (minify 無し)

/** 両 bundle 共通の esbuild オプション。 */
const common = {
  bundle: true,
  format: 'iife',
  target: 'es2022',
  plugins: [solidPlugin()],
  minify: !isDev,
  sourcemap: isDev ? 'inline' : false,
  logLevel: 'info',
  // CSS を文字列として import し entry.tsx で `<style>` 注入できるよう text loader に切替
  // (= esbuild default loader は CSS を別 file 化するため)。 JS bundle 内に内包する。
  loader: {
    '.css': 'text',
  },
}

const targets = [
  { entryPoints: ['entry.tsx'], outfile: '../assets/editor-host.bundle.js' }, // main WebView
  { entryPoints: ['sidebar.tsx'], outfile: '../assets/sidebar.bundle.js' }, // sidebar WebView
]

if (isWatch) {
  // watch mode: context + watch (保存毎に rebuild、 ~0.5s)。 process は常駐。
  const ctxs = await Promise.all(targets.map((t) => context({ ...common, ...t })))
  await Promise.all(ctxs.map((c) => c.watch()))
  console.log('[esbuild] watching webview… 編集 → ~0.5s rebuild → WebView reload で反映')
} else {
  // one-shot build (entry ごとに outfile 指定で 2 回呼ぶ)。
  for (const t of targets) await build({ ...common, ...t })
}
