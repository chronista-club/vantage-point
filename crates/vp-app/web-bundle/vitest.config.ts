import { defineConfig } from 'vitest/config'
import { resolve } from 'path'

export default defineConfig({
  test: {
    environment: 'node',
    include: ['**/*.test.ts', '**/*.test.tsx'],
    exclude: ['node_modules', 'dist'],
  },
  resolve: {
    conditions: ['development', 'browser'],
    // solid-js の多重解決を防ぐ (HistoryStrip.test.ts の "multiple instances" warning 対策)
    alias: {
      'solid-js': resolve('./node_modules/solid-js'),
      'solid-js/web': resolve('./node_modules/solid-js/web'),
    },
  },
})
