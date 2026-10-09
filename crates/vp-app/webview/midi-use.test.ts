// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { mountMidiUse, renderMidiUse } from "./midi-use";
const state = (revision = 3) => ({
	enabled: true,
	connected: true,
	snapshot: {
		sequence: revision,
		session_id: "vp-session",
		devices: [
			{
				device_id: "roto",
				profile_id: "roto",
				name: "ROTO-CONTROL",
				present: true,
				phase: "active",
				assignment: { client_id: "ladyland", revision },
				lease: { session_id: "other" },
				native_ports: { inputs: [], outputs: [] },
			},
		],
	},
});
afterEach(() => {
	document.body.innerHTML = "";
	vi.restoreAllMocks();
	vi.useRealTimers();
});
it("offers an explicit enable action before allowing per-device changes", () => {
	vi.useFakeTimers();
	const send = vi.fn();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, send);
	renderMidiUse({ ...state(), enabled: false });
	const enable = document.querySelector<HTMLButtonElement>("button[data-midi-master]");
	expect(enable?.textContent).toBe("MIDI を有効にする");
	expect(document.body.textContent).toContain("先に MIDI を有効にしてください");
	enable!.click();
	expect(send).toHaveBeenLastCalledWith({ master_enabled: true });
	expect(document.querySelector<HTMLInputElement>("[data-midi-device]")!.disabled).toBe(true);
	renderMidiUse(state());
	expect(document.querySelector<HTMLInputElement>("[data-midi-device]")!.disabled).toBe(false);
	const stop = document.querySelector<HTMLButtonElement>("button[data-midi-master]")!;
	expect(stop.textContent).toBe("MIDI を停止する");
	stop.click();
	expect(send).toHaveBeenLastCalledWith({ master_enabled: false });
});
it("requires explicit takeover and retains the revision the user saw", () => {
	vi.useFakeTimers();
	const send = vi.fn();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, send);
	renderMidiUse(state());
	(
		document.querySelector('[data-midi-device="roto"]') as HTMLInputElement
	).click();
	expect(send.mock.calls.some(([p]) => p.set)).toBe(false);
	renderMidiUse(state(4));
	(document.querySelector("[data-midi-confirm]") as HTMLButtonElement).click();
	expect(send).toHaveBeenLastCalledWith({
		set: {
			device_id: "roto",
			enabled: true,
			expected_revision: 3,
			takeover: true,
		},
	});
});
it("keeps per-device settings disabled when the master or service is unavailable", () => {
	vi.useFakeTimers();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, vi.fn());
	const off = state();
	off.enabled = false;
	off.snapshot.devices[0].assignment.client_id = "vp";
	off.snapshot.devices[0].lease.session_id = "vp-session";
	renderMidiUse(off);
	expect(document.body.textContent).not.toContain("VP で使用中");
	expect(
		(document.querySelector('[data-midi-device="roto"]') as HTMLInputElement)
			.disabled,
	).toBe(true);
	expect(document.body.textContent).toContain("全体 OFF");
	renderMidiUse({ ...state(), connected: false });
	expect(
		(document.querySelector('[data-midi-device="roto"]') as HTMLInputElement)
			.disabled,
	).toBe(true);
});
it("does not replace controls on unchanged polling snapshots", () => {
	vi.useFakeTimers();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, vi.fn());
	renderMidiUse(state());
	const control = document.querySelector('[data-midi-device="roto"]');
	const update = state();
	update.snapshot.sequence = 50;
	renderMidiUse(update);
	expect(document.querySelector('[data-midi-device="roto"]')).toBe(control);
});
it("updates the master switch even when a caller reuses its snapshot object", () => {
	vi.useFakeTimers();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, vi.fn());
	const snapshot = state();
	renderMidiUse(snapshot);
	snapshot.enabled = false;
	renderMidiUse(snapshot);
	expect(document.body.textContent).toContain("全体 OFF");
	expect(
		(document.querySelector('[data-midi-device="roto"]') as HTMLInputElement)
			.disabled,
	).toBe(true);
});

it("allows nanoKONTROL2 takeover through the same revision confirmation", () => {
	vi.useFakeTimers();
	const send = vi.fn();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, send);
	const next = state();
	Object.assign(next.snapshot.devices[0], { device_id: "nanokontrol", profile_id: "nanokontrol", name: "nanoKONTROL2" });
	renderMidiUse(next);
	const toggle = document.querySelector('[data-midi-device="nanokontrol"]') as HTMLInputElement;
	expect(toggle.disabled).toBe(false);
	toggle.click();
	expect(send.mock.calls.some(([p]) => p.set)).toBe(false);
	(document.querySelector("[data-midi-confirm]") as HTMLButtonElement).click();
	expect(send).toHaveBeenLastCalledWith({set:{ device_id:"nanokontrol", enabled:true, expected_revision:3, takeover:true }});
});
