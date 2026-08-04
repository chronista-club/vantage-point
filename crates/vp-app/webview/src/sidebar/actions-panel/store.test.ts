/**
 * `actions-panel/store.ts` の **daemon push の取り込み口**（doc 57 Phase 3）。
 *
 * ここで守るのは「**版が変わった時だけ当てる**」の 1 点。sidebar の state push は 5s ごとに
 * 来るのに creo の取得は 30s 周期なので、素直に当てると同じ一覧を 6 回配り直すことになり、
 * **編集中の行の値が書き戻されて caret が飛ぶ**（行は uncontrolled input + 位置キーイングで、
 * model → DOM の同期は「値が食い違う時だけ書く」createEffect が担っている）。
 *
 * ⚠️ module-scope の `appliedRev` を持つので、test ごとに `vi.resetModules()` で読み直す
 * （`dispatch.test.ts` と同じ理由）。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

type Store = typeof import("./store");

/** creo 由来の 1 件（Rust `CreoAction` と同形の生 JSON）。 */
function raw(id: string, text: string) {
	return { id, text, done: false, bucket: "nexts", order: "a" };
}

describe("applyActionsFromDaemon", () => {
	let store: Store;

	beforeEach(async () => {
		vi.resetModules();
		store = await import("./store");
	});

	it("rev 0（未取得 / 未ログイン / 旧 daemon）では触らない", () => {
		store.commitActions([
			{ id: "act-local", text: "手元の思いつき", bucket: "ideas", order: "a" },
		]);
		store.applyActionsFromDaemon([raw("mem_1", "creo 側")], 0);
		// creo に繋がっていない間は Phase 1 の local 挙動のまま残る。
		expect(store.actions().map((i) => i.id)).toEqual(["act-local"]);
	});

	it("rev が上がったら当てる", () => {
		store.applyActionsFromDaemon([raw("mem_1", "一件目")], 1);
		expect(store.actions().map((i) => i.text)).toEqual(["一件目"]);

		store.applyActionsFromDaemon([raw("mem_1", "書き換えた")], 2);
		expect(store.actions().map((i) => i.text)).toEqual(["書き換えた"]);
	});

	it("⚠️ 同じ rev の再 push は当てない（編集中の行を撃ち返さない）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "creo の内容")], 7);
		// user が編集した（Phase 4 の書きが往復する前）。
		store.commitActions(store.setActionText("mem_1", "打鍵した途中"));
		// 5s 後の push — daemon の cache は変わっていないので rev は同じ。
		store.applyActionsFromDaemon([raw("mem_1", "creo の内容")], 7);
		expect(store.actions()[0].text).toBe("打鍵した途中");
	});

	it("⚠️ daemon 再起動で rev が戻っても当てる（単調増加を仮定しない）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "古い")], 9);
		// daemon 再起動 = cache は memory 上なので rev は 1 から採り直される。
		store.applyActionsFromDaemon([raw("mem_1", "新しい")], 1);
		expect(store.actions()[0].text).toBe("新しい");
	});

	it("rev が数でない（版ズレ / 壊れた push）は未取得と同じ扱い", () => {
		store.commitActions([{ id: "act-local", text: "残る", bucket: "todos", order: "a" }]);
		store.applyActionsFromDaemon([raw("mem_1", "x")], undefined);
		store.applyActionsFromDaemon([raw("mem_1", "x")], "3");
		store.applyActionsFromDaemon([raw("mem_1", "x")], Number.NaN);
		expect(store.actions().map((i) => i.id)).toEqual(["act-local"]);
	});

	it("creo が空を返したら空になる（logout / 全消しが伝わる）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "x")], 1);
		expect(store.actions()).toHaveLength(1);
		store.applyActionsFromDaemon([], 2);
		expect(store.actions()).toHaveLength(0);
	});

	it("creo 側の欠損（bucket / order なし）は normalizeActions が丸める", () => {
		// creo の UI から手で tag を付けた memory は `metadata.vp` を持たない。
		store.applyActionsFromDaemon([{ id: "mem_1", text: "手で引き取った" }], 1);
		expect(store.actions()[0]).toMatchObject({
			id: "mem_1",
			bucket: "todos", // 未知 → 既定の区画
			order: "z", // 未設定 → 末尾側
		});
	});

	it("⚠️ 編集中の行は daemon push で上書きされない", () => {
		store.applyActionsFromDaemon([raw("mem_1", "creo の内容")], 1);
		store.beginEditing("mem_1");
		store.commitActions(store.setActionText("mem_1", "打鍵した途中"));
		// 書きが往復する前に poll が古い内容を持って来た（rev は進んでいる）。
		store.applyActionsFromDaemon([raw("mem_1", "creo の内容")], 2);
		expect(store.actions()[0].text).toBe("打鍵した途中");
		// 抜ければ次の push は素直に当たる。
		store.endEditing("mem_1");
		store.applyActionsFromDaemon([raw("mem_1", "creo の内容")], 3);
		expect(store.actions()[0].text).toBe("creo の内容");
	});

	it("⚠️ 書きかけの新規行が push で消えない（creo にまだ居ないので incoming に無い）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "既存")], 1);
		store.commitActions([
			...store.actions(),
			{ id: "act-new", text: "捕まえた途中", bucket: "ideas", order: "b" },
		]);
		store.beginEditing("act-new");
		store.applyActionsFromDaemon([raw("mem_1", "既存")], 2);
		expect(store.actions().map((i) => i.id)).toEqual(["mem_1", "act-new"]);
	});
});

/**
 * 永続 payload（doc 57 Phase 4）。**削除が絡むので、ここが崩れると memory が消える**。
 */
describe("永続 payload", () => {
	let store: Store;
	let sent: { items: readonly unknown[]; removed: readonly string[] }[];

	beforeEach(async () => {
		vi.resetModules();
		store = await import("./store");
		sent = [];
		store.setActionPersist((p) => sent.push({ items: p.items, removed: p.removed }));
	});

	const last = () => sent[sent.length - 1];

	it("commit のたびに全件を載せる（差分ではない）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "a"), raw("mem_2", "b")], 1);
		store.commitActions(store.setActionText("mem_1", "書き換え"));
		expect(last().items).toHaveLength(2);
		expect(last().removed).toEqual([]);
	});

	it("⚠️ 消した id は `removed` に明示して載る（不在からは推論させない）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "a"), raw("mem_2", "b")], 1);
		store.commitActions(store.removeAction("mem_2"));
		expect(last().removed).toEqual(["mem_2"]);
		expect(last().items.map((i) => (i as { id: string }).id)).toEqual(["mem_1"]);
	});

	it("⚠️ 削除は daemon の一覧から消えるまで載り続ける（coalesce で落ちない）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "a"), raw("mem_2", "b")], 1);
		store.commitActions(store.removeAction("mem_2"));
		// 削除前に発射された poll が mem_2 を持って戻ってきた。
		store.applyActionsFromDaemon([raw("mem_1", "a"), raw("mem_2", "b")], 2);
		// 表示は蘇らない。
		expect(store.actions().map((i) => i.id)).toEqual(["mem_1"]);
		// 控えも降りていない（次の payload にまだ載る）。
		store.commitActions(store.setActionText("mem_1", "x"));
		expect(last().removed).toEqual(["mem_2"]);

		// creo 側で消えたのが見えたら控えを降ろす。
		store.applyActionsFromDaemon([raw("mem_1", "a")], 3);
		store.commitActions(store.setActionText("mem_1", "y"));
		expect(last().removed).toEqual([]);
	});

	it("creo に無い行（local id）の削除は要求を出さない", () => {
		store.commitActions([{ id: "act-1", text: "手元だけ", bucket: "ideas", order: "a" }]);
		store.commitActions(store.removeAction("act-1"));
		expect(last().removed).toEqual([]);
	});

	it("⚠️ 編集中の新規行は送らない（id が差し替わって足元が入れ替わるのを防ぐ）", () => {
		store.commitActions([{ id: "act-1", text: "捕", bucket: "ideas", order: "a" }]);
		store.beginEditing("act-1");
		store.commitActions(store.setActionText("act-1", "捕まえ"));
		expect(last().items).toHaveLength(0);

		// 抜けたら送る（= ここで初めて creo に上がる）。
		store.endEditing("act-1");
		expect(last().items.map((i) => (i as { id: string }).id)).toEqual(["act-1"]);
	});

	it("編集中でも既存行（creo id）は送る（id は変わらないので安全）", () => {
		store.applyActionsFromDaemon([raw("mem_1", "a")], 1);
		store.beginEditing("mem_1");
		store.commitActions(store.setActionText("mem_1", "書き換え"));
		expect(last().items).toHaveLength(1);
	});
});
