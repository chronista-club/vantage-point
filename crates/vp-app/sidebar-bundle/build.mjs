// vp-app sidebar bundle builder.
//
// iconify-icon Web Component を register するだけの minimal bundle。
// web-bundle と異なり SolidJS を含まないので esbuild-plugin-solid 不要、 plain TS で OK。
//
// Output: ../assets/sidebar-icons.bundle.js (iife、 self-contained、 minified)

import { build } from 'esbuild'

const isDev = process.argv.includes('--dev')

await build({
  entryPoints: ['entry.ts'],
  bundle: true,
  format: 'iife',
  target: 'es2022',
  outfile: '../assets/sidebar-icons.bundle.js',
  minify: !isDev,
  sourcemap: isDev ? 'inline' : false,
  logLevel: 'info',
})
