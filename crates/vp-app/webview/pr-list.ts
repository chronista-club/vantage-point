/**
 * R sidebar の PR 節の data / calculations（doc 82）。
 *
 * 描画（right-sidebar-prs.tsx）と取り直しの時刻表をここから分けてあるのは、どちらも
 * DOM 無しで vitest に乗せたいから。応答の形の持ち主は daemon の `lane::prs`（gh の形は
 * そこで畳まれていて、ここには来ない）。
 */

/** CI の畳み込み（daemon `lane::prs::ci_state` と同じ語彙）。 */
export type CiState = "pass" | "fail" | "pending" | "none";
/** review の状態（daemon `lane::prs::review_state` と同じ語彙）。 */
export type ReviewState = "approved" | "changes_requested" | "review_required" | "none";

export interface Pr {
	number: number;
	title: string;
	url: string;
	draft: boolean;
	head: string;
	ci: CiState;
	review: ReviewState;
}

export interface LanePrs {
	/** false = gh 不在 → PR 節ごと出さない（rail の PR ボタンも）。 */
	available: boolean;
	/** lane の cwd の現在の branch（先頭固定の材料）。 */
	branch: string | null;
	prs: Pr[];
	/** 取得失敗の理由（gh はあるが動かなかった / daemon に届かなかった）。 */
	error: string | null;
}

const CI: readonly CiState[] = ["pass", "fail", "pending", "none"];
const REVIEW: readonly ReviewState[] = [
	"approved",
	"changes_requested",
	"review_required",
	"none",
];

/**
 * push の payload（`unknown`）を [`LanePrs`] に写す。
 *
 * 形が壊れていても **error 付きの空**を返す — 例外で落とすと PR 節が「読み込み中」の
 * まま止まる。未知の語彙は `none` に倒す（版ズレで行ごと消えるより害が小さい）。
 */
export function parseLanePrs(payload: unknown): LanePrs {
	if (!payload || typeof payload !== "object") {
		return { available: true, branch: null, prs: [], error: "応答の形が不正" };
	}
	const p = payload as Record<string, unknown>;
	const prs = Array.isArray(p.prs) ? p.prs : [];
	return {
		available: p.available !== false,
		branch: typeof p.branch === "string" ? p.branch : null,
		error: typeof p.error === "string" ? p.error : null,
		prs: prs.flatMap((raw): Pr[] => {
			if (!raw || typeof raw !== "object") return [];
			const r = raw as Record<string, unknown>;
			if (typeof r.number !== "number" || typeof r.url !== "string" || !r.url) return [];
			return [
				{
					number: r.number,
					title: typeof r.title === "string" ? r.title : "",
					url: r.url,
					draft: r.draft === true,
					head: typeof r.head === "string" ? r.head : "",
					ci: CI.includes(r.ci as CiState) ? (r.ci as CiState) : "none",
					review: REVIEW.includes(r.review as ReviewState)
						? (r.review as ReviewState)
						: "none",
				},
			];
		}),
	};
}

/**
 * branch 名から slug を取り出す（branch-step の段 = prefix を外す）。
 * `wip/<slug>` と `review/<slug>` は同じ仕事 — `git next` で段が進んでも同じ PR を指す。
 */
export function branchSlug(branch: string): string {
	return branch.replace(/^(spike|exp|wip|review)\//, "");
}

/**
 * 「この lane の PR」を先頭に固定する。一致は slug で見る（lane は `wip/x` のまま、PR の
 * head は `review/x` という段違いが普通に起きる）。残りは daemon の並び（新しい順）のまま。
 */
export function orderPrs(
	prs: Pr[],
	branch: string | null,
): { pinned: Pr | null; rest: Pr[] } {
	if (!branch) return { pinned: null, rest: prs };
	const slug = branchSlug(branch);
	const pinned = prs.find((p) => branchSlug(p.head) === slug) ?? null;
	return { pinned, rest: pinned ? prs.filter((p) => p !== pinned) : prs };
}

/** 2 段目の 1 要素。tone は色の意味（good = 緑 / bad = 赤 / warn = 黄 / dim = 控えめ）。 */
export interface PrChip {
	kind: "draft" | "ci" | "review" | "head";
	label: string;
	tone: "good" | "bad" | "warn" | "dim";
}

/** 2 段目 = draft / CI / review / head branch の順。状態の無いもの（none）は出さない。 */
export function prMeta(pr: Pr): PrChip[] {
	const out: PrChip[] = [];
	if (pr.draft) out.push({ kind: "draft", label: "draft", tone: "dim" });
	if (pr.ci !== "none") {
		const ci = {
			pass: { label: "CI ✓", tone: "good" },
			fail: { label: "CI ✗", tone: "bad" },
			pending: { label: "CI …", tone: "warn" },
		}[pr.ci] as Pick<PrChip, "label" | "tone">;
		out.push({ kind: "ci", ...ci });
	}
	if (pr.review !== "none") {
		const rv = {
			approved: { label: "approved", tone: "good" },
			changes_requested: { label: "changes", tone: "bad" },
			review_required: { label: "review待ち", tone: "warn" },
		}[pr.review] as Pick<PrChip, "label" | "tone">;
		out.push({ kind: "review", ...rv });
	}
	out.push({ kind: "head", label: pr.head, tone: "dim" });
	return out;
}

/** 取り直しの間隔（doc 82 決定 6: 開いている間 60 秒ごと）。 */
export const PR_POLL_MS = 60_000;

export interface PrPollerDeps {
	fetch(lane: string): void;
	intervalMs: number;
	setInterval(fn: () => void, ms: number): number;
	clearInterval(id: number): void;
}

/**
 * 消費者主導の取り直し（Debug Log の watch/unwatch と同じ型）。
 *
 * `update` に「いま見えているか（active）× どの lane か」を渡すたびに、変化があった時だけ
 * 動く: 見え始めた / lane が変わった = **即 1 回 + 間隔ごと**、見えなくなった = 止める。
 * 同じ状態の再通知では撃たない（開閉の bus が重複して届いても gh を連打しない）。
 */
export function createPrPoller(deps: PrPollerDeps) {
	let timer: number | null = null;
	let current: string | null = null;
	const stop = () => {
		if (timer !== null) deps.clearInterval(timer);
		timer = null;
		current = null;
	};
	return {
		update(s: { active: boolean; lane: string | null }): void {
			const want = s.active && s.lane ? s.lane : null;
			if (want === current) return;
			stop();
			if (!want) return;
			current = want;
			deps.fetch(want);
			timer = deps.setInterval(() => deps.fetch(want), deps.intervalMs);
		},
		/** 手動の取り直し（↻）。見えていない時は何もしない。 */
		refresh(): void {
			if (current) deps.fetch(current);
		},
	};
}
