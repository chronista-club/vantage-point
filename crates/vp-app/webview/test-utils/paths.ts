import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** fixture のパスが Vite のブラウザ向け URL 変換を受けるのを避ける。 */
export const testPath = (moduleUrl: string, relative = '.') => resolve(dirname(fileURLToPath(moduleUrl)), relative)
