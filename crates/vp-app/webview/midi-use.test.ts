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
	vi.unstubAllGlobals();
	vi.useRealTimers();
});

it("resumes after an ancestor visibility change with no resize or new snapshot", async () => {
	vi.useFakeTimers();
	const request = vi.fn(() => 1);
	const cancel = vi.fn();
	vi.stubGlobal("requestAnimationFrame", request);
	vi.stubGlobal("cancelAnimationFrame", cancel);
	document.body.innerHTML = '<section id="pane" style="visibility:hidden"><div id="device-list"></div></section>';
	mountMidiUse(document.querySelector("#device-list")!, vi.fn());
	const viewport = document.querySelector<HTMLElement>(".midi-device-viewport")!;
	Object.defineProperties(viewport, {
		clientWidth: { value: 500 }, clientHeight: { value: 400 },
		getClientRects: { value: () => [{ width: 500, height: 400 }] },
	});
	renderMidiUse(state());
	expect(request).not.toHaveBeenCalled();
	const pane = document.querySelector<HTMLElement>("#pane")!;
	pane.style.visibility = "visible";
	await vi.advanceTimersByTimeAsync(0);
	expect(request).toHaveBeenCalledTimes(1);
	pane.style.visibility = "hidden";
	await vi.advanceTimersByTimeAsync(0);
	expect(cancel).toHaveBeenCalledWith(1);
	pane.style.visibility = "visible";
	await vi.advanceTimersByTimeAsync(0);
	expect(request).toHaveBeenCalledTimes(2);
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
	expect(document.body.textContent).not.toContain("VP で使用中");
	expect(
		(document.querySelector('[data-midi-device="roto"]') as HTMLInputElement)
			.disabled,
	).toBe(true);
});

it("marks a previous active lease as unverified after service disconnection", () => {
	vi.useFakeTimers();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, vi.fn());
	const previous = state();
	previous.snapshot.devices[0].assignment.client_id = "vp";
	previous.snapshot.devices[0].lease.session_id = "vp-session";
	renderMidiUse(previous);
	expect(document.body.textContent).toContain("VP で使用中");
	renderMidiUse({ ...previous, connected: false });
	expect(document.body.textContent).not.toContain("VP で使用中");
	expect(document.body.textContent).toContain("状態未確認");
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

// mem_1CfsA67B5KTNTQgChBcFbM — selecting a model must never acquire a device.
it("keeps a persistent 3D view above the settings and selects without MIDI commands", () => {
	vi.useFakeTimers();
	const send = vi.fn();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, send);
	renderMidiUse(state());
	const view = document.querySelector(".midi-device-view");
	expect(view).not.toBeNull();
	expect(document.querySelector('[data-device-area="top"]')?.childElementCount).toBe(0);
	expect(view?.parentElement?.getAttribute("data-device-area")).toBe("middle");
	expect(document.querySelector('[data-device-area="bottom"] #device-list')).not.toBeNull();
	const select = document.querySelector<HTMLButtonElement>('[data-midi-select="roto"]')!;
	expect(select).not.toBeNull();
	const calls = send.mock.calls.length;
	select.click();
	expect(select.getAttribute("aria-pressed")).toBe("true");
	expect(send.mock.calls.length).toBe(calls);
	renderMidiUse(state(9));
	expect(document.querySelector(".midi-device-view")).toBe(view);
	expect(document.querySelector('[data-midi-select="roto"]')?.getAttribute("aria-pressed")).toBe("true");
});

it("switches the main view and information with bottom tabs, including keyboard and removed devices", () => {
	vi.useFakeTimers();
	const send = vi.fn();
	document.body.innerHTML = '<div id="device-list"></div>';
	mountMidiUse(document.querySelector("#device-list")!, send);
	const snapshot = state();
	snapshot.snapshot.devices.push({ ...snapshot.snapshot.devices[0], device_id: "lpd", profile_id: "lpd8", name: "LPD8" });
	renderMidiUse(snapshot);
	const tabs = () => Array.from(document.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
	expect(tabs().map(t => t.textContent)).toEqual(["全体", "ROTO-CONTROL", "LPD8"]);
	expect(tabs()[0].closest('[data-device-area="bottom"]')).not.toBeNull();
	const calls = send.mock.calls.length;
	tabs()[1].click();
	expect(tabs()[1].getAttribute("aria-selected")).toBe("true");
	expect(document.querySelector<HTMLElement>('[data-midi-row="lpd"]')!.hidden).toBe(true);
	expect(document.querySelector<HTMLElement>('[data-midi-row="roto"]')!.hidden).toBe(false);
	expect(document.querySelector<HTMLElement>('#device-list')!.hidden).toBe(true);
	const labels = Array.from(document.querySelectorAll<HTMLButtonElement>('.midi-device-labels button'));
	expect(labels.map(l => l.hidden)).toEqual([false, true]);
	tabs()[1].dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
	expect(tabs()[2].getAttribute("aria-selected")).toBe("true");
	expect(document.activeElement).toBe(tabs()[2]);
	tabs()[2].dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true }));
	expect(tabs()[0].getAttribute("aria-selected")).toBe("true");
	expect(labels.every(l => !l.hidden)).toBe(true);
	expect(document.querySelector<HTMLElement>('#device-list')!.hidden).toBe(false);
	tabs()[2].click();
	renderMidiUse(state(10));
	expect(tabs()[0].getAttribute("aria-selected")).toBe("true");
	expect(send.mock.calls.length).toBe(calls);
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
