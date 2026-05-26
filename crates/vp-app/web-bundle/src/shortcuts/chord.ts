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
    const mod = isMac ? e.metaKey : e.ctrlKey

    if (!mod) return
    // Shift / Alt の組合せは別 layer (layout 系等) なので directive 対象外
    if (e.shiftKey || e.altKey) return
    // Modifier 自身の keydown (Meta / Control) は skip
    if (e.key === 'Meta' || e.key === 'Control') return

    const key = e.key.toLowerCase()
    if (key.length !== 1) return // 1 文字キーのみ directive 対象

    if (DIRECTIVE_TABLE[key]) {
      e.preventDefault()
      try {
        ctx.exec(key)
      } catch (err) {
        console.warn('[directive] exec failed:', key, err)
      }
    }
  }

  // capture phase で取って xterm.js / picker input 等の inner listener より先に判定
  window.addEventListener('keydown', handler, true)

  return () => {
    window.removeEventListener('keydown', handler, true)
  }
}
