import { gearId, gearLayout, type GearMetadata } from "./midi-gear";
import type { Group } from "three";

export function createGearCache<T>(load: (id: string) => Promise<T>) {
	const cache = new Map<string, Promise<T | null>>();
	return (profile: string): Promise<T | null> => {
		const id = gearId(profile);
		if (!id) return Promise.resolve(null);
		let result = cache.get(id);
		if (!result) {
			result = Promise.resolve().then(() => load(id)).catch(() => null);
			cache.set(id, result);
		}
		return result;
	};
}
interface Descriptor { sha256: string; bytes: number }
interface Manifest { schema: number; gear: Record<string, { glb: Descriptor; json: Descriptor }> }
export interface GearAsset { root: Group; metadata: GearMetadata }
const descriptor = (value: Descriptor | undefined) => value && /^[a-f0-9]{64}$/.test(value.sha256) && Number.isInteger(value.bytes) && value.bytes > 0 && value.bytes <= 32 * 1024 * 1024;
let manifest: Promise<Manifest> | undefined;
const byHash = new Map<string, Promise<GearAsset>>();
async function read(url: string): Promise<Response> {
	const response = await fetch(url);
	if (!response.ok) throw Error("Gear asset unavailable");
	return response;
}

/** The native protocol verifies SHA-256 and pins both requests to one manifest version. */
export const loadGear = createGearCache(async (id): Promise<GearAsset> => {
	manifest ??= read("vp-asset://app/gear/manifest.json").then((response) => response.json());
	const index = await manifest;
	const entry = index.schema === 1 ? index.gear?.[id] : undefined;
	if (!entry || !descriptor(entry.glb) || !descriptor(entry.json)) throw Error("Missing gear descriptor");
	const key = `${entry.glb.sha256}:${entry.json.sha256}`;
	let asset = byHash.get(key);
	if (!asset) {
		asset = (async () => {
			const metadata = await (await read(`vp-asset://app/gear/${id}.${entry.json.sha256}.json`)).json() as GearMetadata;
			if (!gearLayout(metadata)) throw Error("Invalid gear metadata");
			const bytes = await (await read(`vp-asset://app/gear/${id}.${entry.glb.sha256}.glb`)).arrayBuffer();
			if (bytes.byteLength !== entry.glb.bytes) throw Error("Invalid GLB size");
			// The distributed GLB must be self-contained, never fetch paths embedded in a model.
			const view = new DataView(bytes);
			if (view.getUint32(0, true) !== 0x46546c67 || view.getUint32(4, true) !== 2 || view.getUint32(12, true) + 20 > bytes.byteLength || view.getUint32(16, true) !== 0x4e4f534a) throw Error("Invalid GLB");
			const json = JSON.parse(new TextDecoder().decode(bytes.slice(20, 20 + view.getUint32(12, true))));
			if ([...(json.buffers ?? []), ...(json.images ?? [])].some((item: { uri?: string }) => item.uri !== undefined)) throw Error("External GLB resource");
			const { GLTFLoader } = await import("three/addons/loaders/GLTFLoader.js");
			const gltf = await new GLTFLoader().parseAsync(bytes, "");
			return { root: gltf.scene, metadata };
		})();
		byHash.set(key, asset);
	}
	return asset;
});
