/**
 * fleet — 物理艦隊の入力を gallery layout engine への操作に写す mapping registry
 * （doc 49 LE-19: 機材 → 面の対応は consumer 供給）。
 *
 * 入力経路: Devices 🧲 (World) → world-device channel → app.rs `fleet_dispatch_js`
 * → `window.vpFleet.dispatch(payload)` → mapControl（本 module、純 calculation）
 * → gallery-panes.tsx の action 層が engine に適用する。
 *
 * 型が対応を予言する（LE-19）:
 *   多本の連続値（場）→ knob 群（ROTO / LPD8） / 1 本のスカラー（t）→ fader（X-Touch 1 番）
 *   / 離散の選択肢（Scene）→ pad（LPD8）。構造は演奏対象ではない（画面と AI の領分）。
 *
 * 机上の 3 台（2026-07-23 mako 確定）: ROTO-CONTROL / X-Touch / LPD8 mk2。
 */

/** ControlEvent の wire 形（midistage-profiles `device_input.rs` の serde tag 形式） */
export interface FleetControlEvent {
	type: string;
	index: number;
	value?: number;
	pressed?: boolean;
	velocity?: number;
}

/** DeviceEvent::ControlEvent の payload（app.rs `fleet_dispatch_js` が転送する形） */
export interface FleetPayload {
	kind?: string;
	port_name?: string;
	event?: FleetControlEvent;
}

/**
 * mapping の出力 = gallery engine への論理操作。
 * - share: knob 値 = share 指定（§2 の二重読み。pane は structure 順の index 対応）
 * - touch: 触れた/離した（LE-16 Touch — press = 奪取、release = 着地/settle）
 * - scrub: t の hand driver（進行中 transition の t を直接持つ）
 * - pad: Scene slot（tap = apply / 長押し = capture。時間判定は action 層）
 */
export type FleetOp =
	| { op: "share"; paneIndex: number; share: number }
	| { op: "touch"; source: string; pressed: boolean }
	| { op: "scrub"; t: number }
	| { op: "pad"; slot: number; pressed: boolean };

export type FleetDevice = "roto" | "xtouch" | "lpd8";

/** port 名 → 機材種別（Devices の port pattern と同じ部分一致規約） */
export function deviceOf(portName: string): FleetDevice | null {
	if (portName.includes("Roto")) return "roto";
	if (portName.includes("X-Touch")) return "xtouch";
	if (portName.includes("LPD8")) return "lpd8";
	return null;
}

/** share の上限（gallery slider の max と同値 — 1 独占は click/solo の領分に残す） */
const MAX_KNOB_SHARE = 0.95;

const clamp01 = (n: number): number => Math.min(1, Math.max(0, n));

// ---------- フィードバック方向（場 → 機材の投影、LE-19） ----------

/** wire 形（daemon/protocol.rs `FleetFeedback` と一致 — 正規化 0..1） */
export interface FleetFeedback {
	knobs: { index: number; value: number }[];
	fader: number | null;
	pads: { index: number; filled: boolean }[];
}

/** LPD8 の pad 数 = Scene slot 数 */
const PAD_COUNT = 8;

/**
 * 場の状態 → 機材への投影指示（純 calculation）。
 * - knobs: structure 順の member share。**touch 保持中の knob は省く**（Touch 中 = 指定、
 *   release 後 = 表示 — §9。手とモーターを戦わせない）
 * - fader: 進行中 transition の t。無し / fader touch 中は null（動かさない）
 * - pads: Scene slot の占有状態（filled = 点灯）
 */
export function computeFeedback(opts: {
	memberOrder: readonly string[];
	shares: Readonly<Record<string, number>>;
	transitionT: number | null;
	filledSlots: ReadonlySet<number>;
	touched: ReadonlySet<string>;
}): FleetFeedback {
	const knobs: { index: number; value: number }[] = [];
	for (let i = 0; i < Math.min(opts.memberOrder.length, 8); i++) {
		if (opts.touched.has(`roto:${i}`)) continue;
		const id = opts.memberOrder[i];
		if (id === undefined) continue;
		knobs.push({ index: i, value: clamp01(opts.shares[id] ?? 0) });
	}
	const fader =
		opts.transitionT != null && !opts.touched.has("xtouch:fader0")
			? clamp01(opts.transitionT)
			: null;
	const pads = Array.from({ length: PAD_COUNT }, (_, index) => ({
		index,
		filled: opts.filledSlots.has(index),
	}));
	return { knobs, fader, pads };
}

/** 物理入力 1 件 → 論理操作（対応が無ければ null = 無視）。純 calculation */
export function mapControl(portName: string, event: FleetControlEvent): FleetOp | null {
	const device = deviceOf(portName);
	if (!device) return null;

	switch (device) {
		case "roto":
			if (event.type === "knob" && typeof event.value === "number") {
				return {
					op: "share",
					paneIndex: event.index,
					share: Math.min(clamp01(event.value), MAX_KNOB_SHARE),
				};
			}
			if (event.type === "knob_touch") {
				// source = センサー個体の識別子。物理 touch は 8 本独立なので、release の
				// 「全部手放した」判定（settle の刻み時）は action 層が Set で数える
				return { op: "touch", source: `roto:${event.index}`, pressed: event.pressed === true };
			}
			return null;
		case "xtouch":
			// t は 1 本のスカラー — fader 1 番（index 0）だけが scrub を持つ
			if (event.type === "fader" && event.index === 0 && typeof event.value === "number") {
				return { op: "scrub", t: clamp01(event.value) };
			}
			if (event.type === "fader_touch" && event.index === 0) {
				return { op: "touch", source: "xtouch:fader0", pressed: event.pressed === true };
			}
			return null;
		case "lpd8":
			if (event.type === "knob" && typeof event.value === "number") {
				return {
					op: "share",
					paneIndex: event.index,
					share: Math.min(clamp01(event.value), MAX_KNOB_SHARE),
				};
			}
			if (event.type === "pad") {
				return { op: "pad", slot: event.index, pressed: (event.velocity ?? 0) > 0 };
			}
			return null;
	}
}
