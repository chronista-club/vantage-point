/**
 * shell layout — L sidebar | main | R sidebar の**形**（幅 / full-slim / R 開閉）。
 *
 * ## この module が SSOT である理由
 *
 * shell は 1 document の flex 行（`#app-shell`）で、`#sidebar-root` と `#right-sidebar` の
 * 幅がその形を決める。`sidebarForm`（full/slim）は sidebar bundle の `form.ts` が、R の開閉は
 * `right-sidebar.ts` が持っているが、**「window をどう開いていたか」を 1 つの箱で永続化する**
 * ため、形の集約はここに置く。両者からは CustomEvent で「変わった」とだけ伝わる
 * （`vp:board-view` / `vp:board-presence` と同じ bus の流儀）。
 *
 * ## 層の分離
 *
 * - **data**: `state`（幅 2 つ + form + R 開閉）。この module が持つ
 * - **calculations**: `clampWidth` / `nextWidth` — 純関数（vitest 対象）
 * - **actions**: `installShellLayout`（境界の取っ手 + drag 配線 + IPC 送信 + 復元の受け口）
 *
 * ## ⚠️ 保存は「確定時」だけ
 *
 * pointermove ごとに IPC を撃つと、window resize と同じ頻度で session.json を書くことになる。
 * drag 中は DOM だけ動かし、**pointerup で 1 回**送る。
 *
 * ## ⚠️ slim の幅は保存しない
 *
 * `#sidebar-root.slim` は CSS 固定の 44px。保存するのは **full の幅**だけで、slim 中に
 * drag はできない（取っ手を出さない）。混ぜると「slim で終了 → 次回 full が 44px」になる。
 */

/** L sidebar の幅の許容範囲（Rust `SIDEBAR_MIN/MAX_WIDTH` と対の値）。 */
export const SIDEBAR_MIN = 180;
export const SIDEBAR_MAX = 640;
/** R sidebar の幅の許容範囲（Rust `RIGHT_SIDEBAR_MIN/MAX_WIDTH` と対の値）。 */
export const RIGHT_MIN = 240;
export const RIGHT_MAX = 900;

/** 既定値。CSS の初期値（`#sidebar-root{width:280px}` / `#right-sidebar{width:420px}`）と揃える。 */
const DEFAULT_SIDEBAR = 280;
const DEFAULT_RIGHT = 420;

export interface ShellLayoutState {
	sidebarWidth: number;
	rightSidebarWidth: number;
	/** `"full" | "slim"`。slim の幅は CSS 固定なので `sidebarWidth` には触らない。 */
	sidebarForm: string;
	rightSidebarOpen: boolean;
}

const state: ShellLayoutState = {
	sidebarWidth: DEFAULT_SIDEBAR,
	rightSidebarWidth: DEFAULT_RIGHT,
	sidebarForm: "full",
	rightSidebarOpen: false,
};

// ============================================================================
// calculations（純関数 — vitest 対象）
// ============================================================================

/** 範囲に丸める。⚠️ Rust 側の clamp と**同じ端**を持つ（片方だけ変えるとずれる）。 */
export function clampWidth(px: number, min: number, max: number): number {
	if (!Number.isFinite(px)) return min;
	return Math.round(Math.min(max, Math.max(min, px)));
}

/**
 * drag 中の新しい幅。`side` で符号が反転する — L は「境界の x がそのまま幅」、
 * R は「窓の右端から境界までの距離」。
 */
export function nextWidth(
	side: "left" | "right",
	pointerX: number,
	shellRect: { left: number; right: number },
): number {
	return side === "left"
		? clampWidth(pointerX - shellRect.left, SIDEBAR_MIN, SIDEBAR_MAX)
		: clampWidth(shellRect.right - pointerX, RIGHT_MIN, RIGHT_MAX);
}

// ============================================================================
// actions
// ============================================================================

function el<T extends HTMLElement>(sel: string): T | null {
	return document.querySelector<T>(sel);
}

/** 幅を DOM に当てる。form が slim のときは L の inline 幅を外す（CSS の 44px に譲る）。 */
function applyWidths(): void {
	const left = el("#sidebar-root");
	if (left) {
		if (state.sidebarForm === "slim") left.style.removeProperty("width");
		else left.style.width = `${state.sidebarWidth}px`;
	}
	const right = el("#right-sidebar");
	if (right) right.style.width = `${state.rightSidebarWidth}px`;
	// 取っ手は slim / R 閉のときは出さない（掴めないものに取っ手を見せない）。
	const lh = el("#shell-resizer-left");
	if (lh) lh.style.display = state.sidebarForm === "slim" ? "none" : "";
	const rh = el("#shell-resizer-right");
	if (rh) rh.style.display = state.rightSidebarOpen ? "" : "none";
}

function sendIpc(payload: Record<string, unknown>): void {
	const ipc = (globalThis as unknown as { ipc?: { postMessage(m: string): void } }).ipc;
	if (!ipc || typeof ipc.postMessage !== "function") return;
	try {
		ipc.postMessage(JSON.stringify(payload));
	} catch (e) {
		console.warn("[shell-layout] ipc failed", e);
	}
}

/** 形が確定した（drag 終了 / form 切替 / R 開閉）。**ここだけ**が保存を起こす。 */
function persist(): void {
	sendIpc({
		t: "shell:layout",
		sidebar_width: state.sidebarWidth,
		right_sidebar_width: state.rightSidebarWidth,
		sidebar_form: state.sidebarForm,
		right_sidebar_open: state.rightSidebarOpen,
	});
}

/** Rust からの復元 push を当てる（`shell:layout` event）。保存が無ければ呼ばれない。 */
export function applyShellLayout(l: {
	sidebar_width: number;
	right_sidebar_width: number;
	sidebar_form: string;
	right_sidebar_open: boolean;
}): void {
	state.sidebarWidth = clampWidth(l.sidebar_width, SIDEBAR_MIN, SIDEBAR_MAX);
	state.rightSidebarWidth = clampWidth(l.right_sidebar_width, RIGHT_MIN, RIGHT_MAX);
	state.sidebarForm = l.sidebar_form === "slim" ? "slim" : "full";
	state.rightSidebarOpen = !!l.right_sidebar_open;
	applyWidths();
	// 形と開閉は持ち主（form.ts / right-sidebar.ts）に反映させる。復元は**こちらから**
	// 伝える向き（起動時は向こうが既定値で立ち上がっているので、黙っていると食い違う）。
	publishRestore();
}

/**
 * 復元を持ち主へ配る。**撃つだけでなく保持する**。
 *
 * ⚠️ `form.ts` は **sidebar bundle**（`sidebar.bundle.js`）に居て、main bundle より後に
 * 評価される。復元 push は main bundle の `ready` を契機に来るので、**受け口が生える前に
 * 撃つ窓**が実在する（2026-08-06 実測: 復元 09:24:00.321 に対し sidebar boot 09:24:00.264 と
 * 僅差で、実機では class が当たらなかった）。CustomEvent は保持されないので、1 回撃って
 * 終わりだと二度と来ない — これは VP が既に 3 回踏んだ型（devices snapshot / terminal
 * replay / retained board、`app.rs` の各コメント）で、対策も同じ **保留箱**。
 *
 * 遅れて来た受け口は install 時に [`retainedShellRestore`] を引く。
 */
function publishRestore(): void {
	const detail = { form: state.sidebarForm, rightOpen: state.rightSidebarOpen };
	(globalThis as unknown as { __vpShellRestore?: unknown }).__vpShellRestore =
		detail;
	document.dispatchEvent(new CustomEvent("vp:shell-restore", { detail }));
}

/** 保留箱の読み口。復元がまだ来ていなければ `null`。 */
export function retainedShellRestore(): {
	form: string;
	rightOpen: boolean;
} | null {
	const v = (
		globalThis as unknown as {
			__vpShellRestore?: { form: string; rightOpen: boolean };
		}
	).__vpShellRestore;
	return v ?? null;
}

/** 現在の形（テスト用の読み口）。 */
export function shellLayoutState(): Readonly<ShellLayoutState> {
	return state;
}

/**
 * 境界の取っ手を配線する。
 *
 * ⚠️ **drag 中は body に `shell-resizing` を付ける** — iframe（board の html item / preview）が
 * pointer を奪うと drag が途中で死ぬので、CSS 側で `pointer-events:none` を当てて逃がす。
 * 透明 iframe が wheel を吸った件（#899）と同じ性質の罠。
 */
function wireHandle(handle: HTMLElement, side: "left" | "right"): void {
	handle.addEventListener("pointerdown", (e) => {
		const shell = el("#app-shell");
		if (!shell) return;
		e.preventDefault();
		handle.setPointerCapture(e.pointerId);
		document.body.classList.add("shell-resizing");
		handle.classList.add("dragging");

		const onMove = (ev: PointerEvent) => {
			const rect = shell.getBoundingClientRect();
			const px = nextWidth(side, ev.clientX, rect);
			if (side === "left") state.sidebarWidth = px;
			else state.rightSidebarWidth = px;
			applyWidths();
		};
		const onUp = () => {
			handle.removeEventListener("pointermove", onMove);
			handle.removeEventListener("pointerup", onUp);
			handle.removeEventListener("pointercancel", onUp);
			document.body.classList.remove("shell-resizing");
			handle.classList.remove("dragging");
			// 確定 = ここで 1 回だけ保存する（move ごとには撃たない）。
			persist();
		};
		handle.addEventListener("pointermove", onMove);
		handle.addEventListener("pointerup", onUp);
		handle.addEventListener("pointercancel", onUp);
	});
}

/** shell layout を配線する（entry.tsx の boot から 1 回）。 */
export function installShellLayout(): void {
	const lh = el<HTMLElement>("#shell-resizer-left");
	const rh = el<HTMLElement>("#shell-resizer-right");
	if (lh) wireHandle(lh, "left");
	if (rh) wireHandle(rh, "right");

	// 形の変化は持ち主から通知される（sidebar bundle の form.ts / right-sidebar.ts）。
	// ⚠️ 受けたら**保存も撃つ** — 形と開閉は drag と同じ「shell の形」の一部なので、
	// 幅だけ永続化して形が毎回既定に戻ると、復元が半分になる。
	document.addEventListener("vp:sidebar-form", (e) => {
		const d = (e as CustomEvent<{ form?: string }>).detail;
		if (!d?.form) return;
		state.sidebarForm = d.form === "slim" ? "slim" : "full";
		applyWidths();
		persist();
	});
	document.addEventListener("vp:right-sidebar-state", (e) => {
		const d = (e as CustomEvent<{ open?: boolean }>).detail;
		if (typeof d?.open !== "boolean") return;
		state.rightSidebarOpen = d.open;
		applyWidths();
		persist();
	});

	applyWidths();
}
