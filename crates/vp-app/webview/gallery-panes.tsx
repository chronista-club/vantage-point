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
	cloneLayout,
	createLayoutEngine,
	createTimeDriver,
	equalize,
	jumpDriver,
	memberIds,
	moveDominance,
	proposeLayout,
	resolve,
	setShare,
	settleRelease,
} from "@chronista-club/creo-ui-layout";
import { PaneStage, useEngineResolved } from "@chronista-club/creo-ui-layout/solid";
import { onCleanup, onMount } from "solid-js";
import { render } from "solid-js/web";
import { type FleetOp, type FleetPayload, mapControl } from "./fleet";
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

// ---------- fleet 配線（LE-19: 机上の 3 台 → gallery の場） ----------
// 読み手: vp-app app.rs `fleet_dispatch_js`（world-device channel の control_event 転送）。
// mapping は fleet.ts（純 calculation）、ここは engine への action だけを持つ。
// ROTO knob = share（VCA — 1 本上げると他が duck する）/ X-Touch fader 1 = t の hand driver /
// LPD8 pad = Scene slot（tap = apply / 長押し = capture）。

/** LPD8 pad の Scene slot（ephemeral — Reload まで。永続化は需要が出たら settle log 側で） */
const sceneSlots = new Map<number, Layout>();
const padPressAt = new Map<number, number>();
/** pad 押下がこの ms 以上 = capture、未満 = apply */
const PAD_HOLD_MS = 400;
/**
 * 保持中の touch センサー個体（fleet.ts の source key）。物理 touch は knob 8 本 + fader が
 * 独立なので bool では足りない — settle を刻むのは「最後の 1 本を手放した」時だけ
 * （team-b review: 単一 bool だと他 knob 保持中の release が中間状態を早期 settle していた）
 */
const fleetTouches = new Set<string>();
let fleetSettleTimer: number | null = null;

/** touch センサーの無い knob（LPD8）向けの settle debounce（OQ-3 の実機初期値: 500ms） */
function scheduleFleetSettle(): void {
	if (fleetSettleTimer != null) clearTimeout(fleetSettleTimer);
	fleetSettleTimer = window.setTimeout(() => {
		fleetSettleTimer = null;
		if (fleetTouches.size === 0) engine.settle(SCOPE, "human");
	}, 500);
}

function applyFleetOp(op: FleetOp): void {
	switch (op.op) {
		case "share": {
			// knob i ↔ structure 順の member i（mute しても同じ knob で戻せる安定対応）
			const id = memberIds(engine.current(SCOPE).structure)[op.paneIndex];
			if (!id) return;
			seizeDrive();
			engine.update(SCOPE, (l) => setShare(l, id, op.share));
			scheduleFleetSettle();
			return;
		}
		case "touch": {
			if (op.pressed) {
				fleetTouches.add(op.source);
				// 触れる = 時間を止める（LE-16 Touch — 駆動中の transition は凍結して観察できる）
				seizeDrive();
				return;
			}
			fleetTouches.delete(op.source);
			// 他の指がまだ触れている間は確定しない — settle は最後の 1 本の手放しが刻む
			if (fleetTouches.size > 0) return;
			const handle = engine.transition(SCOPE);
			if (handle) {
				// 凍結したまま手放した = 近い端点へ spring で着地（§5）
				activeDrive = settleRelease(handle, chooseDriver());
			} else {
				// knob を回していた = 手放しが「形の確定」— touch release が settle を刻む
				engine.settle(SCOPE, "human");
			}
			return;
		}
		case "scrub": {
			const handle = engine.transition(SCOPE);
			if (!handle) return;
			seizeDrive(); // human が t を持つ = time driver から fader を奪う
			handle.scrub(op.t);
			return;
		}
		case "pad": {
			if (op.pressed) {
				padPressAt.set(op.slot, performance.now());
				return;
			}
			const pressedAt = padPressAt.get(op.slot);
			padPressAt.delete(op.slot);
			const held = pressedAt != null && performance.now() - pressedAt >= PAD_HOLD_MS;
			if (held) {
				sceneSlots.set(op.slot, cloneLayout(engine.current(SCOPE)));
			} else {
				const layout = sceneSlots.get(op.slot);
				if (layout) {
					engine.applyScene(SCOPE, {
						id: `pad-${op.slot}`,
						name: `Pad ${op.slot + 1}`,
						layout,
					});
				}
			}
			return;
		}
	}
}

(window as unknown as { vpFleet?: unknown }).vpFleet = {
	dispatch(payload: unknown): void {
		const p = (payload ?? {}) as FleetPayload;
		if (!p.port_name || !p.event) return;
		const op = mapControl(p.port_name, p.event);
		if (op) applyFleetOp(op);
	},
};

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
