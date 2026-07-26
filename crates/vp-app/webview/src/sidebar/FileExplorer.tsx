/**
 * Sidebar File Explorer overlay picker。
 *
 * Lane の workdir 配下のファイルを ephemeral picker で探索し、 選択ファイルを
 * Canvas (Board) に投げ込むワンショット UI。 picker は file 選択 / Esc /
 * 背景クリックで dismiss する (常設 panel ではない)。
 *
 * ## UX
 * - 起動: LaneRow のフォルダアイコン click、 もしくは sidebar focus 中の
 *   Cmd+F (Mac) / Ctrl+F (他 OS)。 どちらも `window.vpFilePicker.open(address)` を呼ぶ。
 * - 検索 input 空: ツリー (▶ / ▼ で expand toggle)
 * - 入力あり: lane 全体の file を fuzzy 検索した flat list (basename hit +
 *   path-separator-adjacent hit にボーナス)
 * - ↑↓ で移動、 Enter で open、 Esc / 背景クリックで dismiss
 *
 * ## Rust 側との接続
 * - 送信: `sendIpc({ t: 'files:list', path, address })` /
 *   `sendIpc({ t: 'files:open', path, address, rel_path })`
 * - 受信: `window.vpFiles.handleListResult({ address, entries, truncated })`
 * - 起動 API: `window.vpFilePicker.open(address)` (LaneRow / Cmd+F handler が呼ぶ)
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
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { laneAddressKey } from "./lane";

interface Entry {
	rel_path: string;
	kind: "dir" | "file";
	size?: number;
}

interface ListResult {
	address: string;
	entries: Entry[];
	truncated: boolean;
}

declare global {
	interface Window {
		/**
		 * sidebar 外 (directive `f` handler / LaneRow フォルダボタン / Rust bridge) から picker を起動する API。
		 * - `open(address)`: lane の workdir で picker を open (= directive `f` の主動作)
		 * - `sendSelectedToPP()`: picker visible 中の selected file を Canvas に投擲 (= directive `p` の picker context 動作、 picker は dismiss しない)
		 */
		vpFilePicker?: {
			open(address: string): void;
			sendSelectedToPP(): void;
		};
		/** Rust → JS: files:list の結果を sidebar に push back する受け口。 */
		vpFiles?: { handleListResult(result: ListResult): void };
	}
}

// Picker は singleton 想定 (sidebar 全体に 1 つだけ overlay)。 state は module スコープに置き、
// component の `onMount` で window.vpFilePicker / vpFiles に attach する。 これにより
// LaneRow や Cmd+F handler が `window.vpFilePicker.open(...)` で起動できる。
const [visible, setVisible] = createSignal(false);
const [targetAddress, setTargetAddress] = createSignal("");
const [entries, setEntries] = createSignal<Entry[]>([]);
const [truncated, setTruncated] = createSignal(false);
const [loading, setLoading] = createSignal(false);
const [query, setQuery] = createSignal("");
const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
const [selectedIndex, setSelectedIndex] = createSignal(0);
// ピン留めモード: true の間は file 選択後も picker を閉じない (連続選択用)。
// pin を off にする (true → false) 操作自体が close の trigger も兼ねる
// (user 仕様「外すと close」)。 picker を新規 open するたびに false にリセット。
const [pinned, setPinned] = createSignal(false);
// 「root auto-expand を実施するか」 の 1-shot flag。 picker を新規 open するたびに true に
// リセットし、 1 回目の `handleListResult` で消費する。 これにより rapid-click 等で
// 同一 lane の files:list 結果が複数届いた時に user の expand 操作を後着 result が
// 上書きするのを防ぐ (moody-blues PR #439 final review Issue 2)。
const [pendingAutoExpand, setPendingAutoExpand] = createSignal(true);

/**
 * address (= LaneAddressWire::key() 形式) を持つ Lane が属する project path を
 * `sidebar.lanes_by_project` から逆引きする。 見つからなければ `undefined`。
 * LaneRow からは projectPath が直接渡せるが、 Cmd+F handler は active_lane_address
 * しか持っていないため逆引きが必要 (signatures を揃えるためここで吸収する)。
 */
function resolveProjectPath(address: string): string | undefined {
	const map = sidebar.lanes_by_project ?? {};
	for (const [projectPath, lanes] of Object.entries(map)) {
		if (!Array.isArray(lanes)) continue;
		if (lanes.some((l) => laneAddressKey(l) === address)) {
			return projectPath;
		}
	}
	return undefined;
}

function open(address: string): void {
	const projectPath = resolveProjectPath(address);
	if (!projectPath) {
		console.warn(
			"[FileExplorer] address に対応する project が見つかりません:",
			address,
		);
		return;
	}
	setTargetAddress(address);
	setEntries([]);
	setTruncated(false);
	setQuery("");
	setExpanded(new Set<string>());
	setSelectedIndex(0);
	setLoading(true);
	setPinned(false); // 新規 open 時はピン留めを必ずリセット (前回の pin が持ち越されない)
	setPendingAutoExpand(true); // 次回の handleListResult で root auto-expand を実施するよう arm
	setVisible(true);
	sendIpc({ t: "files:list", path: projectPath, address });
}

function dismiss(): void {
	setVisible(false);
	// dismiss 後に IPC 結果が遅れて到着しても無視できるよう loading を寝かせる。
	// (handleListResult 先頭の visible() ガードと合わせて二重防御。)
	setLoading(false);
}

function handleListResult(result: ListResult): void {
	// dismiss 後の遅延到着結果は捨てる: invisible state で signal を書き換えるのを防ぐ
	// (moody-blues PR #439 final review Issue 3)。
	if (!visible()) return;
	// 他 lane の結果が遅れて届く race を避ける: current target 以外は捨てる。
	if (result.address !== targetAddress()) return;
	setEntries(result.entries);
	setTruncated(result.truncated);
	setLoading(false);
	setSelectedIndex(0);
	// root auto-expand は picker open ごとに **初回 result のみ** 実施する。 同一 lane で
	// rapid-click した時、 2 回目以降の result が user の expand/collapse 操作を
	// 上書きする副作用を回避 (moody-blues PR #439 final review Issue 2)。
	if (pendingAutoExpand()) {
		setPendingAutoExpand(false);
		const rootDirs = result.entries
			.filter((e) => e.kind === "dir" && !e.rel_path.includes("/"))
			.map((e) => e.rel_path);
		setExpanded(new Set(rootDirs));
	}
}

/**
 * Fuzzy matcher。 query の各文字を path に **順序保持の部分マッチ** で探す。
 * 連続マッチ / basename hit / path-separator/underscore/hyphen の境界 hit にボーナス。
 * 全文字マッチしなければ `null`。
 */
function fuzzyScore(q: string, path: string): number | null {
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
			if (
				before === "/" ||
				before === "-" ||
				before === "_" ||
				before === "."
			) {
				score += 3; // boundary
			}
			prev = i;
			qi++;
		}
	}
	return qi === ql.length ? score : null;
}

interface DisplayItem {
	entry: Entry;
	depth: number;
}

/**
 * ツリー表示用に entries を絞り込む: expanded 集合に含まれる dir 配下のみ visible。
 * 各 entry の祖先 dir をすべて expand していないと表示しない (典型的な tree expansion)。
 */
function buildTreeView(all: Entry[], expandedSet: Set<string>): DisplayItem[] {
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
 * dir は除外 (検索の文脈で dir を select する意味は薄い)。
 */
function buildFuzzyView(all: Entry[], q: string): DisplayItem[] {
	const scored: { entry: Entry; score: number }[] = [];
	for (const entry of all) {
		if (entry.kind !== "file") continue;
		const s = fuzzyScore(q, entry.rel_path);
		if (s !== null) scored.push({ entry, score: s });
	}
	scored.sort((a, b) => b.score - a.score);
	return scored.slice(0, 100).map(({ entry }) => ({ entry, depth: 0 }));
}

export function FileExplorer() {
	const view = createMemo<DisplayItem[]>(() =>
		query()
			? buildFuzzyView(entries(), query())
			: buildTreeView(entries(), expanded()),
	);

	// 検索 input への ref。 `<Show>` で動的 mount された後に明示的に focus する。
	// wry の WKWebView では動的 insert 後の HTML `autofocus` 属性が効かない場合があり
	// (moody-blues PR 2 review Issue 2)、 createEffect + setTimeout(0) で focus を保証する。
	let inputRef: HTMLInputElement | undefined;
	// 結果 body への ref。 selectedIndex 変化時の scrollIntoView で使う
	// (moody-blues PR 2 review Issue 1: 100 件 fuzzy list で ↓ 連打すると選択が viewport 外に消える)。
	let bodyRef: HTMLDivElement | undefined;

	/**
	 * Directive `p` (= "send current/selected to board") の picker context 実装。
	 * picker visible + selected が file の時に file を Canvas に投げる。 ピン留めの有無に
	 * かかわらず picker は **dismiss しない** (= 規約 v0.3 §C.2 "連続選択を許す")。
	 * 投擲後は input に focus を戻し、 user が次の選択 / 検索 input 操作にすぐ進めるようにする。
	 */
	const sendSelectedToPP = () => {
		if (!visible()) return;
		const items = view();
		const item = items[selectedIndex()];
		if (!item || item.entry.kind !== "file") return;
		const address = targetAddress();
		const projectPath = resolveProjectPath(address);
		if (!projectPath) return;
		sendIpc({
			t: "files:open",
			path: projectPath,
			address,
			rel_path: item.entry.rel_path,
		});
		setTimeout(() => inputRef?.focus(), 0);
	};

	onMount(() => {
		window.vpFilePicker = { open, sendSelectedToPP };
		window.vpFiles = { handleListResult };
	});
	onCleanup(() => {
		if (window.vpFilePicker?.open === open) window.vpFilePicker = undefined;
		if (window.vpFiles?.handleListResult === handleListResult)
			window.vpFiles = undefined;
	});

	// picker が visible になったら input に focus。 0ms setTimeout で次 tick に移し、
	// WKWebView の動的 DOM insert 後でも安定的に focus が当たるようにする。
	createEffect(() => {
		if (!visible()) return;
		setTimeout(() => inputRef?.focus(), 0);
	});

	// selectedIndex が動いたら対応する .vp-fe-row.selected を viewport 内に scroll。
	// `block: 'nearest'` で最小スクロール量に抑える (上 / 下どちらの方向移動でも自然)。
	createEffect(() => {
		selectedIndex(); // track
		if (!visible() || !bodyRef) return;
		const sel = bodyRef.querySelector(
			".vp-fe-row.selected",
		) as HTMLElement | null;
		sel?.scrollIntoView({ block: "nearest" });
	});

	const activate = (entry: Entry) => {
		if (entry.kind === "dir") {
			// ツリー mode: dir click で expand toggle (fuzzy mode では dir は出てこない)
			const next = new Set(expanded());
			if (next.has(entry.rel_path)) next.delete(entry.rel_path);
			else next.add(entry.rel_path);
			setExpanded(next);
			return;
		}
		// file: open IPC を投げて optimistic dismiss。 Canvas 表示完了は待たない。
		const address = targetAddress();
		const projectPath = resolveProjectPath(address);
		if (!projectPath) {
			console.warn("[FileExplorer] open: project path 不明 for", address);
			dismiss();
			return;
		}
		sendIpc({
			t: "files:open",
			path: projectPath,
			address,
			rel_path: entry.rel_path,
		});
		if (pinned()) {
			// ピン留め中は picker を閉じず、 連続選択しやすいよう検索 input に focus を戻す。
			setTimeout(() => inputRef?.focus(), 0);
		} else {
			dismiss();
		}
	};

	// ピンを on/off する。 on→off の遷移は user 仕様「外すと close」に従って dismiss も呼ぶ
	// (= ピンボタンが close 兼トグルを兼ねる)。 off→on は単に pin 状態に切り替えるだけ。
	const togglePin = () => {
		const wasPinned = pinned();
		setPinned(!wasPinned);
		if (wasPinned) {
			dismiss();
		} else {
			// pin on にしたら検索 input に focus を戻す (キーボード操作中の流れを切らない)
			setTimeout(() => inputRef?.focus(), 0);
		}
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
			if (item) activate(item.entry);
		}
	};

	return (
		<Show when={visible()}>
			<div class="vp-file-explorer" onKeyDown={onKeyDown}>
				<div class="vp-fe-backdrop" onClick={dismiss} />
				<div class="vp-fe-panel" onClick={(e) => e.stopPropagation()}>
					<div class="vp-fe-header">
						<input
							ref={inputRef}
							class="vp-fe-search"
							type="text"
							placeholder="ファイル名で検索 (空でツリー)..."
							value={query()}
							onInput={(e) => {
								setQuery(e.currentTarget.value);
								setSelectedIndex(0);
							}}
						/>
						<button
							class="vp-fe-pin"
							classList={{ active: pinned() }}
							onClick={togglePin}
							title={
								pinned()
									? "ピン留め解除 (= 閉じる)"
									: "ピン留め (auto close 無効)"
							}
							type="button"
						>
							<CreoIcon
								name={pinned() ? "ph:push-pin-fill" : "ph:push-pin"}
								size={14}
							/>
						</button>
						<button
							class="vp-fe-close"
							onClick={dismiss}
							title="閉じる (Esc)"
							type="button"
						>
							✕
						</button>
					</div>
					<div class="vp-fe-meta">
						<span class="vp-fe-addr" title={targetAddress()}>
							{targetAddress()}
						</span>
						<Show when={truncated()}>
							<span class="vp-fe-warn">20,000+ entries — truncated</span>
						</Show>
					</div>
					<div class="vp-fe-body" ref={bodyRef}>
						<Show when={loading()}>
							<div class="vp-fe-empty">loading…</div>
						</Show>
						<Show when={!loading() && view().length === 0}>
							<div class="vp-fe-empty">
								{query() ? "マッチなし" : "ファイルなし"}
							</div>
						</Show>
						<For each={view()}>
							{(item, idx) => (
								<div
									class="vp-fe-row"
									classList={{
										selected: idx() === selectedIndex(),
										dir: item.entry.kind === "dir",
										file: item.entry.kind === "file",
									}}
									style={`padding-left:${8 + item.depth * 14}px`}
									onClick={() => {
										setSelectedIndex(idx());
										activate(item.entry);
									}}
								>
									<span class="vp-fe-icon">
										{/* 脱 TUI: ▼/▶/· の text glyph を CreoIcon (Iconify ph:) に置換。
                        dir = caret (展開 affordance)、 file = ph:file を tertiary で控えめに。 */}
										{item.entry.kind === "dir" ? (
											<CreoIcon
												name={
													expanded().has(item.entry.rel_path)
														? "ph:caret-down"
														: "ph:caret-right"
												}
												size={10}
											/>
										) : (
											<CreoIcon name="ph:file" size={10} />
										)}
									</span>
									<span class="vp-fe-name">
										{query()
											? item.entry.rel_path
											: (item.entry.rel_path.split("/").pop() ??
												item.entry.rel_path)}
									</span>
								</div>
							)}
						</For>
					</div>
					<div class="vp-fe-footer">↑↓ 移動 · Enter 開く · Esc 閉じる</div>
				</div>
			</div>
		</Show>
	);
}

/** Shell.tsx の SHELL_CSS に concat して inject する CSS。 */
export const FILE_EXPLORER_CSS = `
.vp-file-explorer{position:absolute;inset:0;z-index:10000;display:flex;}
.vp-fe-backdrop{position:absolute;inset:0;background:rgba(0,0,0,.45);}
.vp-fe-panel{position:relative;display:flex;flex-direction:column;width:100%;
  background:var(--color-surface-bg-base);
  border-right:1px solid var(--color-surface-border,#1f2233);}
.vp-fe-header{flex:0 0 auto;display:flex;gap:6px;padding:8px;
  border-bottom:1px solid var(--color-surface-border,#1f2233);}
.vp-fe-search{flex:1;padding:5px 8px;
  border:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-subtle);color:var(--color-text-primary);
  border-radius:var(--radius-sm,6px);font-family:inherit;font-size:var(--sb-text-hint,12px);}
.vp-fe-search:focus{outline:none;border-color:var(--color-brand-primary);}
.vp-fe-pin{display:inline-flex;align-items:center;padding:3px 5px;
  border:none;background:transparent;color:var(--color-text-tertiary);
  cursor:pointer;border-radius:3px;
  transition:background .12s ease,color .12s ease;}
.vp-fe-pin:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-fe-pin.active{color:var(--color-brand-primary);
  background:var(--color-brand-primary-subtle);}
.vp-fe-close{border:none;background:transparent;color:var(--color-text-tertiary);
  font-size:14px;cursor:pointer;padding:0 6px;border-radius:3px;}
.vp-fe-close:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-fe-meta{flex:0 0 auto;display:flex;justify-content:space-between;gap:8px;
  padding:4px 8px;font-size:var(--sb-text-micro,10px);color:var(--color-text-tertiary);
  border-bottom:1px solid var(--color-surface-border,#1f2233);}
.vp-fe-addr{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-fe-warn{color:var(--color-status-warning,#d49b3f);white-space:nowrap;}
/* 脱 TUI (2026-07): body の VPMono 全面指定を撤去して sidebar の sans を継承。
   サイズも lane 行と同じ hint 段に揃え、 picker = 「行本文」 の意味論で統一する。 */
.vp-fe-body{flex:1;overflow-y:auto;padding:4px 0;
  font-size:var(--sb-text-hint,12px);}
.vp-fe-empty{padding:12px;text-align:center;color:var(--color-text-tertiary);}
.vp-fe-row{display:flex;align-items:center;gap:6px;padding:3px 8px;cursor:pointer;
  white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}
.vp-fe-row:hover{background:var(--color-surface-bg-emphasis);}
.vp-fe-row.selected{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);}
.vp-fe-row.dir{color:var(--color-text-secondary);}
.vp-fe-icon{display:inline-flex;width:12px;justify-content:center;align-items:center;
  flex:0 0 auto;color:var(--color-text-tertiary);}
.vp-fe-name{overflow:hidden;text-overflow:ellipsis;}
.vp-fe-footer{flex:0 0 auto;padding:4px 8px;font-size:var(--sb-text-micro,10px);text-align:center;
  color:var(--color-text-tertiary);
  border-top:1px solid var(--color-surface-border,#1f2233);}
`;
