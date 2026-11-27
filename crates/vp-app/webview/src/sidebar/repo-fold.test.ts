// mem_1ChNtdQKHrSB78Ppek2etg — lead 行の折りたたみ（sub lane 開閉）を見やすくする
import { describe, expect, it } from "vitest";
import { foldItemCount, foldedSummary, type FoldContent } from "./repo-fold";

const content = (over: Partial<FoldContent> = {}): FoldContent => ({
	hint: false,
	subs: 0,
	sessions: 0,
	form: false,
	...over,
});

describe("foldItemCount — lead 行の下に畳まれる物の数", () => {
	it("lead 1 本だけ（子が 0 件）なら 0 — caret を控えめにする根拠", () => {
		expect(foldItemCount(content())).toBe(0);
	});

	it("sub lane・追加セッション・開いている lane 作成フォームを足し合わせる", () => {
		expect(foldItemCount(content({ subs: 2 }))).toBe(2);
		expect(foldItemCount(content({ sessions: 1 }))).toBe(1);
		expect(foldItemCount(content({ form: true }))).toBe(1);
		expect(foldItemCount(content({ subs: 2, sessions: 3, form: true }))).toBe(6);
	});

	it("hint（repo stopped など）を出している間は、その 1 行が中身", () => {
		// hint の間は lane 行を描かない（RepoAccordion の Show fallback）ので、内訳は数えない
		expect(foldItemCount(content({ hint: true }))).toBe(1);
		expect(foldItemCount(content({ hint: true, subs: 2 }))).toBe(1);
	});
});

describe("foldedSummary — 畳んでいる間に lane 数の chip が語る内訳", () => {
	it("中身が無ければ語らない", () => {
		expect(foldedSummary(content())).toBeNull();
	});

	it("hint だけの時も語らない（畳んで隠れる lane は無い）", () => {
		expect(foldedSummary(content({ hint: true, subs: 2 }))).toBeNull();
	});

	it("sub lane / 追加セッション / フォームを、ある物だけ並べる", () => {
		expect(foldedSummary(content({ subs: 2 }))).toBe("sub lane 2 本を畳んでいます");
		expect(foldedSummary(content({ sessions: 1 }))).toBe("追加セッション 1 個を畳んでいます");
		expect(foldedSummary(content({ subs: 3, sessions: 1, form: true }))).toBe(
			"sub lane 3 本・追加セッション 1 個・lane 作成フォームを畳んでいます",
		);
	});
});
