/**
 * New（産む動詞）の bundle 間 bridge — editor-host（edge rail）→ sidebar。
 *
 * ## 経路（doc 58 ④）
 *
 * edge rail の + New menu「lane を作る…」→ 本 module の window event →
 * sidebar が `runNewSub()`（active repo を解決して AddSub form を開く — `n` directive と
 * 同じ入口）を呼ぶ。form 本体は従来どおり名簿内の active repo に ephemeral に出る。
 *
 * ⚠️ event 名は bundle 間の契約 — 文字列を両側に直書きしない（#1003/#1004 型の drift 封じ、
 * `session-now-bridge.ts` と同じ流儀）。
 */

/** window event 名（bundle 間契約）。 */
export const OPEN_NEW_LANE_EVENT = "vp:open-new-lane";

/** editor-host 側（edge rail の New menu）が呼ぶ送り口。 */
export function emitOpenNewLane(): void {
	window.dispatchEvent(new CustomEvent(OPEN_NEW_LANE_EVENT));
}

/** sidebar 側が呼ぶ受け口。戻り値 = 解除関数。 */
export function onOpenNewLane(fn: () => void): () => void {
	const handler = () => fn();
	window.addEventListener(OPEN_NEW_LANE_EVENT, handler);
	return () => window.removeEventListener(OPEN_NEW_LANE_EVENT, handler);
}
