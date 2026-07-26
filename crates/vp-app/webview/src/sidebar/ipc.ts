/**
 * sidebar ⇄ Rust の IPC bridge。
 *
 * 両方向とも SSOT は `crates/vp-app/schema/vp-sidebar.kdl`（1 つの `ipc` channel が両方向を
 * 運び、envelope は方向ごとに別名）:
 *
 * - **JS → Rust**: `window.ipc.postMessage(JSON)`（`IpcEnvelope`）。Rust 側
 *   `handle_sidebar_ipc` が受ける。
 * - **Rust → JS**: `window.vpSidebarDispatch`（`IpcEventEnvelope`、`./dispatch`）。
 *   ⚠️ 旧来は `window.renderSidebarState(...)` のように**名前で関数を呼ぶ**形だった
 *   （doc 53 §6.5.1.3 で移行）。`window.*` の面は**残っている**が、役目が変わった —
 *   「Rust が呼ぶ入口」ではなく「**受け手の置き場**」。overlay 系は component の
 *   mount / unmount に合わせて出入りする（`FileExplorer.tsx` / `WirePanel.tsx` /
 *   `AddPerformer.tsx`）ので、下の handler は **呼ぶ時に引く**（install 時に capture
 *   しない）。受け手が居なければ落ちるが、それは「消費者が居ない」であって取りこぼしでは
 *   ない — bundle 未評価の窓（そちらは `./dispatch` の保留箱が預かる）とは別の話。
 *
 * ⚠️ `renderError` / `handleAddPerformerResult` / `setClonePath` は **今も stub のまま**
 *   （PR-2 / PR-3 で実描画に繋ぐ予定が据え置き）。Rust 側は撃っているので、繋げば出る。
 */
import { applySidebarState } from './store'
import type { SidebarState } from '../generated/SidebarState'
import type { IpcEnvelope } from '../generated/SidebarIpc'
import { installSidebarDispatch } from './dispatch'

/** wry が webview に注入する IPC channel。 */
interface WryIpc {
  postMessage(msg: string): void
}

declare global {
  interface Window {
    ipc?: WryIpc
    /** ⚠️ **実装なし**（下の stub 止まり。PR-2 で実描画に繋ぐ予定のまま）。 */
    renderError?: (msg: string) => void
    /** ⚠️ **実装なし**（stub 止まり。PR-3 で実描画に繋ぐ予定のまま）。 */
    handleAddPerformerResult?: (msg: unknown) => void
    /** `AddPerformer.tsx` が form 表示中だけ**差し替える**受け手（外すと前の値に戻す）。 */
    handleStandsResult?: (msg: unknown) => void
    /** ⚠️ **実装なし**（stub 止まり。PR-3 で実描画に繋ぐ予定のまま）。 */
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
 * Rust → JS の押し込みを実処理に繋ぐ。
 *
 * component の mount より前に呼ぶこと（`sidebar.tsx` の boot で最初に呼ぶ）。ただし
 * **受け口自体**（`openSidebarDispatch`）はさらに前 = module 評価の先頭で生やす — bundle
 * 評価中に届いた押し込みを保留箱が預かれるようにするため。
 *
 * ⚠️ 各 handler が `window.*` を **呼ぶ時に引く**のは意図的。overlay 系（File Explorer /
 * Wire / Add Performer）の受け手は component の mount / unmount で出入りするので、install 時に
 * capture すると unmount 後の古い closure を掴む。**受け手が居ない = 消費者が居ない**ので
 * 落として正しい（bundle 未評価の窓とは別の話で、そちらは保留箱が預かる）。
 */
export function installIpcBridge(): void {
  // 未 mount の窓で `ReferenceError` を出さないための受け皿 stub。
  window.renderError ??= (msg) => console.warn('[vp-sidebar] renderError (stub):', msg)
  window.handleAddPerformerResult ??= (msg) =>
    console.debug('[vp-sidebar] handleAddPerformerResult (stub):', msg)
  window.handleStandsResult ??= (msg) =>
    console.debug('[vp-sidebar] handleStandsResult (stub):', msg)
  window.setClonePath ??= (path) => console.debug('[vp-sidebar] setClonePath (stub):', path)

  installSidebarDispatch({
    // state の形の持ち主は Rust の `SidebarState`（ts-rs 生成）。envelope は「どの窓口へ
    // 届けるか」だけを型にしているので、中身はここでその 1 つの定義に落とす。
    state: (state) => applySidebarState(state as SidebarState),
    error: (message) => window.renderError?.(message),
    performerCreateResult: (repo_path, name, error) =>
      window.handleAddPerformerResult?.({ repo_path, name, error }),
    standsResult: (repo_path, stands, error) =>
      window.handleStandsResult?.({ repo_path, stands, error }),
    filesListResult: (address, entries, truncated) =>
      window.vpFiles?.handleListResult({
        address,
        entries: entries as Parameters<
          NonNullable<Window['vpFiles']>['handleListResult']
        >[0]['entries'],
        truncated,
      }),
    wireResult: (payload) =>
      window.vpWire?.handleResult(
        payload as Parameters<NonNullable<Window['vpWire']>['handleResult']>[0],
      ),
    clonePathPicked: (path) => window.setClonePath?.(path),
    filePickerOpen: (address) => window.vpFilePicker?.open(address),
  })
}
