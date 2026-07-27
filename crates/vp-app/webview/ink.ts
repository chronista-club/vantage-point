/**
 * ink（対話面、doc 52 §3）— board item の上に描いて、明示送信で「面の snapshot + 一行」を
 * focused session の会話へ渡す。
 *
 * ## 設計（doc 52 §3、2026-07-24 確定）
 *
 * - 読み方 = **合成画像 + item id**（semantic anchor は廃案）。annotation は構造データにせず
 *   pixel のまま届き、意味は受け手（claude / codex / grok = vision）が画像から汲む
 * - 撮影 = **WKWebView.takeSnapshot(rect = #ink-stage)**（Rust 側 ink_snapshot.rs）。webview は
 *   rect を送り、Rust が PNG を書いて push envelope `ink:snapshot`（失敗時
 *   `ink:snapshot_error`）で返す。`window.vpInk` は DevTools 検分用に残っているだけで、
 *   **Rust は名前で呼ばない**（doc 53 §6.5.1.3）
 * - 送信後 = **残らない**（送信 = 手放す。update との anchor drift を構造的に回避）
 * - 送信先 = focused session。mode で経路分岐（chat = `conversation:submit` / tui = PTY `term:write`）
 * - **server 0 行**: 既存 IPC（conversation:submit / term:write）を撃つだけ。永続も購読もしない
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: module-local `annotations`（送信/クリアで消える ephemeral 状態）
 * - **calculations**: `inkSendLine` / `inkRoute` / `resolveTooSmall` — 純関数（vitest で固定）
 * - **actions**: `installInk`（overlay の pointer 配線 + palette + snapshot 往復 + IPC 送信）
 */

import type { InkPushHandlers } from "./dispatch";

/** 道具。select = 描かない（下の item を触れる）。 */
export type InkTool = "select" | "line" | "arrow" | "text" | "free";

/** 1 つの annotation（overlay 上の SVG 要素 + 幾何）。送信/クリアで破棄される。 */
interface InkAnnotation {
	kind: "line" | "arrow" | "text" | "freehand";
	els: SVGElement[];
}

/** ink.ts が外から必要とする lane 文脈（entry.tsx が closure で注入）。 */
export interface InkDeps {
	/** 表示中 board item の id（board-handler の cursor）。null = 空 board。 */
	getItemId: () => string | null;
	/** 現 active lane の address（`<repo>/root` 等）。null = lane 未選択。 */
	getLaneAddress: () => string | null;
	/** lane の focused session key（console.focusedOf）。 */
	getFocusedSession: (laneAddr: string) => number;
	/** その session の mode（見え方）。doc 50 §4.6 A6 で lane 単位 console_mode から移行 —
	 *  送り先は「focused session がどちらの面で生きているか」で決まる。 */
	getSessionMode: (laneAddr: string, session: number) => "tui" | "gui";
}

/** IPC 送信の宛先（純関数 inkRoute の出力。actions が実際の postMessage に写す）。 */
export type InkRoute =
	| { kind: "gui"; lane: string; session: number; prompt: string }
	// tui = PTY 直書き。data は「行 + CR」の生文字列（actions 側で UTF-8 → base64 化）。
	// session は宛先 slot（doc 50 §4.6 A6 — xterm が (lane, session) になったので必須）。
	| { kind: "tui"; lane: string; session: number; data: string };

// ============================================================================
// calculations（純関数 — vitest 対象）
// ============================================================================

/** 会話に入る一行。item id があれば「item <id>」を挟む。読み手（他 engine）が予備知識
 *  なしで分かる自然文にする（doc 52 §3 — 内輪語の規約を読み手に要求しない）。 */
export function inkSendLine(itemId: string | null, path: string): string {
	const where = itemId ? `item ${itemId} ` : "";
	return `[対話面] board の ${where}に注釈を描いた。${path} の画像を見て意図を汲んでほしい`;
}

/** mode → 送信経路。**focused session の見え方**で決まる（doc 50 §4.6 A6 — 旧 lane 単位
 *  console_mode 依存から session 単位へ）。chat ならその session の会話へ、term ならその
 *  session の console（PTY）へ流す。
 *
 *  term 側が session を運ぶのは A6 で xterm が (lane, session) になったため — 旧実装は
 *  lane だけを送っており、非 root の console に描いても root へ流れていた。 */
export function inkRoute(
	mode: "tui" | "gui",
	laneAddr: string,
	session: number,
	line: string,
): InkRoute {
	if (mode === "gui") {
		return { kind: "gui", lane: laneAddr, session, prompt: line };
	}
	return { kind: "tui", lane: laneAddr, session, data: `${line}\r` };
}

/** 線/矢印が「点」レベルで短いか（誤タップの棄却）。freehand は点数で別判定。 */
export function resolveTooSmall(from: [number, number], to: [number, number]): boolean {
	return Math.hypot(to[0] - from[0], to[1] - from[1]) < 6;
}

// ============================================================================
// actions
// ============================================================================

const SVG_NS = "http://www.w3.org/2000/svg";

function svgEl<K extends keyof SVGElementTagNameMap>(
	name: K,
	attrs: Record<string, string | number>,
): SVGElementTagNameMap[K] {
	const el = document.createElementNS(SVG_NS, name);
	for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
	return el;
}

function ipcSend(payload: Record<string, unknown>): void {
	const ipc = (globalThis as unknown as { ipc?: { postMessage(m: string): void } }).ipc;
	if (!ipc || typeof ipc.postMessage !== "function") return;
	try {
		ipc.postMessage(JSON.stringify(payload));
	} catch (e) {
		console.warn("[ink] ipc failed", e);
	}
}

/** 生文字列 → UTF-8 bytes → base64（term:write の data 形式、main_area.rs xterm onData と同じ）。 */
function toBase64Utf8(s: string): string {
	const bytes = new TextEncoder().encode(s);
	let bin = "";
	for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
	return btoa(bin);
}

/** InkRoute を実際の IPC に写す（chat = conversation:submit / tui = term:write）。 */
function dispatchRoute(route: InkRoute): void {
	if (route.kind === "gui") {
		ipcSend({ t: "conversation:submit", lane: route.lane, session: route.session, prompt: route.prompt });
	} else {
		ipcSend({
			t: "term:write",
			lane: route.lane,
			session: route.session,
			data: toBase64Utf8(route.data),
		});
	}
}

/**
 * ink を DOM に配線する。#ink-stage / #ink-overlay / #ink-palette / #ink-text / #ink-toast は
 * main_area.rs HTML 側で保証される。board pane が roster から外れると #lane-board が display:none
 * になり、overlay ごと自然に隠れる（別の可視 gate は不要）。
 */
export function installInk(deps: InkDeps): InkPushHandlers | null {
	const stage = document.getElementById("ink-stage");
	// overlay = pointer を捕まえる div（box 全体）。canvas = 描画する svg（pointer-events:none）。
	// svg root を直に armed にすると空白部分が visiblePainted で透過し、下の文字が text 選択される。
	const overlay = document.getElementById("ink-overlay");
	const canvas = document.getElementById("ink-canvas") as unknown as SVGSVGElement | null;
	const palette = document.getElementById("ink-palette");
	const textInput = document.getElementById("ink-text") as HTMLInputElement | null;
	const toast = document.getElementById("ink-toast");
	if (!stage || !overlay || !canvas || !palette || !textInput) {
		console.warn("[ink] mount target 不在 — skip");
		// 受け手が居ない = 消費者不在。呼び手（entry.tsx）が no-op を埋める。
		return null;
	}

	let tool: InkTool = "select";
	const annotations: InkAnnotation[] = [];
	let drawing:
		| { kind: "line" | "arrow"; el: SVGLineElement; from: [number, number]; to: [number, number] }
		| { kind: "free"; el: SVGPathElement; points: [number, number][] }
		| null = null;
	/** 送信中に捕まえた lane（onSnapshot まで保持）。null = 送信中でない。 */
	let pendingLane: string | null = null;

	// ---- palette 構築 ----
	const INK = "var(--vp-ink-color)";
	const mkBtn = (id: string, title: string, inner: string): HTMLButtonElement => {
		const b = document.createElement("button");
		b.id = id;
		b.title = title;
		b.innerHTML = inner;
		palette.appendChild(b);
		return b;
	};
	const mkSep = (): void => {
		const s = document.createElement("span");
		s.className = "ink-sep";
		palette.appendChild(s);
	};
	const btnSelect = mkBtn(
		"ink-t-select",
		"選択（描かない）",
		`<svg width="15" height="15" viewBox="0 0 15 15"><path d="M3 1 L11.5 9 L7.5 9.5 L9.5 13.5 L7.8 14.2 L5.9 10.2 L3 13 Z" fill="currentColor"/></svg>`,
	);
	mkSep();
	const btnLine = mkBtn(
		"ink-t-line",
		"line",
		`<svg width="16" height="16" viewBox="0 0 16 16"><line x1="2" y1="14" x2="14" y2="2" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>`,
	);
	const btnArrow = mkBtn(
		"ink-t-arrow",
		"arrow",
		`<svg width="16" height="16" viewBox="0 0 16 16"><line x1="2" y1="14" x2="12" y2="4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><path d="M8 3 L13 3 L13 8" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
	);
	const btnText = mkBtn("ink-t-text", "text", `<span style="font-weight:700;font-size:15px">T</span>`);
	const btnFree = mkBtn(
		"ink-t-free",
		"freehand",
		`<svg width="18" height="16" viewBox="0 0 18 16"><path d="M2 12 C4 4, 7 4, 9 9 S 14 13, 16 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>`,
	);
	mkSep();
	const btnUndo = mkBtn(
		"ink-undo",
		"undo",
		`<svg width="16" height="16" viewBox="0 0 16 16"><path d="M6 3 L2 7 L6 11 M2.5 7 H10 a4 4 0 0 1 0 8 H7" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
	);
	const btnClear = mkBtn(
		"ink-clear",
		"全部消す",
		`<svg width="14" height="14" viewBox="0 0 14 14"><path d="M2 2 L12 12 M12 2 L2 12" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>`,
	);
	mkSep();
	const btnSend = mkBtn("ink-send", "送信", "送信");
	btnSend.disabled = true;

	const toolButtons: Record<InkTool, HTMLButtonElement> = {
		select: btnSelect,
		line: btnLine,
		arrow: btnArrow,
		text: btnText,
		free: btnFree,
	};

	// ---- 座標・ヘルパ ----
	const pt = (e: PointerEvent): [number, number] => {
		const r = overlay.getBoundingClientRect();
		return [Math.round(e.clientX - r.left), Math.round(e.clientY - r.top)];
	};
	const refreshSend = (): void => {
		btnSend.disabled = annotations.length === 0 || pendingLane !== null;
	};
	const showToast = (msg: string, isError: boolean): void => {
		if (!toast) return;
		toast.textContent = msg;
		toast.classList.toggle("ink-error", isError);
		toast.classList.add("show");
		window.setTimeout(() => toast.classList.remove("show"), 2200);
	};

	// 開いている text 入力を確定する（別道具に切替 / 別位置を置く / 送信 の前に呼ぶ）。
	// const arrow で guard 後に定義することで overlay / textInput の non-null narrowing が効く
	// （hoisted function declaration には narrowing が効かないため）。
	const commitTextIfAny = (): void => {
		if (textInput.style.display !== "block") return;
		const v = textInput.value.trim();
		const x = Number(textInput.dataset.x);
		const y = Number(textInput.dataset.y);
		textInput.style.display = "none";
		if (!v) return;
		const t = svgEl("text", { x, y, class: "ink-note" });
		t.setAttribute("dominant-baseline", "middle");
		t.textContent = v;
		canvas.appendChild(t);
		annotations.push({ kind: "text", els: [t] });
		refreshSend();
	};

	const setTool = (t: InkTool): void => {
		commitTextIfAny();
		tool = t;
		for (const [k, b] of Object.entries(toolButtons)) {
			b.classList.toggle("ink-active", k === t);
		}
		overlay.classList.toggle("ink-off", t === "select");
	};
	for (const [k, b] of Object.entries(toolButtons)) {
		b.addEventListener("click", () => setTool(k as InkTool));
	}

	// ---- line / arrow / freehand ----
	overlay.addEventListener("pointerdown", (e) => {
		if (tool === "select") return;
		// 送信の snapshot 往復中（pendingLane !== null）は描画を止める。ここで描くと撮影に
		// 入らないのに onSnapshot の clearAll() で黙って消え「描いた線が消えた」になる（team-b）。
		if (pendingLane !== null) return;
		if (tool === "text") {
			placeTextInput(...pt(e));
			return;
		}
		e.preventDefault();
		overlay.setPointerCapture(e.pointerId);
		const [x, y] = pt(e);
		if (tool === "free") {
			const path = svgEl("path", {
				d: `M${x},${y}`,
				fill: "none",
				stroke: INK,
				"stroke-width": 2.5,
				"stroke-linecap": "round",
				"stroke-linejoin": "round",
			});
			canvas.appendChild(path);
			drawing = { kind: "free", el: path, points: [[x, y]] };
		} else {
			const line = svgEl("line", {
				x1: x,
				y1: y,
				x2: x,
				y2: y,
				stroke: INK,
				"stroke-width": 2.5,
				"stroke-linecap": "round",
			});
			if (tool === "arrow") line.setAttribute("marker-end", "url(#ink-arrowhead)");
			canvas.appendChild(line);
			drawing = { kind: tool, el: line, from: [x, y], to: [x, y] };
		}
	});
	overlay.addEventListener("pointermove", (e) => {
		if (!drawing) return;
		const [x, y] = pt(e);
		if (drawing.kind === "free") {
			const last = drawing.points[drawing.points.length - 1];
			if (Math.hypot(x - last[0], y - last[1]) < 3) return;
			drawing.points.push([x, y]);
			drawing.el.setAttribute("d", `${drawing.el.getAttribute("d")} L${x},${y}`);
		} else {
			drawing.to = [x, y];
			drawing.el.setAttribute("x2", String(x));
			drawing.el.setAttribute("y2", String(y));
		}
	});
	const finishStroke = (): void => {
		if (!drawing) return;
		const d = drawing;
		drawing = null;
		const tooSmall =
			d.kind === "free" ? d.points.length < 3 : resolveTooSmall(d.from, d.to);
		if (tooSmall) {
			d.el.remove();
			return;
		}
		annotations.push({ kind: d.kind === "free" ? "freehand" : d.kind, els: [d.el] });
		refreshSend();
	};
	overlay.addEventListener("pointerup", finishStroke);
	overlay.addEventListener("pointercancel", finishStroke);

	// ---- text ----
	const placeTextInput = (x: number, y: number): void => {
		commitTextIfAny();
		textInput.style.left = `${x}px`;
		textInput.style.top = `${y}px`;
		textInput.dataset.x = String(x);
		textInput.dataset.y = String(y);
		textInput.value = "";
		textInput.style.display = "block";
		textInput.focus();
	};
	textInput.addEventListener("keydown", (e) => {
		if (e.key === "Enter") commitTextIfAny();
		if (e.key === "Escape") {
			textInput.value = "";
			textInput.style.display = "none";
		}
	});

	// ---- undo / clear ----
	const clearAll = (): void => {
		while (annotations.length) {
			const a = annotations.pop();
			a?.els.forEach((el) => el.remove());
		}
		refreshSend();
	};
	btnUndo.addEventListener("click", () => {
		const a = annotations.pop();
		a?.els.forEach((el) => el.remove());
		refreshSend();
	});
	btnClear.addEventListener("click", clearAll);

	// ---- 明示送信: palette を隠す → snapshot 要求（Rust）→ onSnapshot で会話へ ----
	btnSend.addEventListener("click", () => {
		commitTextIfAny();
		if (annotations.length === 0 || pendingLane !== null) return;
		const laneAddr = deps.getLaneAddress();
		if (!laneAddr) {
			showToast("lane が未選択のため送れません", true);
			return;
		}
		pendingLane = laneAddr;
		refreshSend();
		// 撮影の一瞬 palette を隠す（写り込み防止）。DOM 反映を 1 フレーム跨いでから撮る。
		palette.style.visibility = "hidden";
		if (toast) toast.classList.remove("show");
		requestAnimationFrame(() => {
			const r = stage.getBoundingClientRect();
			ipcSend({
				t: "ink:snapshot",
				rect: { x: r.left, y: r.top, w: r.width, h: r.height },
			});
		});
	});

	// ---- Rust 注入口: snapshot 完了 / 失敗 ----
	const onSnapshot = (payload: { path?: string }): void => {
		palette.style.visibility = "";
		const laneAddr = pendingLane;
		pendingLane = null;
		const path = payload?.path;
		if (!laneAddr || !path) {
			refreshSend();
			showToast("snapshot に失敗しました", true);
			return;
		}
		const itemId = deps.getItemId();
		const session = deps.getFocusedSession(laneAddr);
		// doc 50 §4.6 A6: 送り先は focused **session の mode**（旧: lane 単位 console_mode）。
		const mode = deps.getSessionMode(laneAddr, session);
		const line = inkSendLine(itemId, path);
		dispatchRoute(inkRoute(mode, laneAddr, session, line));
		clearAll();
		showToast(mode === "gui" ? "focused session に送りました" : "console に送りました", false);
	};
	const onSnapshotError = (payload: { message?: string }): void => {
		palette.style.visibility = "";
		pendingLane = null;
		refreshSend(); // annotations は残す（再送できるように）
		showToast(`snapshot 失敗: ${payload?.message ?? "unknown"}`, true);
	};
	// Rust からの供給は **push envelope**（`ink:snapshot` / `ink:snapshot_error`）で、
	// この window face は DevTools からの手動 trigger 用に残してある。
	(window as unknown as { vpInk: { onSnapshot: typeof onSnapshot; onSnapshotError: typeof onSnapshotError } }).vpInk =
		{ onSnapshot, onSnapshotError };
	return {
		inkSnapshot: (path) => onSnapshot({ path }),
		inkSnapshotError: (message) => onSnapshotError({ message }),
	};
}
