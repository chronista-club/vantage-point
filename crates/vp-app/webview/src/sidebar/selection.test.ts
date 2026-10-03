/** mem_1Cffk9CyL6uzCaHqQaT8Pi — 選択タブとメインの色を Editor から調整する。 */
import { readFileSync } from "node:fs";
import { Window } from "happy-dom";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./Shell.tsx", import.meta.url), "utf8");
const css = source.match(/export const SHELL_CSS = `([\s\S]*)`;/)![1];

describe("Lane の選択面", () => {
	it("Editor の背景色変更は選択行とメインに届き、未選択行には届かない", () => {
		const window = new Window();
		try {
			const { document } = window;
			document.head.innerHTML = `<style>${css}</style><style>.chat-view { background: var(--color-bg, #0f1115); }</style>`;
			document.body.innerHTML = `<div id="app-shell">
				<div id="sidebar-root"><div class="vp-sidebar-list"><div class="vp-proj">
					<div id="first" class="vp-lane-row creo-sidenav-link" aria-current="page">portal</div>
					<div id="second" class="vp-lane-row creo-sidenav-link">main</div>
				</div></div></div><div id="host"><div class="chat-view"></div></div></div>`;
			const first = document.querySelector("#first")!;
			const second = document.querySelector("#second")!;
			const chat = document.querySelector(".chat-view")!;
			document.documentElement.style.setProperty("--sb-selection-bg", "#26334a");
			expect(window.getComputedStyle(first).backgroundColor).toBe("#26334a");
			expect(window.getComputedStyle(chat).backgroundColor).toBe("#26334a");
			expect(window.getComputedStyle(second).backgroundColor).not.toBe("#26334a");
			first.removeAttribute("aria-current");
			second.setAttribute("aria-current", "page");
			expect(window.getComputedStyle(first).backgroundColor).not.toBe("#26334a");
			expect(window.getComputedStyle(second).backgroundColor).toBe("#26334a");
		} finally {
			window.happyDOM.abort();
		}
	});
});
