/**
 * ACTIONS の data + actions（doc 57）。
 *
 * ## 層
 *
 * - **data**: `actions` signal（木そのもの）と区画の開閉。この module が SSOT
 * - **calculations**: `model.ts`（純関数、実行時 import ゼロ）
 * - **actions**: `commitActions`（唯一の書き換え口）と永続の seam
 *
 * ## 永続の seam
 *
 * Phase 1 は **creo に繋がない**（リロードで消える）。`setActionPersist` に実体を差すのは
 * Phase 4 で、UI 側の変更はその 1 行だけで済む。こうしておくと Phase 1 は Rust に一切
 * 触らず単独でマージでき、しかも**永続を持たないので migration が一度も発生しない**。
 */
import { createSignal } from "solid-js";
import {
	type ActionItem,
	type BucketId,
	isLocalId,
	normalizeActions,
	orderBetween,
	itemsIn,
} from "./model";

// =============================================================================
// data
// =============================================================================

const [actions, setActionsSignal] = createSignal<ActionItem[]>([]);

/** component が読む reactive な現在値。 */
export { actions };

/** 区画の開閉。component scope を跨ぐので module-scope に置く（`form.ts` と同じ理屈）。 */
const [openBuckets, setOpenBuckets] = createSignal<ReadonlySet<BucketId>>(
	new Set<BucketId>(),
);
export { openBuckets };

export function toggleBucket(id: BucketId): void {
	const next = new Set(openBuckets());
	if (!next.delete(id)) next.add(id);
	setOpenBuckets(next);
}

// =============================================================================
// id — VP が発行する
// =============================================================================

/**
 * Action の id。**creo-ui の `createNodeId` は使わない** — VP の origin は `vp-asset://` で
 * secure context 外の可能性があり、`crypto.randomUUID` が無いと**モジュール連番に縮退**する。
 * 連番は app 再起動でリセットされるので、永続した id と衝突する（doc 57 §5）。
 */
export function newActionId(): string {
	const c = globalThis.crypto;
	if (c?.randomUUID) return `act-${c.randomUUID()}`;
	return `act-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

// =============================================================================
// actions
// =============================================================================

/** 永続 1 回分（doc 57 Phase 4）。Rust の `ActionsWrite` と同形。 */
export interface ActionsPersistPayload {
	/** 現在の一覧（差分ではなく全件）。 */
	items: readonly ActionItem[];
	/** user が明示的に消した id。⚠️ **不在からは決して削除を推論させない**。 */
	removed: readonly string[];
}

/** 永続の実体。`sidebar.tsx` の boot が `sendIpc` を差す。未接続なら no-op。 */
let persist: ((payload: ActionsPersistPayload) => void) | null = null;

/** 永続の実体を差し込む seam。 */
export function setActionPersist(
	fn: ((payload: ActionsPersistPayload) => void) | null,
): void {
	persist = fn;
}

/**
 * いま編集中（focus を持つ）行の id。**daemon push から守る対象**。
 *
 * これが要る理由は 2 つあり、どちらも「打っている最中に足元が入れ替わる」の防止:
 *
 * 1. 5s ごとの push が、往復前の古い text で編集中の行を上書きする
 * 2. **新規行を creo に上げると id が `act-…` → `mem_…` に変わる**。編集中に起きると
 *    行の同一性が飛ぶので、書きかけの新規行は persist の payload から外す（下記）
 */
const [editingId, setEditingId] = createSignal<string | null>(null);

/** 行が focus を得た。 */
export function beginEditing(id: string): void {
	setEditingId(id);
}

/**
 * 行が focus を失った。**書き終えた合図**なので、ここで 1 回 persist を撃つ。
 *
 * 書きかけの新規行は payload から外してあるので、この一撃が無いと
 * 「⌘b で捕まえて blur しただけの Action」が creo に永久に上がらない。
 */
export function endEditing(id: string): void {
	if (editingId() !== id) return; // 既に別の行へ移っている
	setEditingId(null);
	pushPersist(actions());
}

/**
 * 消した id の控え。**daemon の一覧から消えたのを確認するまで持ち続ける**。
 *
 * `watch`（Rust 側の coalesce）は途中の値を捨てるので、1 回でも payload から漏れると
 * その削除は永久に届かない。毎回まるごと載せ、**incoming に居なくなった時だけ**降ろす。
 * 「push が来たら clear」にすると、削除前に発射済みだった poll の応答が先に着いた瞬間に
 * 控えが消え、削除が無かったことになる。
 */
const pendingRemovals = new Set<string>();

/** payload を組んで永続へ渡す。 */
function pushPersist(items: readonly ActionItem[]): void {
	if (!persist) return;
	const editing = editingId();
	persist({
		// ⚠️ **書きかけの新規行は送らない**。送ると creo が id を採番し、次の push で
		// 編集中の行の id が差し替わる（focus と同一性が飛ぶ）。blur の `endEditing` が
		// 改めて送るので取りこぼさない。
		items: items.filter((i) => !(i.id === editing && isLocalId(i.id))),
		removed: [...pendingRemovals],
	});
}

/**
 * 唯一の書き換え口。
 *
 * 借りてきた木の純関数は**操作が不成立なら同一参照を返す**規約なので、ここで `===` を見れば
 * 「何も起きなかった」が判る。呼び手はこの戻り値で focus 移動を抑止する
 * （creo-ui `CUOutliner.tsx:92-97` の `commit` と同じ考え方）。
 */
export function commitActions(next: readonly ActionItem[]): boolean {
	if (next === actions()) return false;
	const items = next as ActionItem[];
	setActionsSignal(items);
	pushPersist(items);
	return true;
}

/**
 * daemon から届いた ACTIONS を取り込む（Phase 3）。**版が変わった時だけ**当てる。
 *
 * Rust push の唯一の受け口。壊れた入力は `normalizeActions` が正すので、そのまま渡してよい。
 *
 * ## なぜ版で門を作るか
 *
 * sidebar の state push は **5s ごと**に来るが、creo の取得は 30s 周期なので、同じ一覧が
 * 6 回撃ち返される。毎回当てると `setActionsSignal` が新しい配列を配り、
 * **編集中の行の値が書き戻されて caret が飛ぶ**（行は `<Index>` = 位置キーイングなので
 * DOM は残るが、値だけ差し戻る）。daemon は**内容が変わった時だけ** rev を上げるので、
 * ここで版を見れば「本当に変わった時」だけに絞れる。
 *
 * `rev === 0` は **未取得**（daemon 起動直後 / creo 未ログイン / 旧 daemon）。この時は何もしない
 * ので、creo に繋がっていない間の sidebar は Phase 1 の local 挙動のまま残る。
 *
 * ⚠️ 比較が `!==` で `>` でないのは、**daemon を再起動すると rev が 1 に戻る**から
 * （cache は memory 上）。単調増加を仮定すると再起動後の一覧が永久に届かない。
 */
let appliedRev = 0;

export function applyActionsFromDaemon(items: unknown, rev: unknown): void {
	const r = typeof rev === "number" && Number.isFinite(rev) ? rev : 0;
	if (r === 0) return; // 未取得 — 触らない
	if (r === appliedRev) return; // 同じ版 = 内容も同じ。編集中の行を撃ち返さない
	appliedRev = r;

	const incoming = normalizeActions(items, newActionId);

	// 消したはずの行が「削除前に発射された poll」で戻ってくることがある。控えに残っている
	// 間は表示から外し、**incoming から消えたのを見て初めて**控えを降ろす（= 削除が届いた証拠）。
	for (const id of [...pendingRemovals]) {
		if (!incoming.some((i) => i.id === id)) pendingRemovals.delete(id);
	}
	const next = incoming.filter((i) => !pendingRemovals.has(i.id));

	// 編集中の行だけは**手元を優先**する（往復前の古い text で上書きしない）。
	// まだ creo に上げていない新規行（payload から外している）はそもそも incoming に居ないので、
	// ここで足し戻さないと打っている最中に行ごと消える。
	const editing = editingId();
	if (editing !== null) {
		const mine = actions().find((i) => i.id === editing);
		if (mine) {
			const at = next.findIndex((i) => i.id === editing);
			if (at >= 0) next[at] = mine;
			else next.push(mine);
		}
	}
	setActionsSignal(next);
}

/**
 * 区画の末尾に空の Action を足して、その id を返す（呼び手が focus を移す）。
 *
 * `after` を渡すとその直後に挿す（Enter の挙動）。
 */
export function appendAction(bucket: BucketId, after?: string): string {
	const inBucket = itemsIn(actions(), bucket);
    const at = after ? inBucket.findIndex((i) => i.id === after) : -1;
	const prev = at >= 0 ? inBucket[at] : inBucket[inBucket.length - 1];
	const next = at >= 0 ? inBucket[at + 1] : undefined;
	const created: ActionItem = {
		id: newActionId(),
		text: "",
		bucket,
		order: orderBetween(prev?.order ?? null, next?.order ?? null),
	};
	commitActions([...actions(), created]);
	return created.id;
}

/**
 * 区画内で 1 つ動かす。端なら**同一参照を返す**（= 何も起きない）。
 *
 * 木の `moveUp` / `moveDown` を使わないのは、あれが「兄弟配列の中の入れ替え」で
 * こちらは `order` の付け替えだから — 並びの持ち主が違う（doc 57 §3）。
 */
export function moveAction(id: string, dir: -1 | 1): readonly ActionItem[] {
	const cur = actions();
	const self = cur.find((i) => i.id === id);
	if (!self) return cur;
	const inBucket = itemsIn(cur, self.bucket);
	const at = inBucket.findIndex((i) => i.id === id);
	const swapAt = at + dir;
	if (swapAt < 0 || swapAt >= inBucket.length) return cur; // 端 = 不成立

	// 移動先の「向こう隣」との間に入る order を作る（2 者の swap ではなく挿入で表す）。
	const target = inBucket[swapAt];
	const beyond = inBucket[swapAt + dir];
	const order =
		dir === -1
			? orderBetween(beyond?.order ?? null, target.order)
			: orderBetween(target.order, beyond?.order ?? null);
	return cur.map((i) => (i.id === id ? { ...i, order } : i));
}

/** 区画を移す（NEXTs → CURRENTs 等）。移動先の末尾に置く。 */
export function moveToBucket(
	id: string,
	bucket: BucketId,
): readonly ActionItem[] {
	const cur = actions();
	const self = cur.find((i) => i.id === id);
	if (!self || self.bucket === bucket) return cur;
	const last = itemsIn(cur, bucket).at(-1);
	const order = orderBetween(last?.order ?? null, null);
	return cur.map((i) => (i.id === id ? { ...i, bucket, order } : i));
}

/**
 * 1 件消す。見つからなければ同一参照。
 *
 * ⚠️ **creo 側では memory ごと消える**（mako 裁定 2026-08-04）。取り消せないので、消す意図は
 * ここで [`pendingRemovals`] に控え、**明示された id としてだけ** daemon に渡す。
 * 一覧からの不在で削除を推論させない（起動直後の短い一覧で全消しになる）。
 */
export function removeAction(id: string): readonly ActionItem[] {
	const cur = actions();
	if (!cur.some((i) => i.id === id)) return cur;
	// creo に上げていない行は向こうに無いので控えない（無駄な削除要求を出さない）。
	if (!isLocalId(id)) pendingRemovals.add(id);
	return cur.filter((i) => i.id !== id);
}

/** text を差し替える。同じ text なら同一参照（打鍵のたびに永続を撃たないため）。 */
export function setActionText(id: string, text: string): readonly ActionItem[] {
	const cur = actions();
	const self = cur.find((i) => i.id === id);
	if (!self || self.text === text) return cur;
	return cur.map((i) => (i.id === id ? { ...i, text } : i));
}

/** done を切り替える。 */
export function setActionDone(id: string, done: boolean): readonly ActionItem[] {
	const cur = actions();
	const self = cur.find((i) => i.id === id);
	if (!self || (self.done ?? false) === done) return cur;
	return cur.map((i) => (i.id === id ? { ...i, done } : i));
}
