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

/** 永続の実体。Phase 4 で `sendIpc` を差す。未接続なら no-op（Phase 1 の既定）。 */
let persist: ((items: readonly ActionItem[]) => void) | null = null;

/** 永続の実体を差し込む seam。 */
export function setActionPersist(
	fn: ((items: readonly ActionItem[]) => void) | null,
): void {
	persist = fn;
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
	persist?.(items);
	return true;
}

/** Rust push の受け口（Phase 3 で `dispatch` から呼ぶ）。壊れた入力は正して受ける。 */
export function applyActions(raw: unknown): void {
	setActionsSignal(normalizeActions(raw, newActionId));
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

/** 1 件消す。見つからなければ同一参照。 */
export function removeAction(id: string): readonly ActionItem[] {
	const cur = actions();
	return cur.some((i) => i.id === id) ? cur.filter((i) => i.id !== id) : cur;
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
