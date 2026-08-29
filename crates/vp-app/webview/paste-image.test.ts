/** 画像貼り付けの純関数（`pickImageFiles` / `bytesToBase64` / `toWirePayload`）。 */
import { describe, expect, it } from "vitest";
import {
	bytesToBase64,
	pickImageFiles,
	type PastedImage,
	toWirePayload,
} from "./paste-image";

/** DataTransfer の最小 stub（items だけ見る実装なので、それだけ模す）。 */
function dt(files: Array<{ type: string; name?: string }>): DataTransfer {
	const items = files.map((f) => ({
		kind: "file" as const,
		type: f.type,
		getAsFile: () => new File([new Uint8Array([1, 2, 3])], f.name ?? "x", { type: f.type }),
	}));
	return { items } as unknown as DataTransfer;
}

describe("pickImageFiles", () => {
	it("null / 画像なしは空", () => {
		expect(pickImageFiles(null)).toEqual([]);
		expect(pickImageFiles(dt([{ type: "text/plain" }]))).toEqual([]);
	});

	it("1 枚ならそのまま", () => {
		const got = pickImageFiles(dt([{ type: "image/png" }]));
		expect(got.map((f) => f.type)).toEqual(["image/png"]);
	});

	it("⚠️ 同一画像の多形式（Shottr 実測）は PNG だけを 1 枚選ぶ", () => {
		// 実測 2026-08-30: PNG 75KB / AVIF 28KB / JPEG 51KB / TIFF 1.5MB … の 9 フレーバー。
		// 全部拾うと同じ絵が 9 枚添付され、TIFF を掴むと 20 倍のデータが IPC に乗る。
		const got = pickImageFiles(
			dt([
				{ type: "image/tiff" },
				{ type: "image/avif" },
				{ type: "image/png" },
				{ type: "image/jpeg" },
				{ type: "image/bmp" },
			]),
		);
		expect(got.map((f) => f.type)).toEqual(["image/png"]);
	});

	it("PNG が無ければ優先順の次（JPEG）を選ぶ", () => {
		const got = pickImageFiles(dt([{ type: "image/tiff" }, { type: "image/jpeg" }]));
		expect(got.map((f) => f.type)).toEqual(["image/jpeg"]);
	});

	it("優先リストに無い形式だけなら先頭に倒す（未知でも engine が読めることがある）", () => {
		const got = pickImageFiles(dt([{ type: "image/tiff" }, { type: "image/bmp" }]));
		expect(got).toHaveLength(1);
		expect(got[0].type).toBe("image/tiff");
	});

	it("同じ type が複数 = 別々の画像なので全部返す", () => {
		// 「type が全部違う」= 同一画像の多形式、という判別なので、重複があれば別画像扱い。
		const got = pickImageFiles(dt([{ type: "image/png" }, { type: "image/png" }]));
		expect(got).toHaveLength(2);
	});
});

describe("bytesToBase64", () => {
	it("既知の値を base64 にする", () => {
		expect(bytesToBase64(new Uint8Array([104, 105]))).toBe("aGk=");
		expect(bytesToBase64(new Uint8Array([]))).toBe("");
	});

	it("⚠️ chunk 境界を越えてもスタックを溢れさせず正しい（0x8000 の倍数付近）", () => {
		// `btoa(String.fromCharCode(...bytes))` の一撃は数 MB で引数が数百万個になり落ちる。
		const n = 0x8000 * 2 + 5;
		const bytes = new Uint8Array(n).fill(65); // 'A'
		const got = bytesToBase64(bytes);
		expect(got).toBe(btoa("A".repeat(n)));
	});
});

describe("toWirePayload", () => {
	it("Rust の parse_image_inputs が読む形（media_type / data）に写す", () => {
		const imgs: PastedImage[] = [
			{
				id: 1,
				mediaType: "image/png",
				dataBase64: "aGk=",
				previewUrl: "blob:x",
				bytes: 2,
			},
		];
		expect(toWirePayload(imgs)).toEqual([{ media_type: "image/png", data: "aGk=" }]);
	});
});
