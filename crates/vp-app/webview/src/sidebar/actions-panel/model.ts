/**
 * ACTIONS の calculations（doc 57）— DOM も signal も知らない純関数だけを置く層。
 *
 * ## なぜ実行時 import をゼロにするか
 *
 * `@chronista-club/creo-ui/controls` は **module 評価時に `delegateEvents()` を呼ぶ**
 * （`window.document` 前提）。vitest は `environment: 'node'` で回っているので、値として
 * import すると **import した瞬間に落ちる**。この file は型だけを `import type` で借り、
 * 実行時の import を持たない — だから node 環境の vitest でそのまま検証できる。
 * 木の書き換え（`setText` / `setDone` …）を呼ぶのは描画側（ActionRow / BucketList）の仕事。
 *
 * ## data / calculations / actions
 *
 * ここは **calculations のみ**。data は `store.ts`、actions（IPC・DOM）は component 側。
 */
import type { OutlinerNode } from "@chronista-club/creo-ui/controls";

// =============================================================================
// 区画（1 層目）— doc 57 §2
// =============================================================================

/**
 * 区画の id。**Action は区画の間を移動する** = 区画は状態（doc 57 §1）。
 *
 * `currents` だけ性格が違う: そこには repo > lane（実体を持つ Action）も居るので、
 * 素の Action を置くかは Phase 5 の判断（doc 57 §7）。v1 の描画対象は残り 5 つ。
 */
export type BucketId = "currents" | "nexts" | "waits" | "ideas" | "events" | "todos";

export interface BucketDef {
	id: BucketId;
	/** サイドバーに出す見出し。既存の `CURRENTs` と同じ「大文字 + 末尾 s」の綴り。 */
	label: string;
	/** 一言の説明（hover 時の title）。 */
	hint: string;
	/**
	 * creo の `status` への写像（doc 57 §3）。
	 *
	 * `null` = status を付けない。IDEAs / EVENTs は task ではないので、`active` にすると
	 * mako の `list_todos` が思いつきで埋まる — **他人の道具を汚さないための線引き**。
	 */
	creoStatus: "active" | null;
}

/** 固定 6 区画。v1 では増減しない（増やすなら区画自体の永続が別途要る、doc 57 §7）。 */
export const BUCKETS: readonly BucketDef[] = [
	{ id: "currents", label: "CURRENTs", hint: "着手中", creoStatus: "active" },
	{ id: "nexts", label: "NEXTs", hint: "次にやる", creoStatus: "active" },
	{ id: "waits", label: "WAITs", hint: "待ち（人 / CI / 返事）", creoStatus: "active" },
	{ id: "ideas", label: "IDEAs", hint: "思いつき", creoStatus: null },
	{ id: "events", label: "EVENTs", hint: "予定", creoStatus: null },
	{ id: "todos", label: "TODOs", hint: "雑務", creoStatus: "active" },
] as const;

/**
 * v1 でサイドバーに描く区画（daemon の直上に並ぶ 5 つ）。
 *
 * `currents` を外すのは、そこが既存の repo 一覧そのものだから（doc 57 §2）。
 * 合流は Phase 5。
 */
export const PANEL_BUCKETS: readonly BucketDef[] = BUCKETS.filter(
	(b) => b.id !== "currents",
);

/** 未知の区画 id を既定へ丸める（creo 側の手編集や版ズレへの防波堤）。 */
export function bucketOf(raw: unknown): BucketId {
	return BUCKETS.some((b) => b.id === raw) ? (raw as BucketId) : "todos";
}

// =============================================================================
// Action（2 層目）
// =============================================================================

/**
 * ACTIONS の 1 件。creo-ui の `OutlinerNode` と**構造的に同型**にしてあるので、
 * 借りてきた木の純関数（`setText` / `setDone` / `moveUp` …）にそのまま渡せる。
 *
 * `children` を持たないのは深さが 2 段固定だから（区画が親を決める、doc 57 §2）。
 */
export interface ActionItem extends OutlinerNode {
	id: string;
	/** タイトル + 内容。1 行目がタイトル、2 行目以降が内容（doc 57 §3 の写像）。 */
	text: string;
	done?: boolean;
	bucket: BucketId;
	/** 区画内の並び。文字列比較で並ぶ（挿入で後続を再採番しない）。 */
	order: string;
}

/**
 * VP が採番した id か（= creo にまだ上がっていない）。
 *
 * ⚠️ **Rust の `creo::client::is_local_id` と対の述語**。片方だけ直すと、`act-` 付きの行が
 * 「既存」と読まれて PUT が 404 になる（軽症）か、creo の id が「新規」と読まれて
 * **同じ Action の memory が書くたびに増える**（重症）。増える側に倒さないため、
 * **両側とも「`act-` で始まるものだけを local と見る」**で揃えてある。
 */
export function isLocalId(id: string): boolean {
	return id.startsWith("act-");
}

/** 1 行目 = タイトル。空なら空文字（placeholder は描画側が出す）。 */
export function titleOf(item: Pick<ActionItem, "text">): string {
	const nl = item.text.indexOf("\n");
	return (nl === -1 ? item.text : item.text.slice(0, nl)).trim();
}

/** 2 行目以降 = 内容。無ければ空文字。 */
export function bodyOf(item: Pick<ActionItem, "text">): string {
	const nl = item.text.indexOf("\n");
	return nl === -1 ? "" : item.text.slice(nl + 1).replace(/^\n+/, "");
}

/** markdown のチェックリスト 1 行。 */
export interface ChecklistEntry {
	checked: boolean;
	label: string;
}

/**
 * 内容から `- [ ] / - [x]` を拾う。チェックリストでない行は無視する
 * （内容は「チェックリスト **または** 説明文」なので、混在しても壊れない）。
 */
export function parseChecklist(body: string): ChecklistEntry[] {
	const out: ChecklistEntry[] = [];
	for (const line of body.split("\n")) {
		const m = /^\s*[-*]\s+\[([ xX])\]\s?(.*)$/.exec(line);
		if (m) out.push({ checked: m[1] !== " ", label: m[2].trim() });
	}
	return out;
}

/**
 * 行の badge に出す「残り」。
 *
 * - 内容がチェックリストなら **未完のチェック数**
 * - そうでなければ `null`（badge を出さない）
 *
 * done の Action は数えない（完了したものの中の未完は、もう用事ではない）。
 */
export function remainingOf(item: Pick<ActionItem, "text" | "done">): number | null {
	if (item.done) return null;
	const entries = parseChecklist(bodyOf(item));
	if (entries.length === 0) return null;
	return entries.filter((e) => !e.checked).length;
}

/** 区画の見出しに出す未完件数（done を除く。空 text の書きかけは煽らないので数えない）。 */
export function countUndone(items: readonly ActionItem[], bucket: BucketId): number {
	return items.filter(
		(i) => i.bucket === bucket && !i.done && titleOf(i) !== "",
	).length;
}

/** 区画に属する Action を `order` 昇順で取り出す（同 order は id で安定化）。 */
export function itemsIn(
	items: readonly ActionItem[],
	bucket: BucketId,
): ActionItem[] {
	return items
		.filter((i) => i.bucket === bucket)
		.sort((a, b) => (a.order === b.order ? cmp(a.id, b.id) : cmp(a.order, b.order)));
}

function cmp(a: string, b: string): number {
	return a < b ? -1 : a > b ? 1 : 0;
}

// =============================================================================
// order — 区画内の並び
// =============================================================================

/**
 * `order` の桁に使う文字。辞書順 = 数の大小になるよう昇順に並べる。
 *
 * **挿入のたびに後続を再採番しない**ためのもので、要件は「2 つの key の間を必ず作れる」。
 * 数値 order だと 20 行の区画で 1 行動かすたびに 20 件を書き直すことになる。
 */
const ORDER_DIGITS = "0123456789abcdefghijklmnopqrstuvwxyz";

/**
 * `prev < 戻り値 < next` になる order を作る（どちらも `null` 可 = 端への挿入）。
 * 桁が隣り合って「間が無い」ときは 1 桁潜る。fractional indexing の midpoint と同じ手筋。
 *
 * ## ⚠️ 「間がそもそも存在しない」入力がある
 *
 * `next` が `prev + "0"` ちょうどの形（例 `prev="a"` / `next="a0"`）だと、**両者の間に入る
 * 文字列が存在しない** — `0` が最小桁なので `"a"` の後ろに何を足しても `"a0"` 以上になる。
 * これが fractional indexing の reference 実装が「末尾が `0` の key」を禁じている理由。
 *
 * VP はこの関数を「必ず作れる」契約で使っている（呼び手に失敗を扱わせない）ので、**事後条件を
 * 検査して、破れたら `next` の後ろに置く**。並びは degrade する（挿したかった位置の 1 つ後ろに
 * 出る）が、**ソート不変条件は決して壊れない**。黙って `next` より大きい値を返すより、
 * 「入らなかったので後ろに出た」の方が読み手に嘘をつかない。
 *
 * 自前で生成した key は常にこの形を避けるので、実際に踏むのは外部由来の `order`
 * （Phase 3 以降の creo 経由）が混ざったときだけ。
 */
export function orderBetween(prev: string | null, next: string | null): string {
	const mid = midpoint(prev ?? "", next);
	const okLow = prev === null || mid > prev;
	const okHigh = next === null || mid < next;
	if (okLow && okHigh) return mid;
	// 間が無かった: next の後ろへ回す（midpoint(x, null) は必ず x より大きい）。
	return next === null ? midpoint(prev ?? "", null) : midpoint(next, null);
}

/** `b === null` は「上限なし」。 */
function midpoint(a: string, b: string | null): string {
	// 呼び手の順序が壊れていた場合の保険 — key は必ず返す（並びが狂っても data は失わない）。
	if (b !== null && a >= b) return midpoint(a, null);

	if (b !== null) {
		// 共通接頭辞は持ち越して、差が出る桁まで潜る
		let n = 0;
		while ((a[n] ?? "0") === b[n]) n++;
		if (n > 0) return b.slice(0, n) + midpoint(a.slice(n), b.slice(n));
	}

	const digitA = a ? ORDER_DIGITS.indexOf(a[0]) : 0;
	const digitB = b !== null ? ORDER_DIGITS.indexOf(b[0]) : ORDER_DIGITS.length;

	// 間に桁がある → その真ん中で終わり
	if (digitB - digitA > 1) {
		return ORDER_DIGITS[Math.round(0.5 * (digitA + digitB))];
	}
	// 隣り合っている: hi 側が 2 桁以上なら、その先頭 1 桁が a より大きく hi より小さい
	if (b !== null && b.length > 1) return b.slice(0, 1);
	// それ以外は lo 側の桁を採用して 1 桁潜る（ここが「必ず作れる」の根拠）
	return ORDER_DIGITS[digitA] + midpoint(a.slice(1), null);
}

// =============================================================================
// 正規化 — creo / 版ズレから来る壊れた入力への防波堤
// =============================================================================

/**
 * 外から来た配列を `ActionItem[]` に正す。**壊れた入力で表示ごと止めない**ための自衛で、
 * data を捨てない方向に倒す（深さ超過を切るのではなく丸める、等）。
 *
 * - 配列でない / 要素が object でない → 捨てる
 * - `id` 欠落・重複 → 採番し直す（id は同一性の唯一の手掛かり）
 * - `bucket` 不明 → `todos` へ
 * - `order` 欠落 → 末尾側に置く
 */
export function normalizeActions(
	raw: unknown,
	newId: () => string,
): ActionItem[] {
	if (!Array.isArray(raw)) return [];
	const seen = new Set<string>();
	const out: ActionItem[] = [];
	for (const entry of raw) {
		if (typeof entry !== "object" || entry === null) continue;
		const e = entry as Record<string, unknown>;
		let id = typeof e.id === "string" && e.id !== "" ? e.id : newId();
		if (seen.has(id)) id = newId();
		seen.add(id);
		out.push({
			id,
			text: typeof e.text === "string" ? e.text : "",
			done: e.done === true,
			bucket: bucketOf(e.bucket),
			order: typeof e.order === "string" && e.order !== "" ? e.order : "z",
		});
	}
	return out;
}

// =============================================================================
// keydown の意図（DOM 非依存の純関数）
// =============================================================================

/**
 * ACTIONS の行で押されたキーの「意図」。
 *
 * `shortcuts/chord.ts` の `directiveKeyOf` と同じ流儀 — **判定を DOM から切り離して
 * vitest で固める**。Mac のローカル検証では見えない経路（修飾キーの折り畳み等）を
 * 机上で押さえられる。
 */
export type ActIntent =
	| "newline" // 同じ Action の中で改行（内容を書き足す）
	| "move-up" // 行ごと入れ替え
	| "move-down"
	| "focus-prev" // 行間の focus 移動
	| "focus-next"
	| "remove" // 空行を削って上へ
	| "commit"; // 入力を確定して抜ける

export interface ActKeyInput {
	key: string;
	metaKey: boolean;
	ctrlKey: boolean;
	altKey: boolean;
	shiftKey: boolean;
	/** 入力欄が空か（Backspace の判定に要る）。 */
	empty: boolean;
	/** caret が先頭にあるか。 */
	atStart: boolean;
	/** caret が末尾にあるか。 */
	atEnd: boolean;
	/** IME 変換中か。**変換確定の Enter を「確定」と誤読しないための gate**。 */
	composing: boolean;
}

/**
 * 行で押されたキーの意図（DOM 非依存）。
 *
 * ## Enter / ⌘Enter は VP のチャット入力と同じ体系（mako 指定 2026-08-03）
 *
 * VP の会話入力は「Enter で送信 / Shift+Enter で改行」。ACTIONS も同じ族に揃える —
 * **Enter = 入力を確定して抜ける / ⌘Enter = 改行**。差し込みを捕まえる面では
 * 「書いたら元の作業へ戻る」が支配的な動作なので、Enter がそこに就くのが自然。
 *
 * ⚠️ この結果、**キーボードから「次の行を足す」経路は無くなった**。行の追加は
 * `⌘ hold b`（捕捉 mode）か「追加」ボタン。捕捉 mode こそが設計上の入口なので損失は小さい。
 */
export function actKeyIntent(e: ActKeyInput, isMac: boolean): ActIntent | null {
	// ⚠️ 変換中はどのキーも IME のもの。ここを通すと変換確定の Enter で入力が終わる。
	if (e.composing) return null;
	const mod = isMac ? e.metaKey : e.ctrlKey;

	switch (e.key) {
		case "Enter":
			return mod ? "newline" : "commit";
		case "ArrowUp":
			// ⌥↑ で入れ替え。⌘↑ は行頭移動として OS に残す（creo は alt||meta だが VP は alt のみ）。
			if (e.altKey) return "move-up";
			// 複数行を持てるので、**先頭に居るときだけ**上の行へ移る。
			// そうしないと内容の 2 行目から 1 行目へ caret を戻せない。
			return e.atStart ? "focus-prev" : null;
		case "ArrowDown":
			if (e.altKey) return "move-down";
			return e.atEnd ? "focus-next" : null;
		case "Backspace":
			// 文字が残っている / caret が先頭でない間は通常の削除に任せる。
			return e.empty && e.atStart ? "remove" : null;
		case "Escape":
			return "commit";
		default:
			return null;
	}
}
