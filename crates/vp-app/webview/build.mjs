// vp-app web bundle builder.
//
// SolidJS の JSX は通常の jsx-automatic では不十分なため、esbuild-plugin-solid を
// 使って Babel SolidJS plugin 経由で compile する。
//
// 2 entry / 2 output:
//   entry.tsx   → ../assets/editor-host.bundle.js
//   sidebar.tsx → ../assets/sidebar.bundle.js
//
// WebView 統合 (step 3a) 以降は 2 WebView ではなく **単一 WebView**。両 bundle とも
// `MAIN_AREA_HTML` から外部 `<script src>` で参照され 1 document で動く (doc 48 Phase 1 で
// inline → 外部化。prod は `MAIN_VIEW_ASSETS` の baked 配信、dev は disk-read)。
//
// mode:
//   (default)  prod build: minify + 2 bundle を 1 回ずつ build して exit。
//   --dev      no-minify + inline sourcemap で 1 回 build。
//   --watch    no-minify + inline sourcemap + esbuild context.watch() で常駐。保存毎に ~0.5s rebuild。
//              `VP_WEBVIEW_DEV=<assets dir>` で起動した vp-app が *.bundle.js を disk-read するので、
//              保存 → View メニュー「Reload WebView」(Cmd+R) だけで反映される (cargo build 不要、
//              手順は docs/guide/webview.md)。

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
