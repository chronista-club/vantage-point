// vp-app web bundle builder.
//
// SolidJS の JSX は通常の jsx-automatic では不十分なため、esbuild-plugin-solid を
// 使って Babel SolidJS plugin 経由で compile する。
//
// Output: ../assets/editor-host.bundle.js (iife、minified、self-contained)

import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'

const isDev = process.argv.includes('--dev')

await build({
  entryPoints: ['entry.tsx'],
  bundle: true,
  format: 'iife',
  target: 'es2022',
  outfile: '../assets/editor-host.bundle.js',
  plugins: [solidPlugin()],
  minify: !isDev,
  sourcemap: isDev ? 'inline' : false,
  logLevel: 'info',
})
