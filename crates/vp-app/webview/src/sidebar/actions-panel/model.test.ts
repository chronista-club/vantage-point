/**
 * ACTIONS の calculations の固定（doc 57）。
 *
 * ⚠️ この file は `model.ts` だけを import する。`model.ts` は creo-ui を `import type` でしか
 * 使っていないので、node 環境（vitest の既定）でも落ちない。値として import すると
 * `@chronista-club/creo-ui/controls` が module 評価時に `delegateEvents()` を呼び、
 * `window is not defined` で **import した瞬間に**失敗する。
 */
import { describe, expect, it } from "vitest";
import {
	BUCKETS,
	PANEL_BUCKETS,
	type ActKeyInput,
	type ActionItem,
	actKeyIntent,
	bodyOf,
	bucketOf,
	countUndone,
	itemsIn,
	normalizeActions,
	orderBetween,
	parseChecklist,
	remainingOf,
	titleOf,
} from "./model";

const item = (over: Partial<ActionItem> = {}): ActionItem => ({
	id: "a",
	text: "",
	bucket: "todos",
	order: "n",
	...over,
});

describe("BUCKETS", () => {
	it("固定 6 区画で、id が重複しない", () => {
		expect(BUCKETS).toHaveLength(6);
		expect(new Set(BUCKETS.map((b) => b.id)).size).toBe(6);
	});

	it("IDEAs / EVENTs は creo status を持たない（list_todos を汚さない）", () => {
		// doc 57 §3 の線引き。ここを active にすると mako の todo 一覧が思いつきで埋まる。
		const statusOf = (id: string) => BUCKETS.find((b) => b.id === id)?.creoStatus;
		expect(statusOf("ideas")).toBeNull();
		expect(statusOf("events")).toBeNull();
		expect(statusOf("nexts")).toBe("active");
		expect(statusOf("waits")).toBe("active");
		expect(statusOf("todos")).toBe("active");
		expect(statusOf("currents")).toBe("active");
	});

	it("v1 の描画対象は currents を除く 5 つ（そこは既存の repo 一覧）", () => {
		expect(PANEL_BUCKETS.map((b) => b.id)).toEqual([
			"nexts",
			"waits",
			"ideas",
			"events",
			"todos",
		]);
	});

	it("未知の bucket は todos へ丸める", () => {
		expect(bucketOf("nexts")).toBe("nexts");
		expect(bucketOf("nope")).toBe("todos");
		expect(bucketOf(undefined)).toBe("todos");
		expect(bucketOf(42)).toBe("todos");
	});
});

describe("titleOf / bodyOf", () => {
	it("1 行目がタイトル、2 行目以降が内容", () => {
		const i = item({ text: "doc 56 設定画面\n\n- [ ] A 形トグル" });
		expect(titleOf(i)).toBe("doc 56 設定画面");
		expect(bodyOf(i)).toBe("- [ ] A 形トグル");
	});

	it("1 行だけなら内容は空", () => {
		expect(bodyOf(item({ text: "単発の用事" }))).toBe("");
	});

	it("タイトルは前後の空白を落とす", () => {
		expect(titleOf(item({ text: "  余白あり  \n本文" }))).toBe("余白あり");
	});
});

describe("parseChecklist", () => {
	it("- [ ] / - [x] を拾い、大文字 X も完了とみなす", () => {
		expect(parseChecklist("- [ ] 未\n- [x] 済\n- [X] 済2")).toEqual([
			{ checked: false, label: "未" },
			{ checked: true, label: "済" },
			{ checked: true, label: "済2" },
		]);
	});

	it("チェックリストでない行は無視する（説明文と混在しても壊れない）", () => {
		expect(parseChecklist("ふつうの説明文\n- [ ] やること\nもう一行")).toEqual([
			{ checked: false, label: "やること" },
		]);
	});

	it("`*` 始まりとインデントも拾う", () => {
		expect(parseChecklist("  * [ ] ネスト")).toEqual([
			{ checked: false, label: "ネスト" },
		]);
	});
});

describe("remainingOf", () => {
	it("チェックリストなら未完の数", () => {
		expect(remainingOf(item({ text: "t\n- [ ] a\n- [x] b\n- [ ] c" }))).toBe(2);
	});

	it("チェックリストでなければ null（badge を出さない）", () => {
		expect(remainingOf(item({ text: "t\nただの説明" }))).toBeNull();
		expect(remainingOf(item({ text: "t" }))).toBeNull();
	});

	it("done の Action は数えない（完了の中の未完はもう用事ではない）", () => {
		expect(remainingOf(item({ text: "t\n- [ ] a", done: true }))).toBeNull();
	});
});

describe("countUndone", () => {
	const items = [
		item({ id: "1", text: "やる", bucket: "nexts" }),
		item({ id: "2", text: "済", bucket: "nexts", done: true }),
		item({ id: "3", text: "", bucket: "nexts" }), // 書きかけ
		item({ id: "4", text: "別区画", bucket: "ideas" }),
	];

	it("done と書きかけ（空 text）を除いて数える", () => {
		expect(countUndone(items, "nexts")).toBe(1);
	});

	it("区画ごとに独立", () => {
		expect(countUndone(items, "ideas")).toBe(1);
		expect(countUndone(items, "waits")).toBe(0);
	});
});

describe("itemsIn", () => {
	it("order 昇順で返す", () => {
		const items = [
			item({ id: "c", bucket: "nexts", order: "z" }),
			item({ id: "a", bucket: "nexts", order: "b" }),
			item({ id: "b", bucket: "nexts", order: "m" }),
			item({ id: "x", bucket: "ideas", order: "a" }),
		];
		expect(itemsIn(items, "nexts").map((i) => i.id)).toEqual(["a", "b", "c"]);
	});

	it("同 order は id で安定する（描画順が揺れない）", () => {
		const items = [
			item({ id: "b", bucket: "nexts", order: "n" }),
			item({ id: "a", bucket: "nexts", order: "n" }),
		];
		expect(itemsIn(items, "nexts").map((i) => i.id)).toEqual(["a", "b"]);
	});
});

describe("orderBetween", () => {
	it("2 つの間に入る key を作る", () => {
		const mid = orderBetween("a", "c");
		expect(mid > "a").toBe(true);
		expect(mid < "c").toBe(true);
	});

	it("隣り合っていて間が無くても、桁を伸ばして必ず作る", () => {
		// ここが要。「間が作れない」を呼び手に扱わせないための性質。
		const mid = orderBetween("a", "b");
		expect(mid > "a").toBe(true);
		expect(mid < "b").toBe(true);
	});

	it("先頭への挿入（prev = null）", () => {
		const first = orderBetween(null, "n");
		expect(first < "n").toBe(true);
	});

	it("末尾への挿入（next = null）", () => {
		const last = orderBetween("n", null);
		expect(last > "n").toBe(true);
	});

	it("空の区画（両端 null）でも key が出る", () => {
		expect(orderBetween(null, null)).not.toBe("");
	});

	it("⚠️ 間が存在しない入力（next = prev+\"0\"）でも不変条件を壊さない", () => {
		// `0` が最小桁なので "a" と "a0" の間に入る文字列は存在しない
		// （reference 実装が末尾 0 の key を禁じている理由）。VP は「必ず作れる」契約なので、
		// 事後条件が破れたら next の後ろへ回す — 並びは degrade するがソートは壊れない。
		for (const [lo, hi] of [
			["a", "a0"],
			["ab", "ab0"],
			["i", "i0"],
			["0", "00"],
		]) {
			const mid = orderBetween(lo, hi);
			expect(mid > lo, `${lo} < ${mid} が破れた`).toBe(true);
			// hi の後ろに出るのは許容（間が無いので）。ただし hi 未満を騙って返してはいけない。
			expect(mid !== hi, `${mid} が hi と同値`).toBe(true);
		}
	});

	it("先頭に 50 回挿し続けても順序が保たれる（上の鏡像）", () => {
		let hi = "n";
		const made: string[] = [];
		for (let n = 0; n < 50; n++) {
			const mid = orderBetween(null, hi);
			expect(mid < hi).toBe(true);
			made.unshift(mid);
			hi = mid;
		}
		expect([...made].sort()).toEqual(made);
	});

	it("同じ隙間に 50 回挿し続けても順序が保たれる（再採番なしの証明）", () => {
		let lo = "a";
		const hi = "b";
		const made: string[] = [];
		for (let n = 0; n < 50; n++) {
			const mid = orderBetween(lo, hi);
			expect(mid > lo).toBe(true);
			expect(mid < hi).toBe(true);
			made.push(mid);
			lo = mid;
		}
		// 生成順にソート済みであること = 並びが壊れていない
		expect([...made].sort()).toEqual(made);
	});
});

describe("normalizeActions", () => {
	let seq = 0;
	const newId = () => `gen-${seq++}`;

	it("配列でなければ空（表示ごと止めない）", () => {
		seq = 0;
		expect(normalizeActions(null, newId)).toEqual([]);
		expect(normalizeActions("nope", newId)).toEqual([]);
		expect(normalizeActions({ a: 1 }, newId)).toEqual([]);
	});

	it("id 欠落は採番、重複は振り直す（id は同一性の唯一の手掛かり）", () => {
		seq = 0;
		const out = normalizeActions(
			[{ id: "dup" }, { id: "dup" }, { text: "no id" }],
			newId,
		);
		expect(new Set(out.map((i) => i.id)).size).toBe(3);
		expect(out[0].id).toBe("dup");
	});

	it("bucket 不明は todos、order 欠落は末尾側", () => {
		seq = 0;
		const [only] = normalizeActions([{ id: "a", bucket: "zzz" }], newId);
		expect(only.bucket).toBe("todos");
		expect(only.order).toBe("z");
	});

	it("done は boolean に潰す（truthy な別値を通さない）", () => {
		seq = 0;
		const out = normalizeActions(
			[
				{ id: "a", done: 1 },
				{ id: "b", done: true },
			],
			newId,
		);
		expect(out[0].done).toBe(false);
		expect(out[1].done).toBe(true);
	});

	it("object でない要素は捨てる", () => {
		seq = 0;
		expect(normalizeActions([null, 3, "x", { id: "ok" }], newId)).toHaveLength(1);
	});

	it("冪等（normalize を 2 回かけても同じ）", () => {
		seq = 0;
		const once = normalizeActions([{ id: "a", text: "t", bucket: "nexts" }], newId);
		const twice = normalizeActions(once, newId);
		expect(twice).toEqual(once);
	});
});

describe("actKeyIntent", () => {
	const ev = (over: Partial<ActKeyInput> & { key: string }): ActKeyInput => ({
		metaKey: false,
		ctrlKey: false,
		altKey: false,
		shiftKey: false,
		empty: false,
		atStart: false,
		atEnd: false,
		composing: false,
		...over,
	});

	it("Enter で確定、⌘Enter で改行（VP のチャット入力と同じ体系）", () => {
		// 「Enter で送信 / Shift+Enter で改行」と同じ族。差し込みを捕まえる面では
		// 「書いたら元の作業へ戻る」が支配的なので Enter がそこに就く。
		expect(actKeyIntent(ev({ key: "Enter" }), true)).toBe("commit");
		expect(actKeyIntent(ev({ key: "Enter", metaKey: true }), true)).toBe("newline");
	});

	it("⚠️ IME 変換中はどのキーも拾わない（変換確定の Enter で入力が終わらない）", () => {
		// この gate が無いと日本語入力で 1 文字確定するたびに focus が抜ける。
		expect(actKeyIntent(ev({ key: "Enter", composing: true }), true)).toBeNull();
		expect(actKeyIntent(ev({ key: "ArrowUp", atStart: true, composing: true }), true)).toBeNull();
		expect(
			actKeyIntent(ev({ key: "Backspace", empty: true, atStart: true, composing: true }), true),
		).toBeNull();
	});

	it("⌥↑↓ で入れ替え（caret の位置に依らない）", () => {
		expect(actKeyIntent(ev({ key: "ArrowUp", altKey: true }), true)).toBe("move-up");
		expect(actKeyIntent(ev({ key: "ArrowDown", altKey: true }), true)).toBe("move-down");
		// 途中に caret があっても入れ替えは効く
		expect(
			actKeyIntent(ev({ key: "ArrowUp", altKey: true, atStart: false }), true),
		).toBe("move-up");
	});

	it("素の ↑↓ は **端に居るときだけ** 行を移る（複数行の中を動けるように）", () => {
		expect(actKeyIntent(ev({ key: "ArrowUp", atStart: true }), true)).toBe("focus-prev");
		expect(actKeyIntent(ev({ key: "ArrowDown", atEnd: true }), true)).toBe("focus-next");
		// 内容の途中では caret 移動を邪魔しない（2 行目から 1 行目へ戻れる）
		expect(actKeyIntent(ev({ key: "ArrowUp", atStart: false }), true)).toBeNull();
		expect(actKeyIntent(ev({ key: "ArrowDown", atEnd: false }), true)).toBeNull();
	});

	it("⌘↑↓ は入れ替えにしない（行頭 / 行末移動として OS に残す）", () => {
		// creo-ui は altKey || metaKey で拾うが、VP は altKey のみ。
		expect(actKeyIntent(ev({ key: "ArrowUp", metaKey: true, atStart: true }), true)).toBe(
			"focus-prev",
		);
		expect(actKeyIntent(ev({ key: "ArrowDown", metaKey: true, atEnd: true }), true)).toBe(
			"focus-next",
		);
	});

	it("Backspace は「空 かつ caret 先頭」のときだけ削除", () => {
		expect(actKeyIntent(ev({ key: "Backspace", empty: true, atStart: true }), true)).toBe("remove");
		expect(actKeyIntent(ev({ key: "Backspace", empty: false, atStart: true }), true)).toBeNull();
		expect(actKeyIntent(ev({ key: "Backspace", empty: true, atStart: false }), true)).toBeNull();
	});

	it("Esc も確定、無関係キーは null", () => {
		expect(actKeyIntent(ev({ key: "Escape" }), true)).toBe("commit");
		expect(actKeyIntent(ev({ key: "a" }), true)).toBeNull();
		expect(actKeyIntent(ev({ key: "Tab" }), true)).toBeNull(); // 深さは区画が決めるので使わない
	});

	it("非 Mac では Ctrl が修飾（Cmd hold の折り畳み）", () => {
		expect(actKeyIntent(ev({ key: "Enter", ctrlKey: true }), false)).toBe("newline");
		expect(actKeyIntent(ev({ key: "Enter", metaKey: true }), false)).toBe("commit");
	});
});
