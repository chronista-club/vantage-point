/**
 * sidebar の Repo (= Runtime Process) D&D 並べ替えロジック。
 *
 * #124。 Repo accordion をドラッグして表示順を変える。 並び順の SSOT は
 * Rust 側 `SessionState.currents_order` — drop 時に `process:reorder` IPC を送ると
 * Rust が永続化し、 次の `SidebarState` push で `currents_order` field に乗って戻る。
 * フロントは push された `processes` を `resolveRepoOrder` で並べ直して描画する。
 *
 * このモジュールは sidebar 全体で共有する drag state (どの Repo を掴んでいるか・
 * どこへ落とそうとしているか) と、 並び順を解決する pure function を提供する。
 * `RepoAccordion` が drag イベントを、 `Shell` が並び替え描画を、 それぞれ参照する。
 */
import { laneAddressKey } from "./lane"
import { createSignal } from 'solid-js'
import type { RepoPaneState } from '../generated/RepoPaneState'
import { sidebar, setCurrentsOrder } from './store'
import { sendIpc } from './ipc'

/** drop インジケータの位置 — target 行の手前 / 後ろ。 */
export type DropPos = 'before' | 'after'

/** ドラッグ中の Repo path。 null = ドラッグなし。 */
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
 * - 載っていない path (並べ替え後に追加された Repo 等) は元の相対順を保ったまま末尾。
 * - `currents_order` が無い / 空なら `processes` のコピーをそのまま返す。
 *
 * `Array.prototype.sort` は ES2019 以降 stable なので、 同 rank の要素 (= 末尾送り)
 * は入力順を保つ。
 */
export function resolveRepoOrder(
  processes: readonly RepoPaneState[],
  currentsOrder: readonly string[] | null | undefined,
): RepoPaneState[] {
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
 * 正しく配列末尾になる (#124 「末尾 Repo に落とせない」 の解消)。 `dragged` /
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
 * 解決順は全 Repo を含む (タブ分割は撤去済、 Shell は 1 リストで全 repo を表示)。
 */
/**
 * ドラッグ中の Lane。 null = ドラッグなし。
 *
 * repo の drag state と分けるのは、並べ替えの単位が違うため (repo 間の
 * 並べ替えと lane 間の並べ替えは互いに drop 先になれない)。 `path` を持つのは
 * **同じ repo 内でしか落とせない**ことを drop 側で判定するため。
 */
export const [dragLane, setDragLane] = createSignal<{
  path: string
  address: string
} | null>(null)

/** lane の drop 先 `{ address, pos }`。 null = 有効な drop 先の上にいない。 */
export const [laneDropMark, setLaneDropMark] = createSignal<{
  address: string
  pos: DropPos
} | null>(null)

/** lane drag 関連 signal を初期状態に戻す (dragend / drop 後に呼ぶ)。 */
export function clearLaneDrag(): void {
  setDragLane(null)
  setLaneDropMark(null)
}

/**
 * lane の drop を確定する action (doc 44 §12)。
 *
 * 現在の表示順 (= server が帳簿の順で並べた `lanes_by_repo`) を起点に
 * `moveInOrder` で並べ替え、`lane:reorder` IPC で **帳簿へ保存する**。
 *
 * ⚠️ repo の並べ替え (`commitRepoReorder`) と違い **楽観更新しない**。
 * 並び順の真実源は Host の帳簿で、楽観更新すると保存に失敗した時に UI だけが
 * 嘘をつく (開発起点 star と同じ規律、doc 44 §10.3)。反映は次の lanes snapshot で
 * 戻る — #835 で push の起床が直ったので即座に届く。
 */
export function commitLaneReorder(
  repoPath: string,
  dragged: string,
  target: string,
  pos: DropPos,
): void {
  const lanes = sidebar.lanes_by_repo?.[repoPath]
  if (!lanes || lanes.length === 0) return
  // ⚠️ 手組みしない — daemon 発行の key を返す `laneAddressKey` を通す。
  const current = lanes.map(laneAddressKey)
  const order = moveInOrder(current, dragged, target, pos)
  sendIpc({ t: 'lane:reorder', path: repoPath, order })
}

export function commitRepoReorder(dragged: string, target: string, pos: DropPos): void {
  const resolved = resolveRepoOrder(sidebar.processes, sidebar.currents_order)
  const order = moveInOrder(
    resolved.map((p) => p.path),
    dragged,
    target,
    pos,
  )
  setCurrentsOrder(order) // 楽観更新: 画面を即座に並び替える
  sendIpc({ t: 'process:reorder', order }) // 永続化: Rust の session_state へ
}
