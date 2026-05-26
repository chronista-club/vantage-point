/**
 * sidebar ⇄ Rust の IPC bridge。
 *
 * v1.0 柱 2 PR-1 足場。 旧 SIDEBAR_HTML が持っていた IPC プロトコルを型付きで再構築する。
 *
 * - **Rust → JS**: Rust が `webview.evaluate_script("window.<fn>(...)")` で呼ぶ関数群。
 *   主役は `renderSidebarState` (state push の唯一経路)。 PR-1 ではこれだけ実描画に
 *   接続し、 残り 4 関数 (renderError / handleAddWingResult / handleStandsResult /
 *   setClonePath) は PR-2/PR-3 まで no-op stub の受け皿にする。
 * - **JS → Rust**: `window.ipc.postMessage(JSON)` で送る 11 種の sidebar 操作メッセージ。
 *   Rust 側 `handle_sidebar_ipc` が受ける。
 */
import { applySidebarState } from './store'
import type { SidebarState } from '../generated/SidebarState'
import type { IpcEnvelope } from '../generated/SidebarIpc'

/** wry が webview に注入する IPC channel。 */
interface WryIpc {
  postMessage(msg: string): void
}

declare global {
  interface Window {
    ipc?: WryIpc
    /** Rust → JS: `SidebarState` を push する唯一の経路。 */
    renderSidebarState?: (state: SidebarState) => void
    /** Rust → JS: TheWorld 接続失敗等の error 表示 (PR-2 で実装)。 */
    renderError?: (msg: string) => void
    /** Rust → JS: Add Wing form の結果通知 (PR-3 で実装)。 */
    handleAddWingResult?: (msg: unknown) => void
    /** Rust → JS: Stand 一覧 fetch の結果通知 (PR-3 で実装)。 */
    handleStandsResult?: (msg: unknown) => void
    /** Rust → JS: Clone folder picker の結果反映 (PR-3 で実装)。 */
    setClonePath?: (path: string) => void
  }
}

/**
 * 型付き postMessage wrapper。
 *
 * メッセージ型 `IpcEnvelope` (sidebar → Rust の IPC、`{ t: <種別>, ... }` の
 * discriminated union) は `crates/vp-app/schema/vp-sidebar.kdl` を SSOT に
 * club-kdl-codegen で生成 (`../generated/SidebarIpc`)。 Rust 側 `handle_sidebar_ipc`
 * が受ける enum も同じ schema 由来 (VP-208) で、 wire 互換が schema 経由で保証される。
 */
export function sendIpc(msg: IpcEnvelope): void {
  const ipc = window.ipc
  if (!ipc) {
    console.warn('[vp-sidebar] window.ipc 未注入 — メッセージを破棄:', msg)
    return
  }
  ipc.postMessage(JSON.stringify(msg))
}

/**
 * Rust → JS の関数を `window` に登録する。
 *
 * Rust は webview build 直後から `renderSidebarState` 等を呼びうるため、 component の
 * mount より前に呼ぶこと。 未登録関数の `ReferenceError` を防ぐため、 PR-1 未実装の
 * 4 関数も no-op stub を登録しておく。
 */
export function installIpcBridge(): void {
  window.renderSidebarState = (state) => applySidebarState(state)

  // PR-2 / PR-3 で実描画に接続予定の受け皿 stub。
  window.renderError ??= (msg) => console.warn('[vp-sidebar] renderError (stub):', msg)
  window.handleAddWingResult ??= (msg) =>
    console.debug('[vp-sidebar] handleAddWingResult (stub):', msg)
  window.handleStandsResult ??= (msg) =>
    console.debug('[vp-sidebar] handleStandsResult (stub):', msg)
  window.setClonePath ??= (path) => console.debug('[vp-sidebar] setClonePath (stub):', path)
}
