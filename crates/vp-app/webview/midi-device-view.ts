import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { deviceAppearance, deviceLayout, deviceGrid } from "./midi-device-layout";
import { createDeviceModel, fitDeviceCamera, type DeviceModel } from "./midi-device-scene";
import type { MidiUseStatus } from "./midi-use";

/** Static scenes only render on demand. Hiding cancels even an already queued frame. */
export function createVisibleFrame(
	draw: () => void,
	request: typeof requestAnimationFrame = requestAnimationFrame,
	cancel: typeof cancelAnimationFrame = cancelAnimationFrame,
) {
	let frame: number | undefined;
	let visible = false;
	let dirty = true;
	let disposed = false;
	const schedule = () => {
		if (disposed || !visible || !dirty || frame !== undefined) return;
		frame = request(() => {
			frame = undefined;
			if (disposed || !visible) return;
			dirty = false;
			draw();
		});
	};
	return {
		invalidate() { dirty = true; schedule(); },
		setVisible(next: boolean) {
			visible = next;
			if (!visible && frame !== undefined) { cancel(frame); frame = undefined; }
			if (visible) { dirty = true; schedule(); }
		},
		dispose() { disposed = true; if (frame !== undefined) cancel(frame); frame = undefined; },
	};
}

export interface MidiDeviceView {
	element: HTMLElement;
	update: (status: MidiUseStatus) => void;
	select: (id: string | null) => void;
	dispose: () => void;
}

export function createMidiDeviceView(onSelect: (id: string) => void): MidiDeviceView {
	const element = document.createElement("div");
	element.className = "midi-device-view";
	const viewport = document.createElement("div");
	viewport.className = "midi-device-viewport";
	viewport.setAttribute("aria-label", "MIDI 機材の 3D 表示");
	const canvas = document.createElement("canvas");
	canvas.setAttribute("aria-label", "ドラッグで回転、スクロールで拡大・縮小。機材名で選択できます");
	const labels = document.createElement("div");
	labels.className = "midi-device-labels";
	const notice = document.createElement("p");
	notice.className = "midi-device-notice";
	const toolbar = document.createElement("div");
	toolbar.className = "midi-device-toolbar";
	const help = document.createElement("span");
	help.textContent = "ドラッグで回転 · スクロールで拡大・縮小";
	const reset = document.createElement("button");
	reset.type = "button";
	reset.textContent = "全体を見る";
	toolbar.append(help, reset);
	viewport.append(canvas, labels, notice);
	element.append(viewport, toolbar);
	const scene = new THREE.Scene();
	scene.background = new THREE.Color(0x141b22);
	scene.add(new THREE.HemisphereLight(0xd8e8ff, 0x444039, 2.3));
	const key = new THREE.DirectionalLight(0xffffff, 3);
	key.position.set(-6, 12, 8);
	scene.add(key);
	const fleet = new THREE.Group();
	scene.add(fleet);
	const camera = new THREE.PerspectiveCamera(42, 1, 0.1, 1000);
	const bounds = new THREE.Box3();
	const models = new Map<string, { model: DeviceModel; label: HTMLButtonElement; anchor: THREE.Vector3; depth: number }>();
	let renderer: THREE.WebGLRenderer | undefined;
	let controls: OrbitControls | undefined;
	let signature = "";
	let selected: string | null = null;
	let status: MidiUseStatus = {};
	let fitNeeded = true;
	let disposed = false;
	let failed = false;
	let contextLost = false;
	let intersecting = true;
	let width = 0;
	let height = 0;
	const events = new AbortController();
	const raycaster = new THREE.Raycaster();
	const point = new THREE.Vector2();
	const projected = new THREE.Vector3();
	const arrange = () => {
		const { columns, rows } = deviceGrid(models.size, width / height || 1);
		let index = 0;
		for (const { model, anchor, depth } of models.values()) {
			model.group.position.set((index % columns - (columns - 1) / 2) * 6.9, 0, (Math.floor(index / columns) - (rows - 1) / 2) * 4.5);
			anchor.copy(model.group.position).add(new THREE.Vector3(0, 0, depth / 2 + 0.42));
			index++;
		}
	};

	const fit = () => {
		bounds.setFromObject(fleet);
		if (bounds.isEmpty()) bounds.set(new THREE.Vector3(-1, 0, -1), new THREE.Vector3(1, 1, 1));
		const result = fitDeviceCamera(camera, bounds);
		if (controls) {
			controls.target.copy(result.target);
			controls.minDistance = Math.max(1, result.distance * 0.25);
			controls.maxDistance = result.distance * 3;
			controls.update();
		}
		fitNeeded = false;
	};
	const isVisible = () => !disposed && !failed && !contextLost && intersecting &&
		!document.hidden && element.isConnected && viewport.getClientRects().length > 0 &&
		viewport.clientWidth > 0 && viewport.clientHeight > 0 && getComputedStyle(viewport).visibility !== "hidden";
	const queue = createVisibleFrame(() => {
		// Check again: a pane can be hidden between an observer callback and the frame.
		if (!isVisible()) return;
		if (!renderer) {
			try {
				renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: "low-power" });
				renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
				controls = new OrbitControls(camera, canvas);
				controls.enablePan = false;
				controls.enableDamping = false;
				controls.maxPolarAngle = Math.PI * 0.48;
				controls.minPolarAngle = Math.PI * 0.08;
				controls.addEventListener("change", queue.invalidate);
			} catch {
				failed = true;
				notice.textContent = "3D 表示を利用できません。下の一覧から機材を確認できます。";
				labels.hidden = true;
				reset.disabled = true;
				queue.setVisible(false);
				return;
			}
		}
		const nextWidth = viewport.clientWidth;
		const nextHeight = viewport.clientHeight;
		if (width !== nextWidth || height !== nextHeight) {
			width = nextWidth;
			height = nextHeight;
			renderer.setSize(width, height, false);
			camera.aspect = width / height;
			arrange();
			fitNeeded = true;
		}
		if (fitNeeded) fit();
		renderer.render(scene, camera);
		for (const { label, anchor } of models.values()) {
			projected.copy(anchor).project(camera);
			label.hidden = projected.z < -1 || projected.z > 1;
			label.style.left = `${(projected.x * 0.5 + 0.5) * width}px`;
			label.style.top = `${(-projected.y * 0.5 + 0.5) * height}px`;
		}
	});
	const visibility = () => queue.setVisible(isVisible());
	// VP keeps hidden panes at full size; Resize/IntersectionObserver alone cannot
	// detect visibility:hidden -> visible. Observe ancestors only, never label styles.
	const attributes = new MutationObserver(visibility);
	let observingAncestors = false;
	const resize = new ResizeObserver(visibility);
	resize.observe(viewport);
	const intersection = new IntersectionObserver((entries) => {
		intersecting = entries[0]?.isIntersecting ?? false;
		visibility();
	});
	intersection.observe(viewport);
	document.addEventListener("visibilitychange", visibility, { signal: events.signal });
	canvas.addEventListener("webglcontextlost", (event) => {
		event.preventDefault();
		contextLost = true;
		notice.textContent = "3D 表示を復旧しています。下の一覧は引き続き使えます。";
		labels.hidden = true;
		visibility();
	}, { signal: events.signal });
	canvas.addEventListener("webglcontextrestored", () => {
		contextLost = false;
		notice.textContent = models.size ? "" : "機材が登録されるとここに表示されます";
		labels.hidden = false;
		visibility();
	}, { signal: events.signal });
	reset.onclick = () => { fitNeeded = true; queue.invalidate(); };
	let down: { id: number; x: number; y: number; moved: boolean } | null = null;
	canvas.addEventListener("pointerdown", (event) => {
		if (event.button === 0 && event.isPrimary) down = { id: event.pointerId, x: event.clientX, y: event.clientY, moved: false };
	}, { signal: events.signal });
	canvas.addEventListener("pointermove", (event) => {
		if (down && Math.hypot(event.clientX - down.x, event.clientY - down.y) > 5) down.moved = true;
	}, { signal: events.signal });
	canvas.addEventListener("pointercancel", () => { down = null; }, { signal: events.signal });
	canvas.addEventListener("pointerup", (event) => {
		const click = down;
		down = null;
		if (!click || click.id !== event.pointerId || click.moved || !isVisible()) return;
		const rect = canvas.getBoundingClientRect();
		point.set((event.clientX - rect.left) / rect.width * 2 - 1, -(event.clientY - rect.top) / rect.height * 2 + 1);
		raycaster.setFromCamera(point, camera);
		let object: THREE.Object3D | null | undefined = raycaster.intersectObject(fleet, true)[0]?.object;
		while (object && !object.userData.deviceId) object = object.parent;
		if (object?.userData.deviceId) onSelect(object.userData.deviceId);
	}, { signal: events.signal });
	const appearance = () => {
		for (const device of status.snapshot?.devices ?? []) {
			const item = models.get(device.device_id);
			if (!item) continue;
			item.model.setAppearance(deviceAppearance(device, status), selected === device.device_id);
			item.label.textContent = device.name;
			item.label.setAttribute("aria-pressed", String(selected === device.device_id));
		}
		queue.invalidate();
	};
	return {
		element,
		update(next) {
			if (!observingAncestors && element.isConnected) {
				for (let ancestor: HTMLElement | null = viewport; ancestor; ancestor = ancestor.parentElement) {
					attributes.observe(ancestor, { attributes: true, attributeFilter: ["style", "class", "hidden"] });
				}
				observingAncestors = true;
			}
			status = next;
			const devices = status.snapshot?.devices ?? [];
			const nextSignature = JSON.stringify(devices.map((d) => [d.device_id, d.profile_id]));
			if (signature !== nextSignature) {
				signature = nextSignature;
				for (const { model } of models.values()) model.dispose();
				models.clear();
				fleet.clear();
				labels.replaceChildren();
				devices.forEach((device) => {
					const model = createDeviceModel(device.device_id, device.profile_id);
					fleet.add(model.group);
					const label = document.createElement("button");
					label.type = "button";
					label.onclick = () => onSelect(device.device_id);
					labels.append(label);
					const anchor = new THREE.Vector3();
					models.set(device.device_id, { model, label, anchor, depth: deviceLayout(device.profile_id).depth });
				});
				arrange();
				fitNeeded = true;
			}
			if (!failed && !contextLost) notice.textContent = devices.length ? "" : "機材が登録されるとここに表示されます";
			appearance();
			visibility();
		},
		select(id) { selected = id; appearance(); },
		dispose() {
			disposed = true;
			queue.dispose();
			resize.disconnect();
			intersection.disconnect();
			attributes.disconnect();
			events.abort();
			controls?.dispose();
			for (const { model } of models.values()) model.dispose();
			models.clear();
			renderer?.dispose();
			scene.clear();
		},
	};
}
