import type { ControlKind, DeviceLayout } from "./midi-device-layout";

export function controlAddress(value: string): { type: string; index: number } | null {
	const match = /^(knob|knob_touch|fader|fader_touch|button|pad)\.(0|[1-9]\d*)$/.exec(value);
	return match && Number(match[2]) <= 255 ? { type: match[1], index: Number(match[2]) } : null;
}

export function gearId(value: string): string | null {
	return /^[a-z0-9][a-z0-9_-]{0,63}$/.test(value) ? (value === "nanokontrol2" ? "nanokontrol" : value) : null;
}

export interface GearPart {
	name: string;
	kind: string;
	center: [number, number];
	size: [number, number, number];
	base?: number;
	control?: string;
}
export interface GearMetadata {
	size: [number, number, number];
	parts: GearPart[];
}
const vector = (value: unknown, length: number, positive = false): value is number[] =>
	Array.isArray(value) && value.length === length && value.every((n) => typeof n === "number" && Number.isFinite(n) && (!positive || n > 0));

export function gearLayout(value: unknown): DeviceLayout | null {
	if (!value || typeof value !== "object") return null;
	const data = value as GearMetadata;
	if (!vector(data.size, 3, true) || !Array.isArray(data.parts)) return null;
	const controls: DeviceLayout["controls"] = [];
	const names = new Set<string>();
	for (const part of data.parts) {
		if (!part || typeof part.name !== "string" || names.has(part.name) || typeof part.kind !== "string" ||
			!vector(part.center, 2) || !vector(part.size, 3, true) ||
			(part.base !== undefined && !Number.isFinite(part.base))) return null;
		names.add(part.name);
		if (part.control === undefined) continue;
		if (typeof part.control !== "string") return null;
		const address = controlAddress(part.control);
		if (!address) return null;
		const kind = address.type.replace("_touch", "") as ControlKind;
		controls.push({ id: part.control, kind, x: part.center[0] / 100, z: part.center[1] / 100, width: part.size[0] / 100, depth: part.size[1] / 100 });
	}
	// Existing scenes use decimetres: both GLB metres and metadata mm map here.
	return { width: data.size[0] / 100, depth: data.size[1] / 100, height: data.size[2] / 100, controls };
}
