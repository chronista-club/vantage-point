/**
 * app pane（Echoes / Preview / GE / Bastet / empty）の **DOM 切替**と slot rect の push。
 *
 * doc 53 §6.5 の World A 畳み込みで `main_area.rs` の inline `<script>` から移設した。
 *
 * ## この module が持つもの / 持たないもの
 *
 * 持つのは「kind → どの DOM pane を active にするか」と「その rect を Rust に push する」まで。
 * **`window.setActivePane` は持たない** — 配置（app-panes）・EchoesHeader・board を含む
 * 完全な切替は `entry.tsx` が 1 本の関数として組み立て、window に載せる。
 *
 * 旧構成では ここが `window.setActivePane` を定義し、`entry.tsx` が **wrap** して layout を
 * 足す 2 段だった。あれは「DOM 切替は World A、layout は World B」という**世界の分断が理由**の
 * 形で、World A が消えた（#921）時点で意味を失っていた。2 段は wrap 側の install を
 * DOMContentLoaded まで遅らせる必要（= 被 wrap 側が後から定義される順序依存）も生んでいた。
 */

/**
 * Rust `push_active_view` が運ぶ payload。
 *
 * `kind` / `pane_id` / `preview_url` / `chat` は本 module（DOM 切替）が読み、残りは
 * `entry.tsx` 側（EchoesHeader の lane 文脈）が読む。1 つの payload を 2 つの関心が
 * 分けて読む形なので、型はここに 1 つ置いて両方から参照する。
 */
export interface ActivePaneInfo {
	kind?: string | null;
	pane_id?: string | null;
	preview_url?: string | null;
	/** doc 33: chat lane (Act II) フラグ。xterm を持たない lane の placeholder 抑止に使う。 */
	chat?: boolean;
	// ↓ Echoes 共通ヘッダ用 lane 文脈（setActivePane 相乗り、creo memo `vp-pane-common-header`）
	lane_name?: string | null;
	cwd?: string | null;
	branch?: string | null;
	/** active engine の session id（Act I の session chip 供給路。Act II は event が上書き）。 */
	session_id?: string | null;
	/** root session の stand（= slot に載る engine 種別、chip prefix 導出用: "echoes" / "codex" /
	 *  "grok" 等）。doc 39 P4-C: Rust push_active_view が engine_stand（root の engine）優先で解決
	 *  済み（cross-engine root でも chip prefix が slot の engine を映す）。無ければ lane 固定 stand。 */
	stand?: string | null;
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
// ⚠️ entry.tsx にも同じ kind を引く表があるが**別の写像**で、統合してはいけない —
// あちらは Frame Engine の `data-frame-id`（"echoes" 等 = 配置の座標系）、こちらは DOM
// element id（"pane-terminal" 等 = 可視性の gate）。同じ kind から**別の軸**を引いている。
const KIND_TO_PANE: Record<string, string> = {
	terminal: "pane-terminal",
	preview: "pane-preview",
	gold_experience: "pane-gold-experience",
	bastet: "pane-bastet",
	empty: "pane-empty",
};

/** 現在 active な pane の info (slot:rect 送出時の pane_id 補完用)。 */
let activePaneInfo: ActivePaneInfo | null = null;

// ========= VP-100 γ-light: slot rect を Rust に push =========
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

/**
 * kind に対応する DOM pane を active にし、preview iframe / showLane / slot rect を追随させる。
 *
 * `entry.tsx` が組み立てる `window.setActivePane` の **前半**（DOM の可視性）。後半（配置・
 * EchoesHeader・board）は entry.tsx 側が続けて行う。
 */
export function applyPaneSwitch(info: ActivePaneInfo | null): void {
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
		//
		// window 経由なのは、この API が Rust からも名前で呼ばれる契約だから（term.ts 参照）。
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

/**
 * host（= main area の root）の resize を拾って slot rect を Rust に push する observer を張る。
 * 中の pane も同サイズでリサイズされるので、観測点は host 1 つで足りる。
 */
export function installSlotRect(): void {
	// PH#4: rAF debounce — window resize 中の高頻度発火で event queue が詰まらないよう、
	// 1 frame に最大 1 回 sendSlotRect を呼ぶように制限。
	let rafScheduled = false;
	const schedule = (): void => {
		if (rafScheduled) return;
		rafScheduled = true;
		requestAnimationFrame(() => {
			rafScheduled = false;
			sendSlotRect();
		});
	};
	const host = document.getElementById("host");
	if (host) new ResizeObserver(schedule).observe(host);
}

/**
 * bundle 到達 stage の runtime 検査（VP-140 diagnostic）。
 * DevTools console から `window.vpBundleProbe()` で呼ぶ。
 */
export function installBundleProbe(): void {
	(window as unknown as { vpBundleProbe: () => unknown }).vpBundleProbe = () => {
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
