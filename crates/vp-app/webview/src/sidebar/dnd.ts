/**
 * sidebar の Project (= Runtime Process) D&D 並べ替えロジック。
 *
 * #124。 Project accordion をドラッグして表示順を変える。 並び順の SSOT は
 * Rust 側 `SessionState.currents_order` — drop 時に `process:reorder` IPC を送ると
 * Rust が永続化し、 次の `SidebarState` push で `currents_order` field に乗って戻る。
 * フロントは push された `processes` を `resolveProjectOrder` で並べ直して描画する。
 *
 * このモジュールは sidebar 全体で共有する drag state (どの Project を掴んでいるか・
 * どこへ落とそうとしているか) と、 並び順を解決する pure function を提供する。
 * `ProjectAccordion` が drag イベントを、 `Shell` が並び替え描画を、 それぞれ参照する。
 */
import { createSignal } from 'solid-js'
import type { ProjectPaneState } from '../generated/ProjectPaneState'
import { sidebar, setCurrentsOrder } from './store'
import { sendIpc } from './ipc'

/** drop インジケータの位置 — target 行の手前 / 後ろ。 */
export type DropPos = 'before' | 'after'

/** ドラッグ中の Project path。 null = ドラッグなし。 */
export const [dragPath, setDragPath] = createSignal<string | null>(null)

/** drop 先の `{ path, pos }`。 null = 有効な drop 先の上にいない。 */
export const [dropMark, setDropMark] = createSignal<{ path: string; pos: DropPos } | null>(null)

/** drag 関連 signal を初期状態に戻す (dragend / drop 後に呼ぶ)。 */
export function clearDrag(): void {
  setDragPath(null)
  setDropMark(null)
}

/**
 * `processes` を `currents_order` に従って並べ替える pure function (calculation)。
 *
 * - `currents_order` に載っている path はその順序で並ぶ。
 * - 載っていない path (並べ替え後に追加された Project 等) は元の相対順を保ったまま末尾。
 * - `currents_order` が無い / 空なら `processes` のコピーをそのまま返す。
 *
 * `Array.prototype.sort` は ES2019 以降 stable なので、 同 rank の要素 (= 末尾送り)
 * は入力順を保つ。
 */
export function resolveProjectOrder(
  processes: readonly ProjectPaneState[],
  currentsOrder: readonly string[] | null | undefined,
): ProjectPaneState[] {
  if (!currentsOrder || currentsOrder.length === 0) return [...processes]
  const rank = new Map(currentsOrder.map((path, i) => [path, i]))
  const TAIL = Number.MAX_SAFE_INTEGER
  return [...processes].sort(
    (a, b) => (rank.get(a.path) ?? TAIL) - (rank.get(b.path) ?? TAIL),
  )
}

/**
 * `order` 配列内で `dragged` を `target` の手前 / 後ろへ移動した新しい order を返す
 * pure function (calculation)。
 *
 * `dragged` を一旦抜いてから `target` の位置を取り直すので、 `dragged` が `target`
 * より前にあっても後ろにあっても挿入位置がずれない。 末尾 `target` の `'after'` も
 * 正しく配列末尾になる (#124 「末尾 Project に落とせない」 の解消)。 `dragged` /
 * `target` が配列に無い、 または同一なら元配列のコピーを返す。
 */
export function moveInOrder(
  order: readonly string[],
  dragged: string,
  target: string,
  pos: DropPos,
): string[] {
  if (dragged === target) return [...order]
  if (order.indexOf(dragged) === -1 || order.indexOf(target) === -1) return [...order]
  const without = order.filter((p) => p !== dragged)
  const ti = without.indexOf(target)
  without.splice(pos === 'before' ? ti : ti + 1, 0, dragged)
  return without
}

/**
 * drop を確定する action。 現在の解決順を計算 → `moveInOrder` で並べ替え →
 * ① ローカル store の `currents_order` を即更新 (楽観更新) ② `process:reorder`
 * IPC を Rust へ送って永続化。
 *
 * ① が無いと、 Rust が re-push しない設計のため drop しても次の push まで並びが
 * 変わらない。 ① で `ordered()` メモが即再計算され `<For>` が即座に並び替わる。
 *
 * 解決順は running / paused タブ越しの全 Project を含む。 タブ表示は `Shell` 側の
 * filter なので、 全体 order のどこに挿しても表示タブ内の見た目順は正しくなる。
 */
export function commitProjectReorder(dragged: string, target: string, pos: DropPos): void {
  const resolved = resolveProjectOrder(sidebar.processes, sidebar.currents_order)
  const order = moveInOrder(
    resolved.map((p) => p.path),
    dragged,
    target,
    pos,
  )
  setCurrentsOrder(order) // 楽観更新: 画面を即座に並び替える
  sendIpc({ t: 'process:reorder', order }) // 永続化: Rust の session_state へ
}
