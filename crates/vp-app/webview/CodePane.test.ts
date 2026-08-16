/**
 * `CodePane.tsx` の純関数部 — fuzzy / tree / gutter。
 *
 * ⚠️ fuzzy / tree は旧 sidebar FileExplorer からの**移植**で、これらの case は
 * 旧実装の実挙動を仕様化したもの（= 移植ミスでスコアリングや絞り込みが変わったら
 * ここが落ちる）。挙動を意図して変える時はこの test を先に書き替えること。
 */
import { describe, expect, it } from "vitest";
import {
	type Entry,
	buildFuzzyView,
	buildTreeView,
	fuzzyScore,
	gutterFor,
} from "./CodePane";

const e = (rel_path: string, kind: "dir" | "file" = "file"): Entry => ({
	rel_path,
	kind,
});

describe("fuzzyScore（旧 FileExplorer の実挙動を仕様化）", () => {
	it("全文字が順序保持でマッチしなければ null", () => {
		expect(fuzzyScore("xyz", "src/main.rs")).toBeNull();
		// ⚠️ 順序保持: 'r' は index 5 にあるが、その**後**に 'm' が無いので null。
		// （'m' 自体は path に居る — 「文字が全部ある」ではなく「順序どおりに現れる」が条件）
		expect(fuzzyScore("rm", "main.rs")).toBeNull();
		expect(fuzzyScore("ma", "main.rs")).not.toBeNull();
		expect(fuzzyScore("zzz", "abc")).toBeNull();
	});

	it("空 query は 0（= 全件マッチ、tree に切替わる前提の既定値）", () => {
		expect(fuzzyScore("", "anything")).toBe(0);
	});

	it("basename hit が dir hit より優遇される", () => {
		// 同じ 'main' でも basename に居る方が高い
		const inBase = fuzzyScore("main", "src/main.rs");
		const inDir = fuzzyScore("main", "main/lib.rs");
		expect(inBase).not.toBeNull();
		expect(inDir).not.toBeNull();
		expect(inBase as number).toBeGreaterThan(inDir as number);
	});

	it("連続マッチが飛び飛びより高い", () => {
		const consec = fuzzyScore("abc", "abc.txt");
		const spread = fuzzyScore("abc", "a1b2c3.txt");
		expect(consec as number).toBeGreaterThan(spread as number);
	});
});

describe("buildTreeView", () => {
	const all = [
		e("src", "dir"),
		e("src/main.rs"),
		e("src/sub", "dir"),
		e("src/sub/deep.rs"),
		e("README.md"),
	];

	it("未展開 dir 配下は隠れる（祖先すべての expand が必要）", () => {
		const view = buildTreeView(all, new Set());
		expect(view.map((v) => v.entry.rel_path)).toEqual(["src", "README.md"]);
	});

	it("src を展開すると直下だけ出る（孫は src/sub の展開も要る）", () => {
		const view = buildTreeView(all, new Set(["src"]));
		expect(view.map((v) => v.entry.rel_path)).toEqual([
			"src",
			"src/main.rs",
			"src/sub",
			"README.md",
		]);
	});

	it("depth = パス分節数 - 1（インデントの入力）", () => {
		const view = buildTreeView(all, new Set(["src", "src/sub"]));
		const deep = view.find((v) => v.entry.rel_path === "src/sub/deep.rs");
		expect(deep?.depth).toBe(2);
	});
});

describe("buildFuzzyView", () => {
	it("dir は除外・score 降順・上位 100 件で cap", () => {
		// ⚠️ filler は**飛び飛びマッチ**にする。`f0/aaa.txt` のような連続マッチは
		// `aaa.txt` と**同点**になり（i=0 の boundary ボーナスは path 先頭にも付く）、
		// 勝敗が sort の安定性依存になる — 同点を作らないのがこの test の作法。
		const many: Entry[] = [e("dir", "dir")];
		for (let i = 0; i < 150; i++) many.push(e(`a-x-a-x-a-${i}.txt`));
		many.push(e("aaa.txt")); // 連続 3 文字 = 飛び飛びより厳密に高い
		const view = buildFuzzyView(many, "aaa");
		expect(view.length).toBe(100);
		expect(view[0]?.entry.rel_path).toBe("aaa.txt");
		expect(view.every((v) => v.entry.kind === "file")).toBe(true);
		// fuzzy 表示は flat（depth 0 固定）
		expect(view.every((v) => v.depth === 0)).toBe(true);
	});
});

describe("gutterFor", () => {
	it("行数分の番号を改行区切りで返す", () => {
		expect(gutterFor("a\nb\nc")).toBe("1\n2\n3");
	});

	it("末尾改行は最終空行として数える（<pre> の見た目と一致）", () => {
		expect(gutterFor("a\n")).toBe("1\n2");
	});

	it("空文字は 1 行", () => {
		expect(gutterFor("")).toBe("1");
	});
});
