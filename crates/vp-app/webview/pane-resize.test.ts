// @vitest-environment happy-dom
// Task: mem_1Cf8LMoTvEnXtru3m1CmW9
import { describe, expect, it } from "vitest";
import { resolve, type Layout } from "@chronista-club/creo-ui-layout";
import { resizeColumns, paneBoundaries, installPaneResizers } from "./pane-resize";

const initial: Layout = {
	structure: { columns: [{ panes: ["a"] }, { panes: ["b", "c"] }, { panes: ["d"] }] },
	attention: { a: 1, b: 1, c: 0.5, d: 1 },
};

describe("pane boundary resize", () => {
	it.each([0, 1, 2])("resizes boundary %i in a four-pane layout without moving other panes", (left) => {
		const ids = ["a", "b", "c", "d"];
		const layout: Layout = {
			structure: { columns: ids.map(id => ({ panes: [id] })) },
			attention: { a: 2, b: 3, c: 4, d: 5 },
		};
		const before = resolve(layout);
		const after = resolve(resizeColumns(layout, left, left + 1, 0.025, 2000));
		expect(paneBoundaries(layout)).toHaveLength(3);
		for (const [index, id] of ids.entries()) {
			if (index !== left && index !== left + 1) {
				expect(after[id].rect.x).toBeCloseTo(before[id].rect.x);
				expect(after[id].rect.w).toBeCloseTo(before[id].rect.w);
			}
		}
		expect(after[ids[left]].rect.w).toBeCloseTo(before[ids[left]].rect.w + 0.025);
	});
	it("moves only the adjacent columns and preserves stacked pane heights", () => {
		const next = resizeColumns(initial, 0, 1, 0.1, 1200);
		const before = resolve(initial), after = resolve(next);
		expect(after.a.rect.w).toBeCloseTo(before.a.rect.w + 0.1);
		expect(after.b.rect.w).toBeCloseTo(before.b.rect.w - 0.1);
		expect(after.d.rect).toEqual(before.d.rect);
		expect(after.b.rect.h).toBeCloseTo(before.b.rect.h);
		expect(initial.attention.a).toBe(1);
	});
	it("keeps both sides visible at the minimum width", () => {
		const next = resolve(resizeColumns(initial, 0, 1, 99, 1200));
		expect(next.b.rect.w * 1200).toBeCloseTo(120);
		expect(next.a.rect.w + next.b.rect.w).toBeCloseTo(2 / 3);
	});
	it("skips hidden columns and does not offer locked boundaries", () => {
		const hidden = { ...initial, attention: { a: 1, b: 0, c: 0, d: 1 } };
		expect(paneBoundaries(hidden).map(v => [v.left, v.right])).toEqual([[0, 2]]);
		expect(paneBoundaries({ ...hidden, locks: { a: 0.5 } })).toEqual([]);
	});
	it("pointer drag updates width, settles once, and cleans up on lane change", () => {
		const container = document.createElement("div");
		document.body.append(container);
		container.getBoundingClientRect = () => ({ width: 1200 } as DOMRect);
		let current = initial;
		let settles = 0;
		const ui = installPaneResizers(container, (next) => {
			current = next;
			ui.sync("lane:a", current);
		}, () => { settles++; });
		ui.sync("lane:a", current);
		const handle = container.querySelector<HTMLElement>("[role=separator]")!;
		expect(handle).not.toBeNull();
		handle.setPointerCapture = () => {};
		handle.releasePointerCapture = () => {};
		handle.hasPointerCapture = () => true;
		handle.dispatchEvent(new PointerEvent("pointerdown", { pointerId: 1, button: 0, clientX: 400 }));
		handle.dispatchEvent(new PointerEvent("pointermove", { pointerId: 1, clientX: 520 }));
		expect(resolve(current).a.rect.w).toBeCloseTo(400 / 1200 + 0.1);
		handle.dispatchEvent(new PointerEvent("pointerup", { pointerId: 1 }));
		expect(settles).toBe(1);
		handle.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft" }));
		expect(resolve(current).a.rect.w * 1200).toBeCloseTo(504);
		expect(settles).toBe(2);
		handle.dispatchEvent(new PointerEvent("pointerdown", { pointerId: 2, button: 0, clientX: 520 }));
		ui.sync("lane:b", initial);
		handle.dispatchEvent(new PointerEvent("pointermove", { pointerId: 2, clientX: 900 }));
		expect(document.body.classList.contains("pane-resizing")).toBe(false);
		expect(settles).toBe(2);
		ui.dispose();
		container.remove();
	});
});
