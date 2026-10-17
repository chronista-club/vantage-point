import * as THREE from "three";
import type { DeviceModel } from "./midi-device-scene";
import { controlAddress, gearLayout, type GearMetadata } from "./midi-gear";

/** Clone per view; shared CPU image sources stay in the cache, GPU resources are disposable. */
export function createGearModel(id: string, root: THREE.Group, metadata: GearMetadata): DeviceModel | null {
	const layout = gearLayout(metadata);
	if (!layout) return null;
	const sourceNames = new Set<string>();
	root.traverse((node) => sourceNames.add(node.name));
	if (metadata.parts.some((part) => !sourceNames.has(part.name))) return null;
	const group = new THREE.Group();
	group.userData.deviceId = id;
	const visual = root.clone(true);
	visual.scale.multiplyScalar(10); // glTF metres -> scene decimetres
	group.add(visual);
	group.updateMatrixWorld(true);
	const materials: { material: THREE.MeshStandardMaterial; color: THREE.Color; opacity: number; transparent: boolean; depthWrite: boolean }[] = [];
	const geometries = new Map<THREE.BufferGeometry, THREE.BufferGeometry>();
	const clonedMaterials = new Map<THREE.Material, THREE.Material>();
	const textures = new Map<THREE.Texture, THREE.Texture>();
	const nodes = new Map<string, THREE.Object3D>();
	const cloneMaterial = (source: THREE.Material) => {
		const existing = clonedMaterials.get(source);
		if (existing) return existing;
		const material = source.clone() as THREE.MeshStandardMaterial;
		clonedMaterials.set(source, material);
		for (const [key, value] of Object.entries(material)) {
			if (!(value instanceof THREE.Texture)) continue;
			let texture = textures.get(value);
			if (!texture) { texture = value.clone(); textures.set(value, texture); }
			(material as unknown as Record<string, unknown>)[key] = texture;
		}
		if (material.color) materials.push({ material, color: material.color.clone(), opacity: material.opacity, transparent: material.transparent, depthWrite: material.depthWrite });
		return material;
	};
	visual.traverse((object) => {
		nodes.set(object.name, object);
		if (!(object instanceof THREE.Mesh)) return;
		const original = object.geometry as THREE.BufferGeometry;
		const geometry = geometries.get(original) ?? original.clone();
		geometries.set(original, geometry);
		object.geometry = geometry;
		object.material = Array.isArray(object.material) ? object.material.map(cloneMaterial) : cloneMaterial(object.material);
	});
	for (const part of metadata.parts) {
		const node = nodes.get(part.name);
		if (!node) continue;
		// Metadata owns planar placement; preserve the GLB's modeled height/rotation.
		const position = node.getWorldPosition(new THREE.Vector3());
		position.x = part.center[0] / 100;
		position.z = part.center[1] / 100;
		node.position.copy(node.parent!.worldToLocal(position));
		if (part.control) {
			node.userData.controlId = part.control;
			node.userData.controlEvent = controlAddress(part.control);
		}
	}
	const outline = new THREE.BoxHelper(visual, 0x81e6d9);
	group.add(outline);
	return {
		group,
		setAppearance(state, selected) {
			for (const { material, color, opacity, transparent, depthWrite } of materials) {
				material.color.copy(color);
				const muted = state === "released";
				if (state === "unsupported" || state === "idle") material.color.lerp(new THREE.Color(0x73777d), 0.25);
				material.transparent = transparent || muted;
				material.opacity = opacity * (muted ? 0.48 : 1);
				material.depthWrite = depthWrite && !muted;
				material.wireframe = false;
			}
			outline.visible = selected;
		},
		dispose() {
			for (const geometry of geometries.values()) geometry.dispose();
			for (const material of clonedMaterials.values()) material.dispose();
			for (const texture of textures.values()) texture.dispose();
			outline.geometry.dispose();
			(outline.material as THREE.Material).dispose();
		},
	};
}
