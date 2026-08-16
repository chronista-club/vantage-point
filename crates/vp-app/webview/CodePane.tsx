/**
 * CodePane — code pane（コードブラウザ P1）の中身。
 *
 * 旧 sidebar File Explorer（ephemeral overlay picker）の後継。overlay と違い **lane の
 * pane として常設**（kind "code"、lane に 1 枚・session 直交 — board と同族）。
 * 「探す・見る」を 1 つの場所で（board への投擲は**オミット** — 旧 picker の投擲経路は
 * board 化 #771 の時点で受け手が消えて無音で死んでおり、用途が見えたら repo の `show`
 * method 経由で作り直す。mako 2026-08-16「無駄なものは作らない」）:
 *
 * - 左列 = 検索 input + tree（▶/▼）/ fuzzy flat list（↑↓/Enter）
 * - 右列 = 選択 file の内容（行番号 gutter + escape 済み `<pre>`）
 * - 名札 = 開いている rel_path + ✕
 *
 * ## 配線（entry.tsx が結ぶ）
 *
 * - demand: pane open（`vp:code-view`）で `code:list`、file 選択で `code:read` を送る
 *   （⚠️ 2 tag とも app.rs `is_main_ipc_tag` の allowlist と対）
 * - 供給: `code:entries` / `code:file` push が `handleEntries` / `handleFile` に届く
 *   （dispatch.ts → entry.tsx の配線）。⚠️ **lane 不一致の結果は捨てる** — 旧 FileExplorer
 *   の race 防御（他 lane の遅延到着が現 lane の表示を上書きする）を引き継ぐ
 *
 * ## P2（tree-sitter symbol outline）への備え
 *
 * entries の `kind` は switch + 未知 kind skip で受ける（種類が増えても無害に無視）。
 * symbol は別 event で届く予定なので、この component の受け口は増える方向で拡張する。
 */

import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import { render } from "solid-js/web";
import { boardKeyOf } from "./lane-panes";
import { toggleCodeOpen } from "./code-view";

// ============================================================================
// data（型 — 形の持ち主は Rust `file_explorer::Entry` / `read_file`）
// ============================================================================

export interface Entry {
	rel_path: string;
	kind: "dir" | "file";
	size?: number;
}

/** `code:file` の payload（`{"text"} | {"error"}` の 2 択 — read_file の返り値）。 */
export type FilePayload = { text?: string; error?: string };

interface DisplayItem {
	entry: Entry;
	depth: number;
}

// ============================================================================
// calculations（純関数 — vitest 対象。fuzzy / tree は旧 FileExplorer から移植）
// ============================================================================

/**
 * Fuzzy matcher。query の各文字を path に**順序保持の部分マッチ**で探す。
 * 連続マッチ / basename hit / path-separator/underscore/hyphen の境界 hit にボーナス。
 * 全文字マッチしなければ `null`。
 */
export function fuzzyScore(q: string, path: string): number | null {
	if (!q) return 0;
	const ql = q.toLowerCase();
	const pl = path.toLowerCase();
	const basenameStart = pl.lastIndexOf("/") + 1;
	let qi = 0;
	let score = 0;
	let prev = -2;
	for (let i = 0; i < pl.length && qi < ql.length; i++) {
		if (pl[i] === ql[qi]) {
			score += 10;
			if (i === prev + 1) score += 8; // 連続
			if (i >= basenameStart) score += 5; // basename
			const before = i === 0 ? "/" : pl[i - 1];
			if (before === "/" || before === "-" || before === "_" || before === ".") {
				score += 3; // boundary
			}
			prev = i;
			qi++;
		}
	}
	return qi === ql.length ? score : null;
}

/**
 * ツリー表示用に entries を絞り込む: expanded 集合に含まれる dir 配下のみ visible。
 * 各 entry の祖先 dir をすべて expand していないと表示しない（典型的な tree expansion）。
 */
export function buildTreeView(
	all: Entry[],
	expandedSet: Set<string>,
): DisplayItem[] {
	const out: DisplayItem[] = [];
	for (const entry of all) {
		const parts = entry.rel_path.split("/");
		let visible = true;
		for (let i = 1; i < parts.length; i++) {
			const ancestor = parts.slice(0, i).join("/");
			if (!expandedSet.has(ancestor)) {
				visible = false;
				break;
			}
		}
		if (!visible) continue;
		out.push({ entry, depth: parts.length - 1 });
	}
	return out;
}

/**
 * Fuzzy 表示用: file entries を score 順にソートして上位 100 件を flat list で。
 * dir は除外（検索の文脈で dir を select する意味は薄い）。
 */
export function buildFuzzyView(all: Entry[], q: string): DisplayItem[] {
	const scored: { entry: Entry; score: number }[] = [];
	for (const entry of all) {
		if (entry.kind !== "file") continue;
		const s = fuzzyScore(q, entry.rel_path);
		if (s !== null) scored.push({ entry, score: s });
	}
	scored.sort((a, b) => b.score - a.score);
	return scored.slice(0, 100).map(({ entry }) => ({ entry, depth: 0 }));
}

/** 行番号 gutter の文字列（"1\n2\n…N"）。空文字は 1 行として数える（<pre> の見た目と一致）。 */
export function gutterFor(text: string): string {
	const n = text === "" ? 1 : text.split("\n").length;
	let out = "";
	for (let i = 1; i <= n; i++) out += i === n ? `${i}` : `${i}\n`;
	return out;
}

// ============================================================================
// actions（mount + IPC）
// ============================================================================

/** main bundle → Rust の生 IPC（board-handler.ts と同じ薄い送り口）。 */
function sendIpc(msg: Record<string, unknown>): void {
	const ipc = (
		window as unknown as { ipc?: { postMessage(m: string): void } }
	).ipc;
	if (!ipc) return;
	try {
		ipc.postMessage(JSON.stringify(msg));
	} catch {
		// 送れない時に UI を壊さない（次の操作で再送される demand 型なので握り潰しで足る）
	}
}

export interface CodePaneController {
	/** 表示 lane の切替（address、null = lane 不在）。表示 cache を新 lane 用に張り替える。 */
	setLane(address: string | null): void;
	/** `code:entries` push の受け口。lane 不一致は捨てる。 */
	handleEntries(lane: string, entries: Entry[], truncated: boolean): void;
	/** `code:file` push の受け口。lane 不一致は捨てる。 */
	handleFile(lane: string, relPath: string, payload: FilePayload): void;
}

export function mountCodePane(host: HTMLElement): CodePaneController {
	const [lane, setLaneSignal] = createSignal<string | null>(null);
	const [entries, setEntries] = createSignal<Entry[]>([]);
	const [truncated, setTruncated] = createSignal(false);
	const [loading, setLoading] = createSignal(false);
	const [query, setQuery] = createSignal("");
	const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
	const [selectedIndex, setSelectedIndex] = createSignal(0);
	// 「root auto-expand を実施するか」の 1-shot flag。open のたびに arm し、初回の
	// entries 受領で消費する（後着 result が user の expand 操作を上書きしない — 旧
	// FileExplorer moody-blues PR #439 Issue 2 の防御を移植）。
	const [pendingAutoExpand, setPendingAutoExpand] = createSignal(true);
	// 開いている file（右列）。
	const [openedPath, setOpenedPath] = createSignal<string | null>(null);
	const [payload, setPayload] = createSignal<FilePayload | null>(null);

	let inputRef: HTMLInputElement | undefined;
	let listRef: HTMLDivElement | undefined;

	const view = createMemo<DisplayItem[]>(() =>
		query()
			? buildFuzzyView(entries(), query())
			: buildTreeView(entries(), expanded()),
	);

	/** pane が開いた（または開いた状態で lane が来た）時の demand。 */
	const demand = (): void => {
		const addr = lane();
		if (addr === null) return;
		setLoading(true);
		setPendingAutoExpand(true);
		sendIpc({ t: "code:list", lane: addr });
		// WKWebView では動的表示直後の focus が効かないことがある（旧 FileExplorer の慣行）。
		setTimeout(() => inputRef?.focus(), 0);
	};

	// pane の開閉は code-view.ts が SSOT。自 lane の open=true を見たら demand する。
	// （表示自体は lane-panes が host の display で行う — この component は中身だけ。）
	const onViewEvent = (e: Event): void => {
		const d = (e as CustomEvent<{ lane: string; open: boolean }>).detail;
		const addr = lane();
		if (addr === null || d.lane !== boardKeyOf(addr)) return;
		if (d.open) demand();
	};
	document.addEventListener("vp:code-view", onViewEvent);
	onCleanup(() => document.removeEventListener("vp:code-view", onViewEvent));

	const openFile = (relPath: string): void => {
		const addr = lane();
		if (addr === null) return;
		setOpenedPath(relPath);
		setPayload(null); // 前の file の内容を新 file に見せない（到着まで空）
		sendIpc({ t: "code:read", lane: addr, rel_path: relPath });
	};

	const activate = (item: DisplayItem): void => {
		if (item.entry.kind === "dir") {
			const next = new Set(expanded());
			if (next.has(item.entry.rel_path)) next.delete(item.entry.rel_path);
			else next.add(item.entry.rel_path);
			setExpanded(next);
			return;
		}
		openFile(item.entry.rel_path);
	};

	const moveSelection = (delta: number): void => {
		const items = view();
		if (items.length === 0) return;
		const next = Math.min(
			Math.max(selectedIndex() + delta, 0),
			items.length - 1,
		);
		setSelectedIndex(next);
		// 選択が viewport 外へ消えないよう追随（100 件 fuzzy list で ↓ 連打する時）。
		listRef
			?.querySelector(`[data-idx="${next}"]`)
			?.scrollIntoView({ block: "nearest" });
	};

	const onKeyDown = (e: KeyboardEvent): void => {
		if (e.key === "ArrowDown") {
			e.preventDefault();
			moveSelection(1);
		} else if (e.key === "ArrowUp") {
			e.preventDefault();
			moveSelection(-1);
		} else if (e.key === "Enter") {
			e.preventDefault();
			const item = view()[selectedIndex()];
			if (item) activate(item);
		} else if (e.key === "Escape") {
			e.preventDefault();
			// query が残っていれば 1 段目 = 検索 clear、空なら 2 段目 = pane close。
			if (query()) {
				setQuery("");
				setSelectedIndex(0);
			} else {
				toggleCodeOpen();
			}
		}
	};

	const controller: CodePaneController = {
		setLane(address: string | null): void {
			if (address === lane()) return;
			setLaneSignal(address);
			// 別 lane の表示を持ち越さない（cache は lane ごとに作り直す。
			// 開いたままの lane へ戻った時は vp:code-view の再通知 → demand で復元）。
			setEntries([]);
			setTruncated(false);
			setQuery("");
			setExpanded(new Set<string>());
			setSelectedIndex(0);
			setOpenedPath(null);
			setPayload(null);
		},
		handleEntries(evLane: string, evEntries: Entry[], evTruncated: boolean) {
			if (evLane !== lane()) return; // 他 lane の遅延到着は捨てる
			// P2 で kind が増えても無害に無視する（未知 kind は skip）。
			const known = evEntries.filter(
				(e) => e.kind === "dir" || e.kind === "file",
			);
			setEntries(known);
			setTruncated(evTruncated);
			setLoading(false);
			setSelectedIndex(0);
			if (pendingAutoExpand()) {
				setPendingAutoExpand(false);
				const rootDirs = known
					.filter((e) => e.kind === "dir" && !e.rel_path.includes("/"))
					.map((e) => e.rel_path);
				setExpanded(new Set(rootDirs));
			}
		},
		handleFile(evLane: string, relPath: string, evPayload: FilePayload) {
			if (evLane !== lane()) return;
			if (relPath !== openedPath()) return; // 前の file の遅延到着を上書きさせない
			setPayload(evPayload);
		},
	};

	const Pane = () => (
		<div class="code-pane" onKeyDown={onKeyDown}>
			<div class="code-plate">
				<span class="code-plate-title">Code</span>
				<span class="code-plate-path">{openedPath() ?? ""}</span>
				<button
					type="button"
					class="code-plate-btn"
					title="閉じる（⌘ hold f）"
					onClick={() => toggleCodeOpen()}
				>
					✕
				</button>
			</div>
			<div class="code-body">
				<div class="code-list-col">
					<input
						ref={inputRef}
						class="code-search"
						type="text"
						placeholder="fuzzy 検索…"
						value={query()}
						onInput={(e) => {
							setQuery(e.currentTarget.value);
							setSelectedIndex(0);
						}}
					/>
					<Show when={truncated()}>
						<div class="code-truncated-warn">
							⚠ 20,000 件で打ち切り — 検索で絞ってください
						</div>
					</Show>
					<div class="code-list" ref={listRef}>
						<Show when={!loading()} fallback={<div class="code-hint">読込中…</div>}>
							<For each={view()}>
								{(item, i) => (
									<div
										class="code-row"
										classList={{ selected: i() === selectedIndex() }}
										data-idx={i()}
										style={{ "padding-left": `${8 + item.depth * 14}px` }}
										onClick={() => {
											setSelectedIndex(i());
											activate(item);
										}}
									>
										<span class="code-row-icon">
											{item.entry.kind === "dir"
												? expanded().has(item.entry.rel_path)
													? "▼"
													: "▶"
												: "·"}
										</span>
										<span class="code-row-name">
											{query()
												? item.entry.rel_path
												: (item.entry.rel_path.split("/").pop() ?? "")}
										</span>
									</div>
								)}
							</For>
						</Show>
					</div>
				</div>
				<div class="code-content-col">
					<Show
						when={openedPath()}
						fallback={<div class="code-hint">file を選ぶと内容が出ます</div>}
					>
						<Show when={payload()} fallback={<div class="code-hint">読込中…</div>}>
							{(p) => (
								<Show
									when={p().error === undefined}
									fallback={<div class="code-error">{p().error}</div>}
								>
									<div class="code-src-scroll">
										{/* gutter は user-select:none — コピーに行番号が混ざらない。
										    同一 scroll 容器内の 2 <pre> なので行位置は常に一致する。 */}
										<pre class="code-gutter">{gutterFor(p().text ?? "")}</pre>
										<pre class="code-src">{p().text ?? ""}</pre>
									</div>
								</Show>
							)}
						</Show>
					</Show>
				</div>
			</div>
		</div>
	);

	render(() => <Pane />, host);
	return controller;
}

// ============================================================================
// CSS（entry.tsx が <style> 注入 — LANE_HEADER_CSS と同 pattern）
// ============================================================================

export const CODE_PANE_CSS = `
.code-pane {
  display: flex; flex-direction: column; height: 100%; min-height: 0;
  background: var(--color-bg-base, #101018);
  color: var(--color-text, #d8d8e0);
  font-size: 12.5px;
}
.code-plate {
  display: flex; align-items: center; gap: 8px; padding: 4px 8px;
  border-bottom: 1px solid var(--color-border-subtle, #26263a);
  flex: none;
}
.code-plate-title { font-weight: 600; letter-spacing: 0.04em; }
.code-plate-path {
  flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; opacity: 0.7; font-family: var(--typography-family-mono, monospace);
}
.code-plate-btn {
  flex: none; background: none; border: 1px solid var(--color-border-subtle, #26263a);
  color: inherit; border-radius: 4px; padding: 1px 8px; cursor: pointer; font-size: 11px;
}
.code-plate-btn:hover { background: var(--color-bg-emphasis, #202036); }
.code-body { display: flex; flex: 1; min-height: 0; }
.code-list-col {
  display: flex; flex-direction: column; width: 240px; flex: none; min-height: 0;
  border-right: 1px solid var(--color-border-subtle, #26263a);
}
.code-search {
  margin: 6px; padding: 4px 8px; flex: none;
  background: var(--color-bg-subtle, #181826); color: inherit;
  border: 1px solid var(--color-border-subtle, #26263a); border-radius: 4px;
  font-size: 12px; outline: none;
}
.code-search:focus { border-color: var(--color-accent, #7c6cff); }
.code-truncated-warn { flex: none; padding: 2px 8px; font-size: 11px; color: #e0b050; }
.code-list { flex: 1; min-height: 0; overflow-y: auto; }
.code-row {
  display: flex; align-items: center; gap: 6px; padding: 2px 8px;
  cursor: pointer; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.code-row:hover { background: var(--color-bg-emphasis, #202036); }
.code-row.selected { background: var(--color-bg-emphasis, #26263e); }
.code-row-icon { flex: none; width: 12px; opacity: 0.6; font-size: 10px; }
.code-row-name { overflow: hidden; text-overflow: ellipsis; }
.code-content-col { flex: 1; min-width: 0; min-height: 0; display: flex; }
.code-hint { margin: auto; opacity: 0.5; }
.code-error { margin: auto; color: #e07060; padding: 12px; }
.code-src-scroll {
  flex: 1; min-width: 0; overflow: auto; display: flex; align-items: flex-start;
}
.code-src-scroll pre {
  margin: 0; padding: 8px 0; font-family: var(--typography-family-mono, monospace);
  font-size: 12px; line-height: 1.45;
}
.code-gutter {
  flex: none; padding: 8px 8px 8px 12px !important; text-align: right;
  opacity: 0.35; user-select: none; -webkit-user-select: none;
}
.code-src { flex: 1; padding-right: 12px !important; }
`;
