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
	type DominanceDirection,
	type Layout,
	type PaneRef,
	createLayoutEngine,
	equalize,
	formatNotation,
	isMember,
	moveDominance,
	setShare,
} from "@chronista-club/creo-ui-layout";
import { PaneStage, useEngineResolved } from "@chronista-club/creo-ui-layout/solid";
import { onCleanup, onMount } from "solid-js";
import { render } from "solid-js/web";
import { GALLERY_CSS, STORIES, isGalleryHash, storyPaneHtml, toggleGalleryHash } from "./gallery";

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
		const l = engine.current(SCOPE);
		const floats = Object.keys(l.attention).filter(
			(id) => !isMember(l.structure, id) && (l.attention[id] ?? 0) > 0,
		);
		return formatNotation(l.structure, floats);
	};

	const gesture = (fn: (l: Layout) => Layout) => {
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
					onFloatMove={(id, pos) => engine.moveFloat(SCOPE, id, pos)}
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
