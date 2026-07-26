/**
 * app pane（Echoes / Preview / GE / Bastet / empty）の表示切替と slot rect の push — 旧 World A の後半。
 *
 * doc 53 §6.5 の World A 畳み込みで `main_area.rs` の inline `<script>` から移設した
 * （挙動は不変。移設の経緯は term.ts の doc comment）。
 *
 * ## ⚠️ 現状 `window.setActivePane` は 2 段になっている
 *
 * ここが定義した `window.setActivePane` を、`entry.tsx` の `installSetActivePaneBridge` が
 * **wrap** して layout 側（app-panes / EchoesHeader / board）を足す。この 2 段構えは
 * 「DOM 切替は World A、layout は World B」という**世界の分断が理由**で生まれた形で、両方が
 * TS になった今は意味が無い。次段（wrap 解消）で 1 本に畳む — この PR は移設に絞り、挙動を
 * 変えないことで回帰の原因を移設に一意化する。
 */

/** Rust `push_active_view` が運ぶ payload（wrap 側 entry.tsx と共有する形）。 */
export interface ActivePaneInfo {
	kind?: string | null;
	pane_id?: string | null;
	preview_url?: string | null;
	/** doc 33: chat lane (Act II) フラグ。xterm を持たない lane の placeholder 抑止に使う。 */
	chat?: boolean;
}

const ipc = () =>
	(window as unknown as { ipc?: { postMessage(m: string): void } }).ipc;

function post(payload: unknown): void {
	try {
		ipc()?.postMessage(JSON.stringify(payload));
	} catch (_) {}
}

// ========= Architecture v4: Lane / Stand 切替 API =========
// Rust → JS で active Lane / Stand を切替。kind が null の場合は empty 状態を表示。
// Phase 5-A: Project-scope Stand (PP/GE/HP) を click 可能 pane として追加。
// VP-142 cleanup: legacy "canvas" kind 削除 (pane-canvas placeholder 廃止に伴い)。
// doc 52 §10 wave 0: paisley_park は app pane を退役（board pane = lane tiling へ）。
//
// ⚠️ entry.tsx にも同名の `KIND_TO_PANE` があるが**別の写像** — あちらは Frame Engine の
// `data-frame-id`（"echoes" 等）、こちらは DOM element id（"pane-terminal" 等）。wrap 解消の
// 段で 1 つの表に統合する。
const KIND_TO_PANE: Record<string, string> = {
	terminal: "pane-terminal",
	preview: "pane-preview",
	gold_experience: "pane-gold-experience",
	bastet: "pane-bastet",
	empty: "pane-empty",
};

export function installActivePane(): void {
	// 現在 active な pane の info (slot:rect 送出時の pane_id 補完用)
	let activePaneInfo: ActivePaneInfo | null = null;

	// ========= VP-100 γ-light: slot rect を Rust に push =========
	// ResizeObserver が active pane container の rect 変化を捕捉。
	function sendSlotRect(): void {
		const target = document.querySelector(".pane.active");
		if (!target) return;
		const r = target.getBoundingClientRect();
		post({
			t: "slot:rect",
			pane_id: activePaneInfo ? activePaneInfo.pane_id || null : null,
			kind: target.getAttribute("data-kind") || "empty",
			rect: { x: r.left, y: r.top, w: r.width, h: r.height },
		});
	}

	function setActiveImpl(info: ActivePaneInfo | null): void {
		activePaneInfo = info || null;
		const kind = info?.kind ? info.kind : "empty";
		const targetId = KIND_TO_PANE[kind] || "pane-empty";
		for (const el of document.querySelectorAll(".pane")) {
			const isActive = el.id === targetId;
			el.classList.toggle("active", isActive);
			// 動的に data-pane-id を設定 (γ-light: native overlay が pane_id で照合する想定)。
			// 注: Frame Engine の static `data-frame-id` (= "echoes" / "pp" 等の Scene lookup key) とは
			// 別 attribute。 同名にすると Lane click でこの動的書き換えが Frame Engine の attribute を
			// hijack して Scene lookup undefined → HIDDEN_TRANSFORM で pane が見えなくなる (VP-141 fix)。
			if (isActive && info?.pane_id) {
				el.setAttribute("data-pane-id", info.pane_id);
			} else if (isActive) {
				el.removeAttribute("data-pane-id");
			}
		}
		if (kind === "preview") {
			const frame = document.getElementById("preview-frame");
			const url = info?.preview_url || "about:blank";
			if (frame && frame.getAttribute("src") !== url) {
				frame.setAttribute("src", url);
			}
		}
		if (kind === "terminal") {
			// per-(lane, session) instance を切替 (= showLane(address))。 pane_id は Lane address。
			// showLane が空なら lane-empty placeholder を出す。 chat (Act II) lane は xterm を
			// 持たない (ChatView が内容) ので、 その旨を渡して placeholder 抑止させる。
			try {
				(
					window as unknown as {
						showLane?: (a: string | null, isChat: boolean) => void;
					}
				).showLane?.(info?.pane_id ?? null, !!info?.chat);
			} catch (_) {}
		}
		// active 切替直後に slot rect を一発送る (ResizeObserver 起動前 fail-safe)
		sendSlotRect();
	}

	// DOM 未 ready の前に呼ばれた場合は buffer
	let pendingPane: ActivePaneInfo | null = null;
	let domReady = false;
	(
		window as unknown as {
			setActivePane: (info: ActivePaneInfo | null) => void;
		}
	).setActivePane = (info) => {
		if (!domReady) {
			pendingPane = info;
			return;
		}
		setActiveImpl(info);
	};

	// ResizeObserver は host (= main area の root) に張る。中の pane も同サイズでリサイズされる。
	// PH#4: rAF debounce — window resize 中の高頻度発火で event queue が詰まらないよう、
	// 1 frame に最大 1 回 sendSlotRect を呼ぶように制限。
	let rafScheduled = false;
	function scheduleSendSlotRect(): void {
		if (rafScheduled) return;
		rafScheduled = true;
		requestAnimationFrame(() => {
			rafScheduled = false;
			sendSlotRect();
		});
	}
	const host = document.getElementById("host");
	if (host) {
		new ResizeObserver(() => scheduleSendSlotRect()).observe(host);
	}

	// DOM ready 後に pending pane を flush
	// VP-142 (PR-ε-3): flush は `window.setActivePane(pendingPane)` 経由で行う (= bridge を通す)。
	// setActiveImpl 直叩きだと entry.tsx で wrap した setActivePane bridge を bypass し、
	// applyScene（配置）や EchoesHeader の lane 文脈が fire しないため、auto-select Lane の
	// 表示が永続的に未接続のままになる回帰を起こす。domReady=true になっているので
	// window.setActivePane 内の buffering 分岐は再 hit しない (= 無限再帰なし)。
	window.addEventListener("DOMContentLoaded", () => {
		domReady = true;
		if (pendingPane !== null) {
			const flush = pendingPane;
			pendingPane = null;
			(
				window as unknown as {
					setActivePane: (info: ActivePaneInfo | null) => void;
				}
			).setActivePane(flush);
		}
	});
}

/**
 * bundle 到達 stage の runtime 検査（VP-140 diagnostic）。
 * DevTools console から `window.vpBundleProbe()` で呼ぶ。
 */
export function installBundleProbe(): void {
	(window as unknown as { vpBundleProbe: () => unknown }).vpBundleProbe =
		() => {
			const w = window as unknown as Record<string, unknown>;
			const paneTerminal = document.querySelector("#pane-terminal");
			return {
				bundleStatus: w.vpBundleStatus,
				vpAppLayoutDefined: typeof w.vpAppLayout !== "undefined",
				setActivePaneDefined: typeof w.setActivePane === "function",
				ensureLaneDefined: typeof w.ensureLane === "function",
				showLaneDefined: typeof w.showLane === "function",
				laneInstancesSize:
					w.__vpLanes instanceof Map ? w.__vpLanes.size : "no __vpLanes",
				paneCount: document.querySelectorAll("[data-frame-id]").length,
				paneTerminalOpacity: paneTerminal
					? getComputedStyle(paneTerminal).opacity
					: null,
			};
		};
	console.info(
		"[vp-bundle] vpBundleProbe registered (call window.vpBundleProbe() in console)",
	);
}
