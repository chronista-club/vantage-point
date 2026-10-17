/**
 * R sidebar の中央の節 = 表示中 lane の repo の open PR 一覧（doc 82）。
 *
 * - 行は 2 段: 1 段目 `#番号 タイトル` / 2 段目 draft・CI・review・head branch
 * - **この lane の PR（slug 一致）は先頭固定**で強調
 * - click = ブラウザで開く（既存 `open-url` IPC — http(s) 限定の検査は Rust 側）、
 *   右 click = URL を clipboard へ
 * - 空なら「open PR なし」。gh 不在（`available:false`）なら節ごと出さない — 判断は
 *   right-sidebar.ts（節の外枠の持ち主）が `available()` を見て行う
 *
 * ## 層の分離
 *
 * - **data**: lane / 応答（signal）。応答は lane ごとに Map で覚える（戻った lane で即出す）
 * - **calculations**: pr-list.ts（parse / 先頭固定 / 2 段目の語彙、vitest 済）
 * - **actions**: mountPrSection（Solid render + open / copy）
 */
import { render } from "solid-js/web";
import { createSignal, For, Show } from "solid-js";
import { copyText } from "./clipboard";
import { type LanePrs, orderPrs, parseLanePrs, type Pr, prMeta } from "./pr-list";

export interface PrSectionApi {
	setLane(addr: string | null): void;
	/** push `lane:prs` の受け口。表示中でない lane の応答も覚えておく（rail の badge 用）。 */
	handle(lane: string, payload: unknown): LanePrs;
	/** 表示中 lane の最後の応答（未取得なら null）。 */
	current(): LanePrs | null;
}

export function mountPrSection(host: HTMLElement): PrSectionApi {
	const [lane, setLane] = createSignal<string | null>(null);
	const [byLane, setByLane] = createSignal<Map<string, LanePrs>>(new Map());
	const [copied, setCopied] = createSignal<number | null>(null);
	const data = () => {
		const l = lane();
		return l ? (byLane().get(l) ?? null) : null;
	};

	const open = (url: string): void => {
		const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc;
		ipc?.postMessage(JSON.stringify({ t: "open-url", url }));
	};
	let copiedTimer: ReturnType<typeof setTimeout> | undefined;
	const copy = (pr: Pr): void => {
		copyText(pr.url);
		setCopied(pr.number);
		clearTimeout(copiedTimer);
		copiedTimer = setTimeout(() => setCopied(null), 1200);
	};

	const Row = (p: { pr: Pr; pinned: boolean }) => (
		<button
			type="button"
			class="pr-row"
			classList={{ pinned: p.pinned }}
			title={`${p.pr.url}\nclick = ブラウザで開く / 右 click = URL をコピー`}
			onClick={() => open(p.pr.url)}
			onContextMenu={(ev) => {
				ev.preventDefault();
				copy(p.pr);
			}}
		>
			<div class="pr-l1">
				<span class="pr-num">#{p.pr.number}</span>
				<span class="pr-title">{p.pr.title}</span>
			</div>
			<div class="pr-l2">
				<Show when={p.pinned}>
					<span class="pr-chip tone-accent">この lane</span>
				</Show>
				<For each={prMeta(p.pr)}>
					{(c) => (
						<span class={`pr-chip tone-${c.tone}`} classList={{ "pr-head": c.kind === "head" }}>
							{c.label}
						</span>
					)}
				</For>
				<Show when={copied() === p.pr.number}>
					<span class="pr-copied">URL をコピーしました</span>
				</Show>
			</div>
		</button>
	);

	const Section = () => (
		<Show when={data()} fallback={<div class="pr-empty">読み込み中…</div>}>
			{(d) => {
				const order = () => orderPrs(d().prs, d().branch);
				return (
					<>
						<Show when={d().error}>
							<div class="pr-error" title={d().error ?? ""}>
								取得できませんでした: {d().error}
							</div>
						</Show>
						<Show when={!d().error && d().prs.length === 0}>
							<div class="pr-empty">open PR なし</div>
						</Show>
						<Show when={order().pinned}>{(p) => <Row pr={p()} pinned />}</Show>
						<For each={order().rest}>{(p) => <Row pr={p} pinned={false} />}</For>
					</>
				);
			}}
		</Show>
	);
	render(() => <Section />, host);

	return {
		setLane(addr) {
			setLane(addr);
		},
		handle(l, payload) {
			const parsed = parseLanePrs(payload);
			setByLane((prev) => new Map(prev).set(l, parsed));
			return parsed;
		},
		current: data,
	};
}

/** PR 節の行スタイル（節の外枠 / accordion の CSS は main_area.rs の #right-sidebar 系）。 */
export const PR_SECTION_CSS = `
.pr-row{ display:flex; flex-direction:column; gap:3px; width:100%; text-align:left;
  padding:6px 10px; border:0; border-left:2px solid transparent; border-radius:6px;
  background:transparent; cursor:pointer; color:var(--color-text-primary); }
.pr-row:hover{ background:var(--color-surface-bg-emphasis); }
.pr-row.pinned{ border-left-color:var(--color-brand-primary,#3b82f6);
  background:color-mix(in srgb, var(--color-brand-primary,#3b82f6) 10%, transparent); }
.pr-l1{ display:flex; gap:6px; align-items:baseline; min-width:0; font-size:11.5px; line-height:1.4; }
.pr-num{ flex:none; color:var(--color-text-tertiary);
  font-family:var(--vp-font-mono),var(--typography-family-mono); }
.pr-title{ min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.pr-l2{ display:flex; flex-wrap:wrap; gap:4px; align-items:center; min-width:0;
  font-family:var(--vp-font-mono),var(--typography-family-mono); font-size:10px; }
.pr-chip{ padding:0 6px; border-radius:4px; line-height:1.6;
  border:1px solid var(--color-surface-border-subtle); color:var(--color-text-secondary); }
.pr-chip.pr-head{ border-color:transparent; color:var(--color-text-tertiary);
  max-width:100%; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.pr-chip.tone-good{ color:#4ade80; border-color:color-mix(in srgb,#4ade80 40%,transparent); }
.pr-chip.tone-bad{ color:#f87171; border-color:color-mix(in srgb,#f87171 40%,transparent); }
.pr-chip.tone-warn{ color:#fbbf24; border-color:color-mix(in srgb,#fbbf24 40%,transparent); }
.pr-chip.tone-accent{ color:var(--color-brand-primary,#3b82f6);
  border-color:color-mix(in srgb,var(--color-brand-primary,#3b82f6) 50%,transparent); }
.pr-copied{ color:var(--color-text-secondary); }
.pr-empty, .pr-error{ padding:8px 12px; font-size:11px; color:var(--color-text-secondary); }
.pr-error{ color:#f87171; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
`;
