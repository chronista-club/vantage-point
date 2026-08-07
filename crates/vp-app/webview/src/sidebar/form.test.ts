/**
 * `form.ts` の純関数 — 一時展開（`b` の ACTIONS 捕捉 mode）を戻すかの判定。
 *
 * ## ⚠️ ここで守るのは「一時的に変えたものは戻る」
 *
 * 形の変更は **必ず永続化される**（`applyForm` → `vp:sidebar-form` → shell-layout →
 * `session.json`）。これは画面と保存が食い違わないための性質なので崩さない。代わりに
 * 一時変更は戻す。戻し忘れると **slim が二度と復活しない**（2026-08-06 の実害:
 * slim 中に `b` を打つと full になり、app を再起動しても full のままだった）。
 *
 * 判定を純関数に出したのは、mode の状態遷移が `document` 無しでは踏めないため
 * （vitest は `environment: 'node'`）。分岐だけでもここで固定しておく。
 */
import { describe, expect, it } from "vitest";
import { formToRestoreOnExit } from "./form";

describe("formToRestoreOnExit（一時展開を戻すか）", () => {
	it("スリムから広げて取り消した → スリムへ戻す", () => {
		expect(formToRestoreOnExit("slim", false)).toBe("slim");
	});

	it("⚠️ 区画を選んだら畳まない（行に focus が当たっていて編集が続く）", () => {
		expect(formToRestoreOnExit("slim", true)).toBeNull();
	});

	it("元からフルなら何もしない（戻す先が無い）", () => {
		expect(formToRestoreOnExit("full", false)).toBeNull();
		expect(formToRestoreOnExit("full", true)).toBeNull();
	});

	it("⚠️ 記録が無ければ何もしない — 二重に抜けても畳まない", () => {
		// `exitCaptureMode` は記録を先に null へ戻す。timeout と keydown が競って
		// 2 回走っても、2 回目は「展開していない」として no-op になる必要がある。
		expect(formToRestoreOnExit(null, false)).toBeNull();
		expect(formToRestoreOnExit(null, true)).toBeNull();
	});
});

/**
 * ⚠️ **上の純関数が正しくても、呼ばれていなければ意味が無い。**
 *
 * 配線（capture mode の入り口と漏斗）は `document` / `window` を触るので
 * `environment: 'node'` の vitest では踏めない。判定だけ緑で配線が消えている状態を
 * 作らないよう、source を読んで**呼び出しの存在**を固定する
 * （`shell-layout.test.ts` の bus test と同じ流儀）。
 */
describe("capture mode の一時展開が配線されている", async () => {
	const src = await (
		await import("node:fs/promises")
	).readFile(new URL("./actions/handlers.ts", import.meta.url), "utf8");

	it("入り口で展開前の形を記録する（連打で上書きしない guard 付き）", () => {
		expect(src).toContain("formBeforeCapture === null");
		expect(src).toContain("formBeforeCapture = sidebarForm()");
	});

	it("漏斗で戻す先を計算して畳む", () => {
		expect(src, "判定を呼んでいない").toContain("formToRestoreOnExit(");
		expect(src, "畳んでいない").toContain("collapseSidebar()");
	});

	it("⚠️ 区画選択の枝だけ `selected` で抜ける（他は取り消し扱い）", () => {
		expect(src).toContain("exitCaptureMode(true)");
	});
});
