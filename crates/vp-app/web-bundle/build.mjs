// vp-app web bundle builder.
//
// SolidJS の JSX は通常の jsx-automatic では不十分なため、esbuild-plugin-solid を
// 使って Babel SolidJS plugin 経由で compile する。
//
// 2 entry / 2 output (v1.0 柱 2 PR-1 で sidebar bundle を追加):
//   entry.tsx   → ../assets/editor-host.bundle.js  (main WebView)
//   sidebar.tsx → ../assets/sidebar.bundle.js      (sidebar WebView)
// 既存出力名を保つため outdir ではなく entry ごとに outfile を指定し build() を 2 回呼ぶ。

import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'

const isDev = process.argv.includes('--dev')

/** 両 bundle 共通の esbuild オプション。 */
const common = {
  bundle: true,
  format: 'iife',
  target: 'es2022',
  plugins: [solidPlugin()],
  minify: !isDev,
  sourcemap: isDev ? 'inline' : false,
  logLevel: 'info',
  // pp-content-persist follow-up: creoui-md-view/styles.css 等を文字列として import し、
  // entry.tsx で `<style>` 注入する。 esbuild default loader は CSS を別 file 化するため、
  // text loader に切り替えて JS bundle 内に内包する。
  loader: {
    '.css': 'text',
  },
}

// main WebView bundle
await build({
  ...common,
  entryPoints: ['entry.tsx'],
  outfile: '../assets/editor-host.bundle.js',
})

// sidebar WebView bundle (v1.0 柱 2 PR-1)
await build({
  ...common,
  entryPoints: ['sidebar.tsx'],
  outfile: '../assets/sidebar.bundle.js',
})
