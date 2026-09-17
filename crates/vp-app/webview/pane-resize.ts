/** lane 内の列境界。幅の正本は既存レイアウトの attention に一本化する。 */
import { resolve, type Layout } from "@chronista-club/creo-ui-layout";

export type PaneBoundary = { left: number; right: number; x: number };

function columnsOf(layout: Layout) {
	const resolved = resolve(layout);
	return layout.structure.columns.map((column, index) => {
		const rect = column.panes.map(id => resolved[id]?.rect).find(r => r && r.w > 0 && r.h > 0);
		return { index, rect, locked: column.panes.some(id => (layout.locks?.[id] ?? 0) > 0) };
	}).filter(c => c.rect !== undefined);
}

export function paneBoundaries(layout: Layout): PaneBoundary[] {
	const columns = columnsOf(layout);
	return columns.slice(1).flatMap((right, i) => {
		const left = columns[i]!;
		return left.locked || right.locked ? [] : [{ left: left.index, right: right.index, x: right.rect!.x }];
	});
}

/** 幅固定されていない隣接2列で幅を移し、列内の縦比率は維持する。 */
export function resizeColumns(layout: Layout, left: number, right: number, delta: number, width: number): Layout {
	if (!Number.isFinite(delta) || !Number.isFinite(width) || width <= 0) return layout;
	if (!paneBoundaries(layout).some(b => b.left === left && b.right === right)) return layout;
	const columns = columnsOf(layout);
	const lw = columns.find(c => c.index === left)!.rect!.w;
	const rw = columns.find(c => c.index === right)!.rect!.w;
	// すでに狭い列も、境界をつかんだ瞬間に幅が跳ばないようにする。
	const minimum = Math.min(120 / width, lw, rw);
	const transfer = Math.max(minimum - lw, Math.min(rw - minimum, delta));
	const attention = { ...layout.attention };
	for (const [index, factor] of [[left, (lw + transfer) / lw], [right, (rw - transfer) / rw]]) {
		for (const id of layout.structure.columns[index!]!.panes) {
			attention[id] = (layout.attention[id] ?? 0) * factor!;
		}
	}
	return { ...layout, attention };
}

export function installPaneResizers(container: HTMLElement, update: (layout: Layout) => void, settle: () => void) {
	let scope = "";
	let layout: Layout;
	const handles = new Map<string, HTMLElement>();
	let drag: { handle: HTMLElement; pointer: number; x: number; width: number; initial: Layout; boundary: PaneBoundary; expected: string; changed: boolean } | undefined;
	const finish = (commit: boolean) => {
		const previous = drag;
		drag = undefined;
		if (!previous) return;
		previous.handle.classList.remove("dragging");
		document.body.classList.remove("pane-resizing");
		if (previous.handle.hasPointerCapture(previous.pointer)) previous.handle.releasePointerCapture(previous.pointer);
		if (commit && previous.changed) settle();
	};
	const sync = (nextScope: string, next: Layout) => {
		// mode・構成・外部レイアウトの変更時は、古いドラッグ操作を解除する。
		if (drag && (scope !== nextScope || JSON.stringify(next) !== drag.expected)) finish(false);
		scope = nextScope;
		layout = next;
		const live = new Set<string>();
		for (const boundary of paneBoundaries(next)) {
			const key = `${boundary.left}:${boundary.right}`;
			live.add(key);
			let handle = handles.get(key);
			if (!handle) {
				handle = document.createElement("div");
				handle.className = "pane-resizer";
				handle.setAttribute("role", "separator");
				handle.setAttribute("aria-orientation", "vertical");
				handle.setAttribute("aria-label", "Pane の幅を変更");
				handle.title = "ドラッグして Pane の幅を変更";
				handle.tabIndex = 0;
				const element = handle;
				handle.addEventListener("pointerdown", e => {
					if (e.button !== 0 || drag) return;
					const width = container.getBoundingClientRect().width;
					if (width <= 0) return;
					e.preventDefault();
					e.stopPropagation();
					element.setPointerCapture(e.pointerId);
					drag = { handle: element, pointer: e.pointerId, x: e.clientX, width, initial: layout, boundary, expected: JSON.stringify(layout), changed: false };
					element.classList.add("dragging");
					document.body.classList.add("pane-resizing");
				});
				handle.addEventListener("pointermove", e => {
					if (!drag || drag.handle !== element || drag.pointer !== e.pointerId) return;
					const next = resizeColumns(drag.initial, drag.boundary.left, drag.boundary.right, (e.clientX - drag.x) / drag.width, drag.width);
					drag.expected = JSON.stringify(next);
					drag.changed = drag.expected !== JSON.stringify(drag.initial);
					update(next);
				});
				for (const name of ["pointerup", "pointercancel", "lostpointercapture"]) {
					handle.addEventListener(name, e => {
						if (drag?.handle === element && drag.pointer === (e as PointerEvent).pointerId) finish(true);
					});
				}
				handle.addEventListener("keydown", e => {
					if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
					e.preventDefault();
					e.stopPropagation();
					const width = container.getBoundingClientRect().width;
					if (width <= 0 || drag) return;
					update(resizeColumns(layout, boundary.left, boundary.right, (e.key === "ArrowLeft" ? -16 : 16) / width, width));
					settle();
				});
				container.append(handle);
				handles.set(key, handle);
			}
			handle.style.left = `${boundary.x * 100}%`;
			handle.setAttribute("aria-valuenow", String(Math.round(boundary.x * 100)));
		}
		for (const [key, handle] of handles) {
			if (!live.has(key)) { handle.remove(); handles.delete(key); }
		}
	};
	const cancel = () => finish(true);
	window.addEventListener("blur", cancel);
	window.addEventListener("resize", cancel);
	return { sync, dispose() {
		finish(false);
		window.removeEventListener("blur", cancel);
		window.removeEventListener("resize", cancel);
		for (const handle of handles.values()) handle.remove();
		handles.clear();
	} };
}
