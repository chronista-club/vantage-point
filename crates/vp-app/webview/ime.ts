/**
 * IME（日本語変換）の打鍵を見分ける — **入力欄を持つ面すべての共有**。
 *
 * ## なぜ 1 本にまとめてあるか
 *
 * 「変換確定の Enter が、そのまま送信/完了として通ってしまう」は VP が **2 度**踏んだ
 * （chat 入力 #963 / ACTIONS 行 2026-08-07）。**同じブラウザの事実**が原因なので、
 * 各面が自前で gate を書くと必ずどれかが古い形（`!isComposing` 単独）で取り残される。
 * 実際 ActionRow は `compositionstart/end` の自前フラグだけを持ち、WKWebView で素通りしていた。
 *
 * ⚠️ **将来 creo-ui へ出す候補**。framework 非依存の純関数で VP 固有の知識を含まない。
 * ただし出すのは「VP 内が 1 本になってから」— 先に外へ出すと 2 箇所目が古いまま残る。
 */

/**
 * この打鍵は IME のものか（= アプリ側の意味に読み替えてはいけない）。
 *
 * エンジンごとに痕跡が違うため**二段ガード**にする（片方だけでは必ず漏れる）:
 * - Blink / Gecko: 確定 keydown は compositionend より**前**に来て `isComposing: true`
 * - WebKit（wry = WKWebView は**こちら**）: compositionend の**後**に来て `isComposing` は
 *   既に false。ただし `keyCode === 229`（"IME processing" の遺産値）が立つ
 *
 * `keyCode` は deprecated だが、WebKit の確定 Enter を見分ける現実的な唯一の信号なので
 * 意図的に使う。`!e.isComposing` 単独ガード（#963 の初版）は WKWebView で素通りし、
 * 日本語変換の確定がそのまま送信になる退行を実際に起こした。
 *
 * ⚠️ Enter 専用ではない。変換中の ↑↓ は**候補の選択**なので、行の並べ替えや focus 移動に
 * 読み替えてはいけない。述語が見ているのは「IME が握っているか」であって key ではない。
 */
export function isImeKeystroke(e: {
	isComposing?: boolean;
	keyCode?: number;
}): boolean {
	return e.isComposing === true || e.keyCode === 229;
}
