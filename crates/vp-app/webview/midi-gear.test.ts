// mem_1Cg1no4SEaGkCYaF3VtCDk
import { expect, it } from "vitest";
import { controlAddress, gearLayout, gearId } from "./midi-gear";

it("maps only ControlEvent kind.index addresses without renaming parts", () => {
 expect(controlAddress("knob.0")).toEqual({ type: "knob", index: 0 });
 expect(controlAddress("fader_touch.8")).toEqual({ type: "fader_touch", index: 8 });
 for (const id of ["knob.-1", "knob.256", "strip.0.knob", "knob.01", "decor.0"]) expect(controlAddress(id)).toBeNull();
});
it("normalizes the nano alias and rejects paths", () => {
 expect(gearId("nanokontrol2")).toBe("nanokontrol");
 for (const id of ["../minilab", "x/y", "%2e%2e", ""]) expect(gearId(id)).toBeNull();
});
it("uses metadata millimetres for control positions and dimensions", () => {
 const layout = gearLayout({ size: [400, 200, 50], parts: [
  { name: "encoder_1", kind: "encoder", center: [-100, 30], size: [20, 20, 15], control: "knob.0" },
  { name: "logo", kind: "decor", center: [0, 0], size: [10, 10, 1] },
 ] });
 expect(layout).toMatchObject({ width: 4, depth: 2, height: 0.5 });
 expect(layout?.controls).toEqual([{ id: "knob.0", kind: "knob", x: -1, z: 0.3, width: 0.2, depth: 0.2 }]);
 expect(gearLayout({ size: [0, 2, 3], parts: [] })).toBeNull();
 expect(gearLayout({ size: [4, 2, 3], parts: [{ center: [null, 0] }] })).toBeNull();
});

import * as THREE from "three";
import { createGearModel } from "./midi-gear-scene";
it("preserves node names, tags controls and fits GLB units to metadata", () => {
 const root = new THREE.Group();
 const knob = new THREE.Mesh(new THREE.BoxGeometry(0.02, 0.015, 0.02), new THREE.MeshStandardMaterial());
 knob.name = "encoder_1";
 knob.position.set(-0.2, 0.05, -0.04); // metadata overrides mismatched planar coordinates
 root.add(knob);
 const metadata = { size: [400, 200, 50] as [number, number, number], parts: [{ name: "encoder_1", kind: "knob", center: [-100, 30] as [number, number], size: [20, 20, 15] as [number, number, number], control: "knob.0" }] };
 expect(createGearModel("missing-nodes", new THREE.Group(), metadata)).toBeNull();
 const model = createGearModel("device-1", root, metadata);
 expect(model).not.toBeNull();
 const part = model!.group.getObjectByName("encoder_1")!;
 expect(part.userData.controlId).toBe("knob.0");
 expect(part.userData.controlEvent).toEqual({ type: "knob", index: 0 });
 model!.group.updateMatrixWorld(true);
 expect(part.getWorldPosition(new THREE.Vector3()).x).toBeCloseTo(-1);
 expect(part.getWorldPosition(new THREE.Vector3()).z).toBeCloseTo(0.3);
 expect(part.getWorldPosition(new THREE.Vector3()).y).toBeCloseTo(0.5);
 expect(knob.position.x).toBe(-0.2);
 model!.setAppearance("unsupported", true);
 expect((part as THREE.Mesh).material).not.toBe(knob.material);
 expect(((part as THREE.Mesh).material as THREE.MeshStandardMaterial).wireframe).toBe(false);
 expect(((part as THREE.Mesh).material as THREE.MeshStandardMaterial).opacity).toBe(1);
 model!.dispose();
});
it("shares cloned resources within a view and releases textures without destroying the cache", () => {
 const texture = new THREE.Texture();
 const material = new THREE.MeshStandardMaterial({ map: texture });
 const geometry = new THREE.BoxGeometry();
 const root = new THREE.Group();
 const a = new THREE.Mesh(geometry, material); a.name = "a";
 const b = new THREE.Mesh(geometry, material); b.name = "b";
 root.add(a, b);
 const model = createGearModel("device", root, { size: [100, 100, 100], parts: [] })!;
 const ca = model.group.getObjectByName("a") as THREE.Mesh;
 const cb = model.group.getObjectByName("b") as THREE.Mesh;
 expect(ca.material).toBe(cb.material);
 expect(ca.geometry).toBe(cb.geometry);
 const clonedTexture = (ca.material as THREE.MeshStandardMaterial).map!;
 expect(clonedTexture).not.toBe(texture);
 let disposed = 0;
 let sourceDisposed = false;
 clonedTexture.addEventListener("dispose", () => disposed++);
 texture.addEventListener("dispose", () => sourceDisposed = true);
 model.dispose();
 expect(disposed).toBe(1);
 expect(sourceDisposed).toBe(false);
});
