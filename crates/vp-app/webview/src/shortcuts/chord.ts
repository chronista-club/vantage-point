/**
 * VP shortcut directive dispatcher (= Layer D "実装方針" in docs/design/18-shortcut-convention.md)
 *
 * `Cmd hold + <key>` (macOS) / `Ctrl hold + <key>` (Linux/Windows) の keydown event を
 * capture phase で listen し、 `DIRECTIVE_TABLE` に登録された key なら exec を呼ぶ。
 *
 * **chord 2 段 state machine ではない** — 規約 v0.3 で「動詞集合 + 単発キー」 design に
 * 統一されたため。 user の `Cmd hold f → 操作 → Cmd hold p` flow は OS 上では 2 つの独立
 * `Cmd+letter` keydown event として届くだけで、 state machine も timer も不要。
 *
 * (ファイル名は historical reason で `chord.ts` のままだが、 内部は directive dispatcher。)
 */

import { DIRECTIVE_TABLE } from './chord-table'

export interface DirectiveContext {
  /** registered directive (= `DIRECTIVE_TABLE[key]` 有り) の発火時に呼ばれる handler。 */
  exec(key: string): void
}

/** `directiveKeyOf` が読む keydown event の判定材料（KeyboardEvent の部分集合、テスト可能形）。 */
export interface DirectiveKeyInput {
  metaKey: boolean
  ctrlKey: boolean
  altKey: boolean
  shiftKey: boolean
  key: string
}

/**
 * keydown event が registered directive なら key を、そうでなければ null を返す（純関数）。
 *
 * Shift 修飾の扱い（v0.9 で全キー reject に統一）:
 * - `Ctrl+Shift + <key>` は layout / visual 系（Scene 切替 / Scene cyclic）の領域。
 *   **非 Mac では「Cmd hold」が Ctrl に折り畳まれる**ため、shift を通すと既存の
 *   `Ctrl+Shift+[ / ]`（Scene cyclic）が `[` `]` directive と二重発火する
 *   （moody-blues 指摘 #2 — symbol directive が空だった間は不可視だった衝突）。
 * - DIRECTIVE_TABLE のキーは shift 無しの表記しか持たないので、「shift 併用 = table の
 *   キーとは別の入力」として一律 reject が正。将来 shift+symbol directive を足すときは
 *   table 側に shift 込みの表現を導入してこの gate ごと再設計する。
 */
export function directiveKeyOf(
  e: DirectiveKeyInput,
  isMac: boolean,
): string | null {
  const mod = isMac ? e.metaKey : e.ctrlKey
  if (!mod) return null
  // Alt 修飾は terminal の Opt+letter 入力 (π 等) と被るので reject。
  if (e.altKey) return null
  // Modifier 自身の keydown (Meta / Control) は skip
  if (e.key === 'Meta' || e.key === 'Control') return null

  if (e.key.length !== 1) return null // 1 文字キーのみ directive 対象
  if (e.shiftKey) return null
  const key = e.key.toLowerCase()

  return DIRECTIVE_TABLE[key] ? key : null
}

/**
 * window に keydown listener (capture phase) を install する。
 * 戻り値: uninstall 関数。
 *
 * 各 WebView (main view / sidebar) で 1 回ずつ呼ぶ想定。 directive ごとの実際の挙動は
 * `ctx.exec(key)` 側で実装する (sidebar なら direct 呼出、 main view なら IPC bridge 等)。
 */
export function installDirectiveHandler(ctx: DirectiveContext): () => void {
  const handler = (event: Event): void => {
    const e = event as KeyboardEvent
    const isMac = navigator.platform.toUpperCase().includes('MAC')
    const key = directiveKeyOf(e, isMac)
    if (key === null) return

    e.preventDefault()
    try {
      ctx.exec(key)
    } catch (err) {
      console.warn('[directive] exec failed:', key, err)
    }
  }

  // capture phase で取って xterm.js / picker input 等の inner listener より先に判定
  window.addEventListener('keydown', handler, true)

  return () => {
    window.removeEventListener('keydown', handler, true)
  }
}
