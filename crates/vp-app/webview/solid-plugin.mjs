import { readFile } from 'node:fs/promises'
import { transform } from '@solidjs/compiler'
import { transformSync } from 'esbuild'
import remapping from '@ampproject/remapping'

/** app bundle・DOM テスト・preview で共用する Solid 2 compiler。 */
export function solidPlugin() {
  return {
    name: 'solid-2',
    setup(build) {
      const compile = (source, filename) => {
        const stripped = transformSync(source, { loader: filename.endsWith('.jsx') ? 'jsx' : 'tsx', jsx: 'preserve', sourcefile: filename, sourcemap: 'external' })
        const result = transform(stripped.code, { filename, generate: 'dom', sourceMap: true })
        const map = result.map ? remapping([JSON.parse(result.map), JSON.parse(stripped.map)], () => null).toString() : null
        return result.code + (map
          ? `\n//# sourceMappingURL=data:application/json;base64,${Buffer.from(map).toString('base64')}`
          : '')
      }
      // DOM テストと preview の inline TSX entry も同じ compiler を通す。
      const stdin = build.initialOptions.stdin
      if (stdin && ['tsx', 'jsx'].includes(stdin.loader)) {
        stdin.contents = compile(stdin.contents, stdin.sourcefile || 'inline.tsx')
        stdin.loader = 'js'
      }
      build.onLoad({ filter: /\.[jt]sx$/ }, async ({ path }) => ({
        contents: compile(await readFile(path, 'utf8'), path), loader: 'js',
      }))
    },
  }
}
