import * as THREE from "three";
import { deviceLayout, type DeviceAppearance } from "./midi-device-layout";

export interface DeviceModel {
	group: THREE.Group;
	setAppearance: (state: DeviceAppearance, selected: boolean) => void;
	dispose: () => void;
}

/** All controls keep their semantic IDs; the renderer needn't know MIDI addresses. */
export function createDeviceModel(deviceId: string, profile: string): DeviceModel {
	const layout = deviceLayout(profile);
	const group = new THREE.Group();
	group.userData.deviceId = deviceId;
	const chassis = new THREE.MeshStandardMaterial({ color: 0x242a32, roughness: 0.78 });
	const control = new THREE.MeshStandardMaterial({ color: 0x637080, roughness: 0.6 });
	const light = new THREE.MeshStandardMaterial({ color: 0x4b706b, roughness: 0.48 });
	const rail = new THREE.MeshStandardMaterial({ color: 0x111820 });
	const materials = [chassis, control, light, rail];
	const geometries: THREE.BufferGeometry[] = [];
	const mesh = (geometry: THREE.BufferGeometry, material: THREE.Material, x: number, y: number, z: number, parent: THREE.Object3D = group) => {
		geometries.push(geometry);
		const object = new THREE.Mesh(geometry, material);
		object.position.set(x, y, z);
		parent.add(object);
		return object;
	};
	mesh(new THREE.BoxGeometry(layout.width, layout.height, layout.depth), chassis, 0, layout.height / 2, 0);
	for (const c of layout.controls) {
		const part = new THREE.Group();
		part.name = c.id;
		part.userData.controlId = c.id;
		part.position.set(c.x, layout.height, c.z);
		group.add(part);
		if (c.kind === "knob") {
			mesh(new THREE.CylinderGeometry(c.width / 2, c.width * 0.6, 0.19, 16), control, 0, 0.12, 0, part);
			mesh(new THREE.TorusGeometry(c.width * 0.7, 0.021, 6, 20), light, 0, 0.018, 0, part).rotation.x = -Math.PI / 2;
			mesh(new THREE.BoxGeometry(0.026, 0.018, c.width * 0.4), light, 0, 0.223, -c.width * 0.18, part);
		} else if (c.kind === "fader") {
			mesh(new THREE.BoxGeometry(0.047, 0.02, c.depth), rail, 0, 0.016, 0, part);
			mesh(new THREE.BoxGeometry(c.width, 0.09, 0.15), control, 0, 0.065, 0, part);
		} else {
			mesh(new THREE.BoxGeometry(c.width, c.kind === "lcd" ? 0.022 : 0.055, c.depth), light, 0, 0.035, 0, part);
		}
	}
	const outlineGeometry = new THREE.EdgesGeometry(new THREE.BoxGeometry(layout.width + 0.09, layout.height + 0.07, layout.depth + 0.09));
	const outlineMaterial = new THREE.LineBasicMaterial({ color: 0x81e6d9 });
	const outline = new THREE.LineSegments(outlineGeometry, outlineMaterial);
	outline.position.y = layout.height / 2;
	group.add(outline);
	return {
		group,
		setAppearance(state, selected) {
			const wire = state === "unsupported";
			const held = state === "held";
			for (const material of materials) {
				material.wireframe = wire;
				material.transparent = state === "released";
				material.opacity = state === "released" ? 0.38 : 1;
				material.depthWrite = state !== "released";
				material.emissive.setHex(0);
			}
			chassis.color.setHex(state === "idle" ? 0x4b5058 : 0x242a32);
			light.color.setHex(held ? 0x63ddc5 : 0x6a7079);
			light.emissive.setHex(held ? 0x227e70 : 0);
			outline.visible = selected;
		},
		dispose() {
			for (const geometry of geometries) geometry.dispose();
			for (const material of materials) material.dispose();
			outlineGeometry.dispose();
			outlineMaterial.dispose();
		},
	};
}

/** Fit all eight corners in camera space; a sphere wastes space for wide fleets. */
export function fitDeviceCamera(camera: THREE.PerspectiveCamera, bounds: THREE.Box3): { target: THREE.Vector3; distance: number } {
	const vertical = THREE.MathUtils.degToRad(camera.fov / 2);
	const horizontal = Math.atan(Math.tan(vertical) * camera.aspect);
	const target = bounds.getCenter(new THREE.Vector3());
	let distance = 3;
	for (const x of [bounds.min.x, bounds.max.x]) for (const y of [bounds.min.y, bounds.max.y]) for (const z of [bounds.min.z, bounds.max.z]) {
		const dy = y - target.y;
		const dz = z - target.z;
		const depth = (dy + dz) / Math.SQRT2;
		const up = (dy - dz) / Math.SQRT2;
		distance = Math.max(distance, depth + 1.18 * Math.max(Math.abs(x - target.x) / Math.tan(horizontal), Math.abs(up) / Math.tan(vertical)));
	}
	camera.position.copy(target).add(new THREE.Vector3(0, 1, 1).normalize().multiplyScalar(distance));
	camera.near = Math.max(0.01, distance / 1000);
	camera.far = distance * 20;
	camera.lookAt(target);
	camera.updateProjectionMatrix();
	camera.updateMatrixWorld();
	return { target, distance };
}
