/**
 * Rust → sidebar bundle の押し込みの **単一の受け口**（`window.vpSidebarDispatch`）。
 *
 * SSOT は `crates/vp-app/schema/vp-sidebar.kdl`。同じ `ipc` channel が両方向を運び、
 * 生成される envelope は方向ごとに別名（`IpcEnvelope` = client → server /
 * `IpcEventEnvelope` = server → client）。再生成は
 * `cargo test -p vp-app --test sidebar_ipc_codegen`。
 *
 * ## なぜ main bundle の `dispatch.ts` に相乗りしないか
 *
 * webview は 1 document だが **bundle は 2 本**（`editor-host.bundle.js` /
 * `sidebar.bundle.js`）で、module state を共有できない。`dispatch.ts` の保留箱は main bundle
 * の中にあり、こちらから受け手を登録する術がない。**bundle が受け口の単位**なので、
 * sidebar は自分の受け口を持つ（構造は main 側と対称）。
 *
 * ## 何が良くなるか（doc 53 §6.5.1.3）
 *
 * 旧来は `window.renderSidebarState(...)` のように**名前で関数を呼ぶ**形で、負債が 2 つ:
 *
 * 1. **型が無い** — 引数の数や順序が食い違っても Rust も TS も黙る
 * 2. **押し込みが黙って落ちる** — `window.X && window.X.y(...)` は bundle 準備前なら
 *    **no-op で「成功」する**
 *
 * ①は codegen が、②はここの保留箱が塞ぐ。
 *
 * ⚠️ 保留箱が救えるのは「bundle は評価済みだが受け手が未 install」の窓まで。bundle 評価
 * **前**に Rust が撃った分は `window.vpSidebarDispatch` 自体が居ないので届かない。sidebar の
 * state は変化のたびに撃ち直されるので、その窓の取りこぼしは次の push で埋まる
 * （main bundle 側の `ready` replay に相当するものは sidebar には無い）。
 */
import type { IpcEventEnvelope } from '../generated/SidebarIpc'

/** 受け手が揃うまでの保留箱。install 時に順序どおり流す。 */
let pending: IpcEventEnvelope[] | null = []

/** wire 名 → 実処理。`installSidebarDispatch` が受け取る。 */
export interface SidebarPushHandlers {
  state(state: unknown): void
  error(message: string): void
  performerCreateResult(repoPath: string, name: string, error: string | null): void
  agentsResult(repoPath: string, agents: unknown[], error: string | null): void
  filesListResult(address: string, entries: unknown[], truncated: boolean): void
  wireResult(payload: unknown): void
  clonePathPicked(path: string): void
  filePickerOpen(address: string): void
}

let handlers: SidebarPushHandlers | null = null

function apply(msg: IpcEventEnvelope): void {
  if (!handlers) return
  // ⚠️ `switch` の網羅性は TS が見る — schema に event を足して codegen を回すと、
  // ここに arm を足すまで型が通らない。**これが「境界に型が無い」の解消そのもの**。
  switch (msg.t) {
    case 'sidebar:state':
      handlers.state(msg.state)
      break
    case 'sidebar:error':
      handlers.error(msg.message)
      break
    case 'performer:create_result':
      // `error` は schema で optional — 「成功」が型に載る（旧: JS へ null 直書き）。
      handlers.performerCreateResult(msg.repo_path, msg.name, msg.error ?? null)
      break
    case 'agents:result':
      handlers.agentsResult(msg.repo_path, msg.agents, msg.error ?? null)
      break
    case 'files:list_result':
      handlers.filesListResult(msg.address, msg.entries, msg.truncated)
      break
    case 'wire:result':
      handlers.wireResult(msg.payload)
      break
    case 'clone:path_picked':
      handlers.clonePathPicked(msg.path)
      break
    case 'file_picker:open':
      handlers.filePickerOpen(msg.address)
      break
    default: {
      // 網羅していれば `never`。Rust 側が新しい event を撃ってきた（= 版ズレ）ときだけ来る。
      const unknown: never = msg
      console.warn('[vp-sidebar-dispatch] 未知の push', unknown)
    }
  }
}

/**
 * 受け口を window に生やす。**module 評価の先頭で呼ぶ**こと。
 *
 * ⚠️ ここの置き場所が保留箱の効き目を決める。`installSidebarDispatch` の直前に置くと
 * **保留窓が実質ゼロ**になる（bundle 評価中は受け口が無く、Rust 側の
 * `window.vpSidebarDispatch &&` guard が押し込みを黙って捨てる = 保留箱にすら入らない）。
 */
export function openSidebarDispatch(): void {
  ;(window as unknown as { vpSidebarDispatch: (m: IpcEventEnvelope) => void }).vpSidebarDispatch =
    (msg) => {
      if (pending) {
        pending.push(msg)
        return
      }
      apply(msg)
    }
}

/** 実処理を繋ぎ、保留分を**届いた順に**流す。 */
export function installSidebarDispatch(h: SidebarPushHandlers): void {
  handlers = h
  const queued = pending ?? []
  pending = null // 以降は直通
  for (const msg of queued) apply(msg)
  if (queued.length > 0) {
    console.info(`[vp-sidebar-dispatch] 保留 ${queued.length} 件を流した`)
  }
}
