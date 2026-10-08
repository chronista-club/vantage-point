// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { renderDevices } from "./devices";

afterEach(() => {
	document.body.innerHTML = "";
	vi.useRealTimers();
});

it("keeps raw ports collapsed and preserves expansion across device updates", () => {
	vi.useFakeTimers();
	document.body.innerHTML = '<div id="device-list"></div>';
	const ports = [{ port_name: "nanoKONTROL2 <CTRL>", has_input: true, has_output: false, hold_reason: "unsupported" }];
	renderDevices(ports);
	const details = document.querySelector<HTMLDetailsElement>("#device-list details");
	expect(details).not.toBeNull();
	expect(details!.open).toBe(false);
	expect(details!.querySelector("summary")?.textContent).toBe("接続ポート詳細 · 1 ポート");
	expect(details!.textContent).toContain("nanoKONTROL2 <CTRL>");
	expect(details!.querySelector("ctrl")).toBeNull();
	expect(details!.textContent).not.toContain("対応外");
	details!.open = true;
	renderDevices([...ports, { port_name: "ROTO", has_input: true, has_output: true }]);
	expect(document.querySelector<HTMLDetailsElement>("#device-list details")!.open).toBe(true);
	expect(document.querySelectorAll(".midi-use-settings")).toHaveLength(1);
});
