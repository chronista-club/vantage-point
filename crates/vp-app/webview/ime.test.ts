/**
 * `ime.ts` — IME の打鍵判別。**入力欄を持つ面すべてが読む 1 本**。
 *
 * ⚠️ ここが緩むと「日本語変換の確定 Enter が、そのまま送信 / 入力完了として通る」が
 * 復活する。VP は 2 度踏んでいる（chat 入力 #963 / ACTIONS 行 2026-08-07）ので、
 * **engine 別の 2 経路を両方**固定しておく。
 */
import { describe, expect, it } from "vitest";
import { isImeKeystroke } from "./ime";

describe("isImeKeystroke — IME の打鍵か（engine 別二段ガード）", () => {
	it("素の Enter は false（= 送信 / 完了してよい）", () => {
		expect(isImeKeystroke({ isComposing: false, keyCode: 13 })).toBe(false);
	});

	it("Blink/Gecko: 確定 keydown は isComposing=true で弾く", () => {
		expect(isImeKeystroke({ isComposing: true, keyCode: 13 })).toBe(true);
	});

	// WKWebView（wry = VP の実機）はこちら。compositionend が先に走り isComposing は
	// 既に false — keyCode 229 だけが痕跡。この経路の取りこぼしが #963 初版の退行
	// （日本語変換の確定でそのまま送信）の原因だった。
	it("⚠️ WebKit: isComposing=false でも keyCode 229 なら弾く", () => {
		expect(isImeKeystroke({ isComposing: false, keyCode: 229 })).toBe(true);
	});

	it("フィールド欠落（合成イベント等）は false = 進める側に倒す", () => {
		expect(isImeKeystroke({})).toBe(false);
	});
});
