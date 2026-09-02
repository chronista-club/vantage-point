import { defineConfig } from 'vitest/config'
import { resolve } from 'path'

// solid-js を browser 条件で解決させる設定は 2 段構え:
//   1. resolve.conditions / ssr.resolve.conditions — Vite が変換する module に効く条件
//   2. server.deps.inline — vitest 4 は environment: 'node' で node_modules を Node のネイティブ
//      解決に外出し (externalize) するため、1 の条件が届かず solid-js の **server build** が選ばれ
//      「Client-only API called on the server side」で creo-ui-icons-web 経由の import が落ちる
//      (vitest 3 → 4 で 6 file が全滅、2026-09-02)。solid-js と creo-ui 系を inline にして Vite に通す
const solidConditions = ['development', 'browser']

export default defineConfig({
  test: {
    environment: 'node',
    include: ['**/*.test.ts', '**/*.test.tsx'],
    exclude: ['node_modules', 'dist'],
    server: {
      deps: {
        inline: [/solid-js/, /@chronista-club\/creo-ui/],
      },
    },
  },
  resolve: {
    conditions: solidConditions,
    // solid-js の多重解決を防ぐ (HistoryStrip.test.ts の "multiple instances" warning 対策)
    alias: {
      'solid-js': resolve('./node_modules/solid-js'),
      'solid-js/web': resolve('./node_modules/solid-js/web'),
    },
  },
  ssr: {
    resolve: {
      conditions: solidConditions,
    },
  },
})
