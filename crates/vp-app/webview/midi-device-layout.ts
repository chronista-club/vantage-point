import type { Device, MidiUseStatus } from "./midi-use";

export const midiControlProfiles = new Set(["roto", "lpd8", "xtouch", "nanokontrol"]);
export type DeviceAppearance = "held" | "released" | "idle" | "unsupported";
export type ControlKind = "knob" | "fader" | "pad" | "button" | "lcd";
export interface DeviceControl {
	/** Stable within a profile; future live values address device_id + this id. */
	id: string;
	kind: ControlKind;
	x: number;
	z: number;
	width: number;
	depth: number;
}
export interface DeviceLayout {
	width: number;
	depth: number;
	height: number;
	controls: DeviceControl[];
}

export function deviceGrid(count: number, aspect: number): { columns: number; rows: number } {
	const columns = Math.max(1, Math.min(Math.ceil(Math.sqrt(count)), Math.floor(Math.sqrt(count * aspect))));
	return { columns, rows: Math.ceil(count / columns) };
}

/** Ownership alone does not prove a live lease. Never light a stale snapshot. */
export function deviceAppearance(d: Device, status: MidiUseStatus): DeviceAppearance {
	if (!status.connected || !d.present || d.error) return "idle";
	if (!midiControlProfiles.has(d.profile_id)) return "unsupported";
	if (status.enabled && d.phase === "active" && d.assignment.client_id === "vp" &&
		d.lease?.session_id === status.snapshot?.session_id && !!d.lease?.session_id) return "held";
	return "released";
}

/** Display geometry, not a MIDI parser. Protocol counts: midistage-profiles/device_profile.rs.
 * Coordinates are schematic (not measured CAD); nanoKONTROL remains unsupported by VP. */
export function deviceLayout(profile: string): DeviceLayout {
	const controls: DeviceControl[] = [];
	const add = (id: string, kind: ControlKind, x: number, z: number, width = 0.22, depth = width) =>
		controls.push({ id, kind, x, z, width, depth });
	if (profile === "xtouch") {
		for (let i = 0; i < 8; i++) {
			const x = -2.5 + i * 0.58;
			add(`strip.${i}.lcd`, "lcd", x, -1.13, 0.48, 0.28);
			add(`strip.${i}.knob`, "knob", x, -0.66);
			for (const [j, name] of ["rec", "solo", "mute", "select"].entries())
				add(`strip.${i}.${name}`, "button", x, -0.25 + j * 0.26, 0.2, 0.15);
			add(`strip.${i}.fader`, "fader", x, 1.08, 0.25, 0.85);
		}
		add("master.fader", "fader", 2.35, 0.62, 0.25, 1.65);
		for (let i = 0; i < 5; i++) add(`transport.${i}`, "button", 1.87 + (i % 2) * 0.48, -1.13 + Math.floor(i / 2) * 0.3);
		return { width: 6, depth: 3.5, height: 0.4, controls };
	}
	if (profile === "lpd8") {
		for (let i = 0; i < 8; i++) {
			add(`pad.${i}`, "pad", -2.06 + (i % 4) * 0.6, -0.42 + Math.floor(i / 4) * 0.75, 0.5, 0.56);
			add(`knob.${i}`, "knob", 0.65 + (i % 4) * 0.46, -0.43 + Math.floor(i / 4) * 0.76);
		}
		return { width: 5, depth: 2, height: 0.22, controls };
	}
	if (profile === "nanokontrol" || profile === "nanokontrol2") {
		for (let i = 0; i < 8; i++) {
			const x = -1.3 + i * 0.47;
			add(`strip.${i}.knob`, "knob", x, -0.5, 0.19);
			add(`strip.${i}.fader`, "fader", x, 0.28, 0.18, 0.85);
			for (let j = 0; j < 3; j++) add(`strip.${i}.button.${j}`, "button", x + 0.2, -0.01 + j * 0.26, 0.12, 0.15);
		}
		for (let i = 0; i < 6; i++) add(`transport.${i}`, "button", -2.26 + (i % 2) * 0.34, -0.35 + Math.floor(i / 2) * 0.36, 0.23);
		return { width: 5, depth: 1.8, height: 0.19, controls };
	}
	if (profile === "roto") {
		for (let i = 0; i < 8; i++) {
			const x = -2.17 + i * 0.62;
			add(`knob.${i}`, "knob", x, 0.28, 0.3);
			add(`lcd.${i}`, "lcd", x, -0.36, 0.48, 0.38);
		}
		return { width: 5.4, depth: 1.65, height: 0.32, controls };
	}
	return { width: 3.3, depth: 1.8, height: 0.3, controls };
}
