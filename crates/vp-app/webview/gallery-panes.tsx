/**
 * Gallery の pane 化 — creo-ui-layout の VP 最初のコンテンツ（LE-P2 PR1、doc 49 §6 step 3）。
 *
 * gallery.ts（純 data / calculation）の story を PaneStage の pane として並べ、
 * attention 連続場を WKWebView 実機で dogfood する台。本 module は action 層:
 * Solid の mount/dispose・keyboard・engine への gesture 配線だけを持つ。
 *
 * 操作（PR1 の最小セット — AI の口 = MCP layout bridge は PR2）:
 *   click     = その pane を主役に引き上げ（setShare 0.55）
 *   ← → ↑ ↓  = 主役（dominance）の 2D 移動
 *   e         = 等分 snap
 * 場・settle log は module 単位で生き、gallery を閉じても配置が残る（Reload まで）。
 *
 * 旧 syncGalleryDom / installGallery（doc 48 Phase 3、innerHTML 版）の後継。
 * Editor Mode との同居規約（#editor-root z-index）は GALLERY_CSS 側で維持。
 */

import {
	type ApplyPolicy,
	type DominanceDirection,
	type DriverRun,
	type Layout,
	type PaneRef,
	type ResolvedMap,
	type TransitionDriver,
	createLayoutEngine,
	createTimeDriver,
	equalize,
	jumpDriver,
	moveDominance,
	proposeLayout,
	resolve,
	setShare,
} from "@chronista-club/creo-ui-layout";
import { PaneStage, useEngineResolved } from "@chronista-club/creo-ui-layout/solid";
import { onCleanup, onMount } from "solid-js";
import { render } from "solid-js/web";
import {
	GALLERY_CSS,
	type LayoutSpec,
	STORIES,
	applyLayoutSpec,
	isGalleryHash,
	layoutNotation,
	layoutSnapshot,
	storyPaneHtml,
	takeRecent,
	toggleGalleryHash,
} from "./gallery";

const SCOPE = "gallery";

/** click で主役に引き上げる時の share（独占でなく「大きめ」— 他の story も見えたまま） */
const RAISE_SHARE = 0.55;

// engine は module 単位で 1 個 — gallery を閉じても場と settle log が残る（dogfood 都合）
const engine = createLayoutEngine();
engine.update(SCOPE, () => initialLayout());
engine.settle(SCOPE, "human");

/** 初期配置: 全 story を横一列・等分（純 calculation） */
function initialLayout(): Layout {
	const attention: Record<string, number> = {};
	for (const s of STORIES) attention[s.id] = 1;
	return { structure: { columns: STORIES.map((s) => ({ panes: [s.id] })) }, attention };
}

// PaneStage の契約: PaneRef は referentially stable に保つ（host 再生成 = LE-10 違反を防ぐ）
const PANE_REFS: readonly PaneRef[] = STORIES.map((s) => ({ id: s.id, label: s.title }));
const PANE_HTML = new Map(STORIES.map((s) => [s.id, storyPaneHtml(s)]));

const ARROW_TO_DIR: Record<string, DominanceDirection> = {
	ArrowLeft: "left",
	ArrowRight: "right",
	ArrowUp: "up",
	ArrowDown: "down",
};

/** Editor Mode パネル等の入力中は gallery hotkey を横取りしない */
function isEditableTarget(t: EventTarget | null): boolean {
	if (!(t instanceof HTMLElement)) return false;
	return (
		t.isContentEditable ||
		t.tagName === "INPUT" ||
		t.tagName === "TEXTAREA" ||
		t.tagName === "SELECT"
	);
}

function GalleryPanes() {
	const resolved = useEngineResolved(engine, SCOPE);

	// 現在の記法（構造 + float）— 場の状態が一目で読める dogfood 計器
	const notation = () => {
		resolved(); // 購読
		return layoutNotation(engine.current(SCOPE));
	};

	const gesture = (fn: (l: Layout) => Layout) => {
		seizeDrive(); // Touch: 触れた瞬間に奪取（engine.update の seize + driver timer 停止）
		engine.update(SCOPE, fn);
		engine.settle(SCOPE, "human");
	};

	onMount(() => {
		const onKeydown = (e: KeyboardEvent) => {
			if (e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return;
			if (isEditableTarget(e.target)) return;
			const dir = ARROW_TO_DIR[e.key];
			if (dir) {
				e.preventDefault();
				gesture((l) => moveDominance(l, dir));
			} else if (e.key === "e") {
				e.preventDefault();
				gesture((l) => equalize(l));
			}
		};
		window.addEventListener("keydown", onKeydown);
		onCleanup(() => window.removeEventListener("keydown", onKeydown));
	});

	return (
		<>
			<header class="g-header">
				<h1>Component Gallery</h1>
				<p class="g-note">
					Ctrl+Shift+G で戻る / Ctrl+Shift+E で Editor Mode / click = 引き上げ・←→↑↓ =
					主役移動・e = 等分（layout = creo-ui-layout の場）
				</p>
				<p class="g-note g-notation">{notation()}</p>
			</header>
			<div class="gp-stage">
				<PaneStage
					resolved={resolved()}
					panes={PANE_REFS}
					onFloatMove={(id, pos) => {
						seizeDrive(); // float drag も直接操作 = 奪取
						engine.moveFloat(SCOPE, id, pos);
					}}
					renderPane={(pane) => (
						// biome-ignore lint/security/noDangerouslySetInnerHtml: story は compile-time 定数（gallery.ts）のみ
						<div
							class="gp-pane"
							innerHTML={PANE_HTML.get(pane.id) ?? ""}
							onClick={() => gesture((l) => setShare(l, pane.id, RAISE_SHARE))}
						/>
					)}
				/>
			</div>
		</>
	);
}

// ---------- MCP layout bridge（LE-P2 PR2 = LE-15 の webview 側、P3 で policy 経由に） ----------
// 読み手: vp-app app.rs `editor_bridge_js` の layout_* arm（`window.vpLayoutHost.mcp`）。
// gallery scope の apply policy = read（LE-16）: set は proposeLayout が受け、time driver
// （spring）が t を運び切った所で commit(author="ai") が settle 監査を刻む — AI の変更が
// 「滑らかに現れる」のを mako が画面で見る HITL ループ（#872 の editor_set と同型）。
// 駆動中に human が触れば seize されて commit は起きない（Touch — 注視の主権は user）。
// ⚠️ work lane の scope を追加する時は既定を "write"（hitl gate）にすること（P4、Moody 申し送り）。

const APPLY_POLICY: ApplyPolicy = "read";

/** reduced-motion は「jump driver の選択」に落ちる（LE-7）。curve は package 内蔵の臨界減衰 */
function chooseDriver(): TransitionDriver {
	const reduced =
		typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
	return reduced ? jumpDriver : createTimeDriver();
}

// AI 駆動の遷移は同時に 1 本 — 新しい set / human の直接操作が来たら前の timer は畳む
let activeDrive: DriverRun | null = null;
function seizeDrive(): void {
	activeDrive?.cancel();
	activeDrive = null;
}

function sharesFrom(resolvedMap: ResolvedMap): Record<string, number> {
	const out: Record<string, number> = {};
	for (const [id, pane] of Object.entries(resolvedMap)) {
		out[id] = pane.attention;
	}
	return out;
}

const layoutMcp = {
	get(): unknown {
		// 遷移中は commit 前の layout が返る（≈0.35s の eventual consistency — 監査は settle log）
		return layoutSnapshot(SCOPE, engine.current(SCOPE), sharesFrom(engine.resolved(SCOPE)));
	},
	set(spec: unknown): unknown {
		try {
			const next = applyLayoutSpec(engine.current(SCOPE), (spec ?? {}) as LayoutSpec);
			seizeDrive();
			const result = proposeLayout(engine, SCOPE, next, {
				policy: APPLY_POLICY,
				driver: chooseDriver(),
			});
			if (!result.accepted) {
				return { error: "apply policy = off — 提案は受けられない" };
			}
			activeDrive = result.drive ?? null;
			// 応答は target の姿（spring の完了を MCP 往復で待たせない）。human が途中で
			// 奪取すれば commit されない — その時の実勢は layout_get / history で読める
			return layoutSnapshot(SCOPE, next, sharesFrom(resolve(next)));
		} catch (e) {
			return { error: e instanceof Error ? e.message : String(e) };
		}
	},
	history(opts?: { limit?: number } | null): unknown {
		const entries = takeRecent(engine.history(SCOPE), opts?.limit ?? 10);
		return {
			entries: entries.map((e) => ({
				author: e.author,
				at: e.at,
				notation: layoutNotation(e.layout),
			})),
		};
	},
};
(window as unknown as { vpLayoutHost?: unknown }).vpLayoutHost = { mcp: layoutMcp };

// ---------- mount / unmount（旧 syncGalleryDom の後継） ----------

let disposeGallery: (() => void) | null = null;

/** hash に合わせて gallery root の生成/破棄と `#app-shell` の表示を同期する */
export function syncGalleryDom(): void {
	const active = isGalleryHash(location.hash);
	const existing = document.getElementById("gallery-root");
	const appShell = document.getElementById("app-shell");
	if (active && !existing) {
		const style = document.createElement("style");
		style.id = "gallery-style";
		style.textContent = GALLERY_CSS;
		document.head.appendChild(style);
		const root = document.createElement("div");
		root.id = "gallery-root";
		document.body.appendChild(root);
		disposeGallery = render(() => <GalleryPanes />, root);
		if (appShell) appShell.style.display = "none";
	} else if (!active && existing) {
		disposeGallery?.();
		disposeGallery = null;
		existing.remove();
		document.getElementById("gallery-style")?.remove();
		if (appShell) appShell.style.display = "";
	}
}

/** boot 時に 1 回呼ぶ: hashchange 追従 + Ctrl+Shift+G toggle + 初期同期 */
export function installGallery(): void {
	window.addEventListener("hashchange", syncGalleryDom);
	// capture phase: xterm 等の下位 listener に stopPropagation されても拾えるようにする
	// (keybindings.ts の Scene hotkey と同じ防御。現行 xterm は Ctrl+Shift+文字 を
	// cancel しないが、将来の挙動変更に耐える側に倒す)
	window.addEventListener(
		"keydown",
		(e) => {
			if (e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey && e.code === "KeyG") {
				e.preventDefault();
				location.hash = toggleGalleryHash(location.hash);
			}
		},
		true,
	);
	syncGalleryDom();
}
