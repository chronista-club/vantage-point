/**
 * Command Palette（⌘K）— GPUI 借用 epic #2 の visible payoff。
 *
 * `Cmd/Ctrl + K` で sidebar 全面 overlay を開き、 **全 Action（directive registry）** を
 * fuzzy 検索 + ↑↓ navigate + Enter で実行する。 各 Action の `run` は当面
 * `dispatchDirective(key)` に委譲（registry.ts）。 構造は LanePicker.tsx を踏襲（class prefix = `vp-cp-`）。
 */
import {
	For,
	Show,
	createEffect,
	createMemo,
	createSignal,
	onCleanup,
	onMount,
} from "solid-js";
import { ACTIONS } from "./actions/registry";
import type { Action } from "./actions/types";

declare global {
	interface Window {
		vpCommandPalette?: { open(): void };
	}
}

const isMac = navigator.platform.toUpperCase().includes("MAC");
const MOD = isMac ? "⌘" : "Ctrl+";

// module-scope singleton state
const [visible, setVisible] = createSignal(false);
const [query, setQuery] = createSignal("");
const [selectedIndex, setSelectedIndex] = createSignal(0);

function open(): void {
	setQuery("");
	setSelectedIndex(0);
	setVisible(true);
}

function dismiss(): void {
	setVisible(false);
}

/** Fuzzy matcher（LanePicker / FileExplorer と同じ簡易 score）。 全文字マッチしなければ `null`。 */
function fuzzyScore(q: string, target: string): number | null {
	if (!q) return 0;
	const ql = q.toLowerCase();
	const tl = target.toLowerCase();
	let qi = 0;
	let score = 0;
	let prev = -2;
	for (let i = 0; i < tl.length && qi < ql.length; i++) {
		if (tl[i] === ql[qi]) {
			score += 10;
			if (i === prev + 1) score += 8;
			const before = i === 0 ? " " : tl[i - 1];
			if (before === " " || before === "/" || before === "-" || before === "_")
				score += 3;
			prev = i;
			qi++;
		}
	}
	return qi === ql.length ? score : null;
}

/** title 優先で fuzzy、 無ければ description で低スコア。 */
function actionScore(q: string, a: Action): number | null {
	if (!q) return 0;
	const t = fuzzyScore(q, a.title);
	if (t !== null) return t + 100;
	const d = fuzzyScore(q, a.description);
	return d === null ? null : d;
}

function filterActions(q: string): Action[] {
	if (!q) return ACTIONS;
	const scored: { a: Action; s: number }[] = [];
	for (const a of ACTIONS) {
		const s = actionScore(q, a);
		if (s !== null) scored.push({ a, s });
	}
	scored.sort((x, y) => y.s - x.s);
	return scored.map(({ a }) => a);
}

export function CommandPalette() {
	const view = createMemo(() => filterActions(query()));

	let inputRef: HTMLInputElement | undefined;
	let bodyRef: HTMLDivElement | undefined;

	// ⌘K（Cmd/Ctrl + K）で open。 capture phase で xterm.js / inner input より先に取る。
	const onGlobalKey = (event: Event) => {
		const e = event as KeyboardEvent;
		const mod = isMac ? e.metaKey : e.ctrlKey;
		if (mod && !e.altKey && e.key.toLowerCase() === "k") {
			e.preventDefault();
			open();
		}
	};

	onMount(() => {
		window.vpCommandPalette = { open };
		window.addEventListener("keydown", onGlobalKey, true);
	});
	onCleanup(() => {
		if (window.vpCommandPalette?.open === open)
			window.vpCommandPalette = undefined;
		window.removeEventListener("keydown", onGlobalKey, true);
	});

	createEffect(() => {
		if (!visible()) return;
		setTimeout(() => inputRef?.focus(), 0);
	});

	createEffect(() => {
		selectedIndex();
		if (!visible() || !bodyRef) return;
		const sel = bodyRef.querySelector(
			".vp-cp-row.selected",
		) as HTMLElement | null;
		sel?.scrollIntoView({ block: "nearest" });
	});

	const activate = (a: Action) => {
		dismiss();
		// dismiss 後に run（picker overlay と被らないよう / focus を handler 側に渡す）
		a.run();
	};

	const onKeyDown = (e: KeyboardEvent) => {
		if (e.key === "Escape") {
			e.preventDefault();
			dismiss();
			return;
		}
		const items = view();
		if (items.length === 0) return;
		if (e.key === "ArrowDown") {
			e.preventDefault();
			setSelectedIndex((idx) => Math.min(idx + 1, items.length - 1));
		} else if (e.key === "ArrowUp") {
			e.preventDefault();
			setSelectedIndex((idx) => Math.max(idx - 1, 0));
		} else if (e.key === "Enter") {
			e.preventDefault();
			const item = items[selectedIndex()];
			if (item) activate(item);
		}
	};

	return (
		<Show when={visible()}>
			<div class="vp-command-palette" onKeyDown={onKeyDown}>
				<div class="vp-cp-backdrop" onClick={dismiss} />
				<div class="vp-cp-panel" onClick={(e) => e.stopPropagation()}>
					<div class="vp-cp-header">
						<span class="vp-cp-prompt">⌘K</span>
						<input
							ref={inputRef}
							class="vp-cp-search"
							type="text"
							placeholder="コマンドを検索..."
							value={query()}
							onInput={(e) => {
								setQuery(e.currentTarget.value);
								setSelectedIndex(0);
							}}
						/>
						<button
							class="vp-cp-close"
							onClick={dismiss}
							title="閉じる (Esc)"
							type="button"
						>
							✕
						</button>
					</div>
					<div class="vp-cp-body" ref={bodyRef}>
						<Show when={view().length === 0}>
							<div class="vp-cp-empty">マッチなし</div>
						</Show>
						<For each={view()}>
							{(a, idx) => (
								<div
									class="vp-cp-row"
									classList={{ selected: idx() === selectedIndex() }}
									onClick={() => {
										setSelectedIndex(idx());
										activate(a);
									}}
								>
									<div class="vp-cp-text">
										<span class="vp-cp-title">{a.title}</span>
										<span class="vp-cp-desc">{a.description}</span>
									</div>
									<Show when={a.key}>
										<span class="vp-cp-kbd">
											{MOD}
											{a.key?.toUpperCase()}
										</span>
									</Show>
								</div>
							)}
						</For>
					</div>
					<div class="vp-cp-footer">↑↓ 移動 · Enter 実行 · Esc 閉じる</div>
				</div>
			</div>
		</Show>
	);
}

/** Shell.tsx の SHELL_CSS に concat する CSS（class prefix = `vp-cp-`）。 */
export const COMMAND_PALETTE_CSS = `
.vp-command-palette{position:absolute;inset:0;z-index:10100;display:flex;justify-content:center;align-items:flex-start;}
.vp-cp-backdrop{position:absolute;inset:0;background:rgba(0,0,0,.5);}
.vp-cp-panel{position:relative;display:flex;flex-direction:column;width:100%;max-height:100%;
  background:var(--color-surface-bg-base);
  border-right:1px solid var(--color-surface-border,#1f2233);}
.vp-cp-header{flex:0 0 auto;display:flex;align-items:center;gap:6px;padding:8px;
  border-bottom:1px solid var(--color-surface-border,#1f2233);}
.vp-cp-prompt{flex:0 0 auto;font-size:11px;color:var(--color-brand-primary,#3fb9d4);
  font-weight:600;padding:0 2px;}
.vp-cp-search{flex:1;padding:5px 8px;
  border:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-subtle);color:var(--color-text-primary);
  border-radius:var(--radius-sm,6px);font-family:inherit;font-size:12px;}
.vp-cp-search:focus{outline:none;border-color:var(--color-brand-primary);}
.vp-cp-close{border:none;background:transparent;color:var(--color-text-tertiary);
  font-size:14px;cursor:pointer;padding:0 6px;border-radius:3px;}
.vp-cp-close:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
.vp-cp-body{flex:1;overflow-y:auto;padding:4px 0;}
.vp-cp-empty{padding:12px;text-align:center;color:var(--color-text-tertiary);font-size:11px;}
.vp-cp-row{display:flex;align-items:center;gap:8px;padding:6px 12px;cursor:pointer;}
.vp-cp-row:hover{background:var(--color-surface-bg-emphasis);}
.vp-cp-row.selected{background:var(--color-brand-primary-subtle,#1c2436);}
.vp-cp-text{flex:1;min-width:0;display:flex;flex-direction:column;gap:1px;}
.vp-cp-title{font-size:12px;color:var(--color-text-primary);
  overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-cp-row.selected .vp-cp-title{color:var(--color-brand-primary,#3fb9d4);}
.vp-cp-desc{font-size:10px;color:var(--color-text-tertiary);
  overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-cp-kbd{flex:0 0 auto;font-family:'VPMono',monospace;font-size:10px;
  color:var(--color-text-tertiary);
  border:1px solid var(--color-surface-border,#1f2233);
  border-radius:4px;padding:1px 5px;background:var(--color-surface-bg-subtle);}
.vp-cp-footer{flex:0 0 auto;padding:4px 8px;font-size:10px;text-align:center;
  color:var(--color-text-tertiary);
  border-top:1px solid var(--color-surface-border,#1f2233);}
`;
