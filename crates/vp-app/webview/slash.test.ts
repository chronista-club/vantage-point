/**
 * `slash.ts` — chat 入力の slash 補完の判断。
 *
 * ⚠️ ここで守るのは **「行頭でしか効かない」と「空白で引数に移る」** の 2 点。どちらも
 * 公式の仕様（"A command is only recognized at the start of your message." /
 * `/fix-issue 123 high` で `$0` `$1` に入る）を写したもので、崩すと path を打っただけで
 * palette が出る / 引数が打てない、のどちらかになる。
 */
import { describe, expect, it } from "vitest";
import {
	applyCompletion,
	filterSlashCommands,
	moveSelection,
	slashQuery,
} from "./slash";

describe("slashQuery（補完を出すか / 絞り込みの語）", () => {
	it("行頭の `/` で開く。語はまだ空", () => {
		expect(slashQuery("/")).toBe("");
	});

	it("打った分が絞り込みの語になる", () => {
		expect(slashQuery("/gitn")).toBe("gitn");
	});

	it("⚠️ 行頭でなければ出さない（path や URL で邪魔しない）", () => {
		expect(slashQuery("src/sidebar/form.ts を読んで")).toBeNull();
		expect(slashQuery("https://example.com/x")).toBeNull();
		expect(slashQuery(" /compact")).toBeNull(); // 先頭の空白も行頭ではない
	});

	it("⚠️ 空白が来たら閉じる（そこから先は引数）", () => {
		expect(slashQuery("/fix-issue ")).toBeNull();
		expect(slashQuery("/fix-issue 123 high")).toBeNull();
	});

	it("⚠️ 改行も空白扱い（1 行目だけがコマンド行）", () => {
		expect(slashQuery("/compact\nそのあと続き")).toBeNull();
	});

	it("空文字は開かない", () => {
		expect(slashQuery("")).toBeNull();
	});
});

describe("filterSlashCommands（近い順）", () => {
	const all = [
		"clear",
		"compact",
		"code-review",
		"chronista-style:codeflow",
		"gitnexus-guide",
	];

	it("語が空なら全部（順序は元のまま）", () => {
		expect(filterSlashCommands(all, "")).toEqual(all);
	});

	it("前方一致が先に来る", () => {
		expect(filterSlashCommands(all, "c")[0]).toBe("clear");
	});

	it("⚠️ 同順位は短い順 — 1 打で長い名前空間付きに追い越されない", () => {
		// `c` は 4 つとも前方一致（`chronista-style:codeflow` は `:` の後ろで一致）。
		// 辞書順だけで並べると `chronista-style:codeflow` が先頭に来て邪魔になる。
		expect(filterSlashCommands(all, "c")).toEqual([
			"clear",
			"compact",
			"code-review",
			"chronista-style:codeflow",
		]);
	});

	it("⚠️ `plugin:skill` は `:` の後ろでも前方一致する", () => {
		// user は plugin 名を覚えていない。`codeflow` で引けないと実用にならない。
		expect(filterSlashCommands(all, "codeflow")).toEqual([
			"chronista-style:codeflow",
		]);
	});

	it("部分一致も拾うが、前方一致より後ろに置く", () => {
		const r = filterSlashCommands(all, "review");
		expect(r).toEqual(["code-review"]);
	});

	it("大文字小文字を区別しない", () => {
		expect(filterSlashCommands(all, "COMPACT")).toEqual(["compact"]);
	});

	it("一致なしは空", () => {
		expect(filterSlashCommands(all, "zzz")).toEqual([]);
	});
});

describe("moveSelection（端で巻く）", () => {
	it("下へ / 上へ", () => {
		expect(moveSelection(0, 1, 3)).toBe(1);
		expect(moveSelection(1, -1, 3)).toBe(0);
	});

	it("⚠️ 端は巻く（行き止まりを作らない）", () => {
		expect(moveSelection(2, 1, 3)).toBe(0);
		expect(moveSelection(0, -1, 3)).toBe(2);
	});

	it("候補ゼロでも壊れない", () => {
		expect(moveSelection(0, 1, 0)).toBe(0);
	});
});

describe("applyCompletion", () => {
	it("⚠️ 末尾に空白を足す（そのまま引数を打ち始められる）", () => {
		expect(applyCompletion("fix-issue")).toBe("/fix-issue ");
		// 空白が無いと slashQuery が開いたままで、次の打鍵が絞り込みに食われる。
		expect(slashQuery(applyCompletion("fix-issue"))).toBeNull();
	});
});
