/**
 * board-view — board pane の view 層（doc 55: 投影と表示所有権）。
 *
 * board の表示は「内容の事実（presence = 非空）」ではなく **user の明示状態**が決める
 * （doc 55 §5 — item が永続化されると board はほぼ常に非空になり、presence は表示駆動の
 * 座を降りた）。本 module がその view 状態 `{open, form, floatRect}` の SSOT。
 *
 * - **form = 投影**（doc 55 §4: float | docked）。doc 50 A6「mode は session の属性」と
 *   同型の「form は board pane の属性」— 同じ pane の 2 つの見え方で、モデルの分岐ではない
 * - **float**: 右側余白に浮かぶ層。開閉・新着・移動・リサイズで**他 pane は 1px も動かない**
 *   （doc 55 の reflow 規律 — 他 pane が動くのは dock⇄undock の user 操作のときだけ）
 * - **docked**: 従来の tiling 配置。roster への出入りは lane-panes（'vp:board-view' 購読）が担う
 * - **AI はこの層に書けない**（doc 55 §4.3 — 「奪わない」の形態レベル保証。MCP に表示動詞は無い）
 * - 状態は in-memory（doc 55 §9 = 先行 + 乗せ替え。A7 layout 永続の到着時に移設する）
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: module-local `states`（lane flat key → BoardViewState）+ `badges`（新着未読）
 * - **calculations**: `toggleOpen` / `toggleForm` / `initialFloatRect` / `clampRect` /
 *   `moveRect` / `resizeRect` — 純関数（vitest で固定）
 * - **actions**: `installBoardView`（取っ手 / form ボタン / drag / resize の配線 + float 投影 +
 *   'vp:board-view' dispatch）。shortcut（keybindings.ts）からは `toggleBoardOpen` /
 *   `toggleBoardForm` が入口
 */

/** board pane の投影（doc 55 §4）。 */
export type BoardForm = "float" | "docked";

/** float 時の矩形。**workbench（#lane-panes）左上原点の px**。lane ごとに記憶され、
 *  window resize では clamp した見た目だけ縮む（記憶自体は壊さない — doc 55 §7.1）。 */
export type FloatRect = { x: number; y: number; w: number; h: number };

/** lane 1 つぶんの view 状態。rect = null は「まだ触っていない」= 初回既定値を使う。 */
export type BoardViewState = {
	open: boolean;
	form: BoardForm;
	rect: FloatRect | null;
};

/** 既定 = 閉 × float。表示は user が起こす（doc 55 §5 — presence 自動表示の退役）。 */
export const DEFAULT_BOARD_VIEW: BoardViewState = {
	open: false,
	form: "float",
	rect: null,
};

/** float の最小サイズ（名札 + 数行が読める程度、doc 55 §7.1）。 */
export const FLOAT_MIN_W = 260;
export const FLOAT_MIN_H = 160;
/** 初回配置の余白（右端からの逃げ。初回にしか効かない — 一度動かせば記憶が正）。 */
const FLOAT_MARGIN = 12;

// ============================================================================
// calculations（純関数 — vitest 対象）
// ============================================================================

/** 開閉 toggle（Ctrl+Shift+B / 取っ手）。form は保つ = 閉→開は前回の form のまま。 */
export function toggleOpen(s: BoardViewState): BoardViewState {
	return { ...s, open: !s.open };
}

/** form toggle（Ctrl+Shift+N / 名札ボタン）。開いていれば float ⇄ docked。
 *  閉じていれば「切替先の form で開く」に化ける（doc 55 §7 — B が「前回のまま開く」を
 *  担うので、N は「もう片方で開きたい」の意図に割り当たる）。 */
export function toggleForm(s: BoardViewState): BoardViewState {
	const next: BoardForm = s.form === "float" ? "docked" : "float";
	return { ...s, open: true, form: next };
}

const clampN = (v: number, lo: number, hi: number): number =>
	Math.min(Math.max(v, lo), Math.max(lo, hi));

/** 矩形を workbench 内へ収める。サイズは [min, workbench]、位置は はみ出さない範囲。
 *  workbench が最小サイズより狭い縮退時はサイズ最小を優先し右下がはみ出す（迷子より可読）。 */
export function clampRect(
	r: FloatRect,
	wb: { w: number; h: number },
): FloatRect {
	const w = clampN(r.w, FLOAT_MIN_W, wb.w);
	const h = clampN(r.h, FLOAT_MIN_H, wb.h);
	const x = clampN(r.x, 0, wb.w - w);
	const y = clampN(r.y, 0, wb.h - h);
	return { x, y, w, h };
}

/** 初回既定値（doc 55 §9、mako 裁定 2026-07-30）: 縦 = workbench の 92% /
 *  横 = 縦 × 1/√2（A4 縦置き比率）。右端に寄せ、縦は中央。 */
export function initialFloatRect(wb: { w: number; h: number }): FloatRect {
	const h = Math.round(wb.h * 0.92);
	const w = Math.round(h * Math.SQRT1_2);
	return clampRect(
		{ x: wb.w - w - FLOAT_MARGIN, y: Math.round((wb.h - h) / 2), w, h },
		wb,
	);
}

/** 移動（名札 drag）。サイズ不変で位置だけ動かし、workbench 内に clamp。 */
export function moveRect(
	r: FloatRect,
	dx: number,
	dy: number,
	wb: { w: number; h: number },
): FloatRect {
	return clampRect({ ...r, x: r.x + dx, y: r.y + dy }, wb);
}

/** リサイズの取っ手。当初は右寄せ想定で左系（w/s/sw）だけだったが、実機 dogfood
 *  （mako 2026-07-30）で「いつもの挙動 = 右下ドラッグで縦横」が求められ右系（e/se）を追加。
 *  上辺は名札 = 移動の取っ手なので n 系は作らない。 */
export type ResizeDir = "w" | "s" | "e" | "sw" | "se";

/** リサイズ（縁/角 drag）。w = 左辺（右端 anchor）、e = 右辺（左端 anchor）、
 *  s = 下辺（上端 anchor）。角は両軸の合成。 */
export function resizeRect(
	r: FloatRect,
	dir: ResizeDir,
	dx: number,
	dy: number,
	wb: { w: number; h: number },
): FloatRect {
	let { x, y, w, h } = r;
	if (dir.includes("w")) {
		const right = x + w;
		w = clampN(w - dx, FLOAT_MIN_W, right);
		x = right - w;
	} else if (dir.includes("e")) {
		w = clampN(w + dx, FLOAT_MIN_W, wb.w - x);
	}
	if (dir.includes("s")) {
		h = clampN(h + dy, FLOAT_MIN_H, wb.h - y);
	}
	return clampRect({ x, y, w, h }, wb);
}

// ============================================================================
// data（module-local。in-memory — A7 到着で layout 永続へ乗せ替え）
// ============================================================================

/** lane flat key（'conductor' / performer 名 — board-handler と同じ key 系）→ view 状態。 */
const states = new Map<string, BoardViewState>();
/** 閉時に fresh（新着）が来た lane（doc 55 §5.2 — 取っ手 badge の点灯元）。開くと消える。 */
const badges = new Set<string>();

function stateOf(flat: string): BoardViewState {
	return states.get(flat) ?? DEFAULT_BOARD_VIEW;
}

// ============================================================================
// actions（installBoardView + shortcut 入口）
// ============================================================================

export interface BoardViewDeps {
	/** #lane-board（board の静的 host。float 投影の対象）。 */
	board: HTMLElement;
	/** #lane-panes（workbench = float の座標系 + clamp 境界）。 */
	workbench: HTMLElement;
	/** #board-handle（取っ手 — 開閉の入口 + 新着 badge）。 */
	handle: HTMLElement;
	/** #board-form-btn（board 名札の form 切替ボタン）。 */
	formBtn: HTMLElement;
}

interface BoardViewController {
	setActiveLane(flat: string | null): void;
	toggleOpen(): void;
	toggleForm(): void;
}

/** singleton（keybindings / entry から module 関数越しに触る）。 */
let controller: BoardViewController | null = null;

/** 開閉 toggle（Ctrl+Shift+B）。未 install / lane 不在は no-op。 */
export function toggleBoardOpen(): void {
	controller?.toggleOpen();
}

/** form toggle（Ctrl+Shift+N）。未 install / lane 不在は no-op。 */
export function toggleBoardForm(): void {
	controller?.toggleForm();
}

export function installBoardView(deps: BoardViewDeps): BoardViewController {
	let activeFlat: string | null = null;
	/** drag / resize 中の一時 rect（確定は pointerup で states へ）。 */
	let liveRect: FloatRect | null = null;

	const wbSize = (): { w: number; h: number } => ({
		w: deps.workbench.clientWidth,
		h: deps.workbench.clientHeight,
	});

	/** 表示中 lane の float 矩形（記憶 or 初回既定値を clamp した見た目）。 */
	const viewRect = (s: BoardViewState): FloatRect => {
		const wb = wbSize();
		return s.rect ? clampRect(s.rect, wb) : initialFloatRect(wb);
	};

	const writeRect = (r: FloatRect): void => {
		deps.board.style.left = `${r.x}px`;
		deps.board.style.top = `${r.y}px`;
		deps.board.style.width = `${r.w}px`;
		deps.board.style.height = `${r.h}px`;
	};

	/** roster への出入りを lane-panes へ知らせる（docked の tiling 反映は向こうの仕事）。 */
	const dispatchView = (flat: string): void => {
		const s = stateOf(flat);
		document.dispatchEvent(
			new CustomEvent("vp:board-view", {
				detail: { lane: flat, open: s.open, form: s.form },
			}),
		);
	};

	/** view 状態 → DOM への投影。float は本 module が rect を書き、docked / 閉は
	 *  lane-panes（roster / stray）が display を決める。 */
	const project = (): void => {
		const flat = activeFlat;
		// lane 不在: 取っ手ごと消す（#lane-empty の世界）
		deps.handle.style.display = flat ? "" : "none";
		if (!flat) return;
		const s = stateOf(flat);
		const floating = s.open && s.form === "float";
		deps.board.classList.toggle("board-floating", floating);
		if (floating) {
			writeRect(liveRect ?? viewRect(s));
			deps.board.style.display = "";
		} else if (!s.open) {
			// 閉: 即座に消す（lane-panes の stray 掃除を待たない）。#899 の教訓どおり
			// hit-test から外す隠し方（display:none）で、透明 iframe の wheel 吸いを防ぐ。
			deps.board.style.display = "none";
		} else {
			// docked: inline px を返上し、lane-panes render の % 書き込みに委ねる
			deps.board.style.left = "";
			deps.board.style.top = "";
			deps.board.style.width = "";
			deps.board.style.height = "";
		}
		// 取っ手: badge / 状態 tooltip
		deps.handle.classList.toggle("has-badge", badges.has(flat));
		// form ボタン: 表示はこれから行ける先（docked 中は Float、float 中は Dock）
		deps.formBtn.textContent = s.form === "float" ? "Dock" : "Float";
	};

	const mutate = (
		flat: string,
		fn: (s: BoardViewState) => BoardViewState,
	): void => {
		const next = fn(stateOf(flat));
		states.set(flat, next);
		if (next.open) badges.delete(flat); // 開いた = 見た（badge 消灯）
		liveRect = null;
		project();
		dispatchView(flat);
	};

	// ---- drag（移動: 名札）と resize（縁/角） ----------------------------------

	/** pointer 操作の共通配線。setPointerCapture で iframe 上でも追跡が切れない
	 *  （保険で .board-interacting を立て、iframe の pointer-events を落とす —
	 *  doc 55 実装留意 / ink overlay と同じ流儀）。 */
	const wireDrag = (
		el: HTMLElement,
		apply: (r: FloatRect, dx: number, dy: number) => FloatRect,
		guard?: (e: PointerEvent) => boolean,
	): void => {
		el.addEventListener("pointerdown", (e) => {
			const flat = activeFlat;
			if (!flat) return;
			const s = stateOf(flat);
			if (!(s.open && s.form === "float")) return;
			if (guard && !guard(e)) return;
			e.preventDefault();
			const start = liveRect ?? viewRect(s);
			const sx = e.clientX;
			const sy = e.clientY;
			el.setPointerCapture(e.pointerId);
			deps.board.classList.add("board-interacting");
			/** drag 開始時の文脈がまだ生きているか。drag 中に外部要因（Ctrl+Shift+N /
			 *  MCP switch_lane 等）で lane・form が変わったら、以降の書き込みを捨てる —
			 *  誤った lane への rect 上書きと、docked へ遷移後の px 直書き（lane-panes render
			 *  との二重書き手化）を防ぐ（moody-blues finding #2、2026-07-30）。 */
			const dragAlive = (): boolean => {
				const cur = stateOf(flat);
				return activeFlat === flat && cur.open && cur.form === "float";
			};
			const onMove = (ev: PointerEvent): void => {
				if (!dragAlive()) return;
				liveRect = apply(start, ev.clientX - sx, ev.clientY - sy);
				writeRect(liveRect);
			};
			const onUp = (): void => {
				el.removeEventListener("pointermove", onMove);
				el.removeEventListener("pointerup", onUp);
				el.removeEventListener("pointercancel", onUp);
				deps.board.classList.remove("board-interacting");
				// 確定 = 記憶（lane ごと）。文脈が死んでいたら捨てる（上記 guard と同じ理由）
				if (liveRect && dragAlive()) {
					states.set(flat, { ...stateOf(flat), rect: liveRect });
				}
				liveRect = null;
			};
			el.addEventListener("pointermove", onMove);
			el.addEventListener("pointerup", onUp);
			el.addEventListener("pointercancel", onUp);
		});
	};

	// 移動: 名札 drag（ボタン類は素通し）
	const plate = deps.board.querySelector<HTMLElement>(".board-plate");
	if (plate) {
		wireDrag(
			plate,
			(r, dx, dy) => moveRect(r, dx, dy, wbSize()),
			(e) => !(e.target as HTMLElement | null)?.closest("button, input"),
		);
	}

	// リサイズ: 縁/角の取っ手（float 時のみ CSS が表示）
	for (const dir of ["w", "s", "e", "sw", "se"] as const) {
		const rs = document.createElement("div");
		rs.className = `board-rs board-rs-${dir}`;
		deps.board.appendChild(rs);
		wireDrag(rs, (r, dx, dy) => resizeRect(r, dir, dx, dy, wbSize()));
	}

	// ---- 取っ手 / form ボタン ---------------------------------------------------

	deps.handle.addEventListener("click", () => {
		if (activeFlat) mutate(activeFlat, toggleOpen);
	});
	deps.formBtn.addEventListener("click", () => {
		if (activeFlat) mutate(activeFlat, toggleForm);
	});

	// ---- 新着 badge（doc 55 §5.2 — fresh は表示を起こさず、閉時は取っ手で知らせる） ----

	document.addEventListener("vp:board-presence", (e) => {
		const d = (
			e as CustomEvent<{ lane: string; present: boolean; fresh?: boolean }>
		).detail;
		if (!d?.lane || !d.fresh) return;
		if (stateOf(d.lane).open) return; // 開いていれば見えている（focus 寄せは lane-panes）
		badges.add(d.lane);
		if (d.lane === activeFlat) project();
	});

	// ---- workbench resize: clamp を当て直す（記憶 rect は壊さない、doc 55 §7.1） ----

	new ResizeObserver(() => {
		const flat = activeFlat;
		if (!flat) return;
		const s = stateOf(flat);
		if (s.open && s.form === "float" && !liveRect) writeRect(viewRect(s));
	}).observe(deps.workbench);

	const ctl: BoardViewController = {
		setActiveLane(flat) {
			if (activeFlat === flat) return;
			activeFlat = flat;
			liveRect = null;
			project();
			if (flat) dispatchView(flat); // 新 lane の roster を向こうにも同期させる
		},
		toggleOpen() {
			if (activeFlat) mutate(activeFlat, toggleOpen);
		},
		toggleForm() {
			if (activeFlat) mutate(activeFlat, toggleForm);
		},
	};
	controller = ctl;
	return ctl;
}
