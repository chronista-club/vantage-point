/**
 * 「今なにを」(now-line) の bundle 間 bridge — editor-host → sidebar。
 *
 * ## 経路（doc 58 ②-a）
 *
 * `vp now` → daemon `session_now` → `NowLine` conversation event → `console:event` push
 * → chatview の `foldInto` が session の `ChatState.nowLine` に畳む（ここまで既存）
 * → **chatview が本 module で tee** → sidebar 名簿の 2 行目（進行の本体）。
 *
 * ## ⚠️ なぜ CustomEvent か / なぜこの 1 枚に固めるか
 *
 * - editor-host bundle と sidebar bundle は**同一 document**（doc 48 step 3a）なので、
 *   Rust を経由せず window event で足りる（配線ゼロ・順序も 1 document 内で自明）。
 * - event 名 / detail 形 / 鍵合成は **bundle 間の契約**。文字列を両側に直書きすると
 *   rename で片側だけ変わり無音で断線する（#1003/#1004 の取り残しと同型）ので、
 *   両 bundle がこの 1 module を import する形で drift を構造的に封じる。
 *
 * ## 「今」の契約（doc 51 §1 A3）
 *
 * text=null は「今は無い」= 消す指示。「今」は turn より長生きしないので、
 * turn を閉じる event で null が流れてくる。
 */

/** window event 名（bundle 間契約）。 */
export const SESSION_NOW_EVENT = "vp:session-now";

/** tee の中身。lane は daemon 発行の address key（`<repo>/lane/<name>`）。 */
export type SessionNowDetail = {
	lane: string;
	session: number;
	/** null = 「今」は無い（turn 閉鎖 / engine 途絶）— 受け側は消す。 */
	text: string | null;
};

/** 名簿側の map 鍵。session を `#` で継ぐ（`vp lane slots` の表示形と同じ語彙）。 */
export function sessionNowKey(lane: string, session: number): string {
	return `${lane}#${session}`;
}

/** editor-host 側（chatview）が呼ぶ送り口。 */
export function emitSessionNow(detail: SessionNowDetail): void {
	window.dispatchEvent(new CustomEvent(SESSION_NOW_EVENT, { detail }));
}

/** sidebar 側が呼ぶ受け口。戻り値 = 解除関数（今は使い手なし、対称性のため）。 */
export function onSessionNow(fn: (d: SessionNowDetail) => void): () => void {
	const handler = (e: Event) => fn((e as CustomEvent<SessionNowDetail>).detail);
	window.addEventListener(SESSION_NOW_EVENT, handler);
	return () => window.removeEventListener(SESSION_NOW_EVENT, handler);
}
