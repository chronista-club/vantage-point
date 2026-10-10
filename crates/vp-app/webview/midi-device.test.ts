// mem_1CfsA67B5KTNTQgChBcFbM — scene/ownership tests require no WebGL or MIDI hardware.
import { describe, expect, it } from "vitest";
import * as THREE from "three";
import { deviceAppearance, deviceLayout, deviceGrid } from "./midi-device-layout";
import { createDeviceModel, fitDeviceCamera } from "./midi-device-scene";
import { createVisibleFrame } from "./midi-device-view";
import type { Device, MidiUseStatus } from "./midi-use";

const device: Device = {
	device_id: "xtouch-1", profile_id: "xtouch", name: "X-Touch", present: true,
	phase: "active", assignment: { client_id: "vp", revision: 1 }, lease: { session_id: "vp-1" },
};
const status: MidiUseStatus = { connected: true, enabled: true, snapshot: { sequence: 1, session_id: "vp-1", devices: [device] } };

describe("ownership appearance", () => {
	it("lights only the current VP lease", () => {
		expect(deviceAppearance(device, status)).toBe("held");
		expect(deviceAppearance({ ...device, lease: { session_id: "other-vp" } }, status)).toBe("released");
		expect(deviceAppearance({ ...device, assignment: { client_id: "ladyland", revision: 2 } }, status)).toBe("released");
		expect(deviceAppearance({ ...device, lease: null }, status)).toBe("released");
	});
	it("never keeps a stale snapshot lit when service, presence, or master changes", () => {
		expect(deviceAppearance(device, { ...status, connected: false })).toBe("idle");
		expect(deviceAppearance({ ...device, present: false }, status)).toBe("idle");
		expect(deviceAppearance({ ...device, error: "lease expired" }, status)).toBe("idle");
		expect(deviceAppearance(device, { ...status, enabled: false })).toBe("released");
		expect(deviceAppearance({ ...device, phase: "releasing" }, status)).toBe("released");
	});
	it("does not confuse a modeled device with a supported MIDI mapping", () => {
		expect(deviceAppearance({ ...device, profile_id: "nanokontrol" }, status)).toBe("held");
		expect(deviceAppearance({ ...device, profile_id: "new-device" }, status)).toBe("unsupported");
	});
});

describe("schematic profiles", () => {
	it("uses fewer columns in a narrow pane without overlapping chassis", () => {
		expect(deviceGrid(5, 2.4).columns).toBe(3);
		expect(deviceGrid(5, 0.8).columns).toBe(2);
		expect(deviceGrid(5, 0.35).columns).toBe(1);
		expect(deviceGrid(0, 1)).toEqual({ columns: 1, rows: 0 });
	});
	it.each([
		["xtouch", "fader", 9], ["xtouch", "knob", 8], ["xtouch", "lcd", 8],
		["lpd8", "pad", 8], ["lpd8", "knob", 8],
		["nanokontrol", "fader", 8], ["nanokontrol", "knob", 8],
		["roto", "lcd", 8], ["roto", "knob", 8],
	])("%s exposes %s controls (%i)", (profile, kind, count) => {
		expect(deviceLayout(profile as string).controls.filter((c) => c.kind === kind)).toHaveLength(count as number);
	});
	it.each(["xtouch", "lpd8", "nanokontrol", "roto", "unknown"])("%s has stable distinct IDs and fits its chassis", (profile) => {
		const layout = deviceLayout(profile);
		expect(layout).toEqual(deviceLayout(profile));
		expect(new Set(layout.controls.map((c) => c.id)).size).toBe(layout.controls.length);
		for (const c of layout.controls) {
			expect(Math.abs(c.x) + c.width / 2).toBeLessThan(layout.width / 2);
			expect(Math.abs(c.z) + c.depth / 2).toBeLessThan(layout.depth / 2);
		}
		if (profile === "unknown") expect(layout.controls).toEqual([]);
	});
});

it("models keep control addresses, update materials, and release their resources", () => {
	const model = createDeviceModel("lpd8-1", "lpd8");
	expect(model.group.userData.deviceId).toBe("lpd8-1");
	expect(model.group.getObjectByName("pad.0")?.userData.controlId).toBe("pad.0");
	const meshes: THREE.Mesh[] = [];
	model.group.traverse((object) => { if (object instanceof THREE.Mesh) meshes.push(object); });
	model.setAppearance("released", false);
	expect(meshes.every((m) => (m.material as THREE.MeshStandardMaterial).transparent)).toBe(true);
	model.setAppearance("held", true);
	expect(meshes.every((m) => (m.material as THREE.MeshStandardMaterial).opacity === 1)).toBe(true);
	expect(meshes.some((m) => (m.material as THREE.MeshStandardMaterial).emissive.getHex() !== 0)).toBe(true);
	model.setAppearance("unsupported", false);
	expect(meshes.every((m) => (m.material as THREE.MeshStandardMaterial).wireframe)).toBe(true);
	let disposed = 0;
	const geometries = new Set(meshes.map((m) => m.geometry));
	for (const geometry of geometries) geometry.addEventListener("dispose", () => { disposed++; });
	model.dispose();
	expect(disposed).toBe(geometries.size);
});

it.each([0.35, 1, 3])("fits every corner of the fleet at 45 degrees with aspect %s", (aspect) => {
	const camera = new THREE.PerspectiveCamera(42, aspect);
	const bounds = new THREE.Box3(new THREE.Vector3(-10, 0, -5), new THREE.Vector3(10, 1, 5));
	const { target } = fitDeviceCamera(camera, bounds);
	expect(camera.position.y - target.y).toBeCloseTo(camera.position.z - target.z);
	let occupied = 0;
	for (const x of [bounds.min.x, bounds.max.x]) for (const y of [bounds.min.y, bounds.max.y]) for (const z of [bounds.min.z, bounds.max.z]) {
		const point = new THREE.Vector3(x, y, z).project(camera);
		expect(Math.abs(point.x)).toBeLessThan(1);
		expect(Math.abs(point.y)).toBeLessThan(1);
		expect(Math.abs(point.z)).toBeLessThan(1);
		occupied = Math.max(occupied, Math.abs(point.x), Math.abs(point.y));
	}
	expect(occupied).toBeGreaterThan(0.65);
});

it("coalesces redraws and cancels all frames while hidden or disposed", () => {
	const callbacks = new Map<number, FrameRequestCallback>();
	let id = 0;
	let rendered = 0;
	const queue = createVisibleFrame(() => rendered++, (callback) => { callbacks.set(++id, callback); return id; }, (key) => { callbacks.delete(key); });
	const flush = () => { for (const [key, callback] of callbacks) { callbacks.delete(key); callback(0); } };
	queue.invalidate();
	expect(callbacks.size).toBe(0);
	queue.setVisible(true);
	queue.invalidate();
	queue.invalidate();
	expect(callbacks.size).toBe(1);
	queue.setVisible(false);
	expect(callbacks.size).toBe(0);
	flush();
	expect(rendered).toBe(0);
	queue.setVisible(true);
	flush();
	expect(rendered).toBe(1);
	expect(callbacks.size).toBe(0); // no perpetual animation loop
	queue.invalidate();
	queue.dispose();
	queue.setVisible(true);
	flush();
	expect(rendered).toBe(1);
});
