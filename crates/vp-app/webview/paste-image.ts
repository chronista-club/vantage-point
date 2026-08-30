/**
 * chat 入力欄への画像貼り付け（2026-08-30）。clipboard / drop から画像を拾って base64 化する。
 *
 * ## なぜ PNG を優先するか
 *
 * スクショアプリ（Shottr / macOS 標準）は**同じ画像を複数形式で**clipboard に載せる。
 * 実測（Shottr、2026-08-30）: PNG 75KB / AVIF 28KB / JPEG 51KB / **TIFF 1.5MB** / BMP 1.5MB …
 * の 9 フレーバー。素直に先頭を取ると TIFF を掴んで **20 倍**のデータを IPC に流しうる。
 *
 * ⚠️ ただし **WebKit は `items` に出す前に 1 つへ絞る**（実測 2026-08-30: 9 フレーバーの
 * clipboard でも `items.length === 1`）。つまり貼り付け経路では下の優先ロジックは
 * 実質素通り。**削らないのは** ①ブラウザ層の絞り込みは環境 / version 依存 ②drop 経路
 * （Finder から複数ファイル）では実際に複数来る、の 2 つの理由から。
 *
 * ## VP は保存しない
 *
 * 拾った画像は送信時に engine へ渡すだけで、transcript / replay には残さない（mako 裁定）。
 * だから file 名も保存先も要らず、必要なのは media_type + base64 だけ。
 */

/** 添付 1 枚。`previewUrl` は表示用の objectURL（捨てる時に revoke が要る）。 */
export type PastedImage = {
	id: number;
	mediaType: string;
	/** base64（data URL の prefix は含まない）。engine へはこれを渡す。 */
	dataBase64: string;
	previewUrl: string;
	/** 元のバイト数（上限判定と表示用）。 */
	bytes: number;
};

/** 1 枚あたりの上限 (bytes)。超える画像は落とす（engine 側の上限と IPC 負荷の両方を見た値）。 */
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

/** 拾う画像形式の優先順。左ほど優先（サイズと互換性の兼ね合い）。 */
const PREFERRED_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"];

let nextId = 1;

/**
 * DataTransfer から画像 file を拾う（純粋 = テスト可能）。
 *
 * 同一の貼り付けに複数形式が入っている場合は **1 枚だけ**選ぶ（同じ絵が 9 枚添付されないため）。
 * 判定は「item が 1 つでも `PREFERRED_TYPES` を含むなら、その中で最も優先度の高い type だけを取る」。
 * どれも該当しなければ `image/*` の先頭を取る（未知形式でも engine が読めることはある）。
 */
export function pickImageFiles(dt: DataTransfer | null): File[] {
	if (!dt) return [];
	const files: File[] = [];
	const seen: string[] = []; // 診断用（実機で何が来るかは環境依存 — 推測しないで見る）
	for (const item of Array.from(dt.items ?? [])) {
		seen.push(`${item.kind}:${item.type}`);
		if (item.kind !== "file") continue;
		const f = item.getAsFile();
		if (f && f.type.startsWith("image/")) files.push(f);
	}
	if (files.length === 0 && seen.length > 0) {
		// 画像を拾えなかった時だけ出す（通常時は黙る）。「clipboard に PNG はあるのに
		// items には出ない」等の環境差を、推測でなくログで切り分けるため。
		console.info("[chat] 画像なしの貼り付け:", seen.join(", "));
	}
	if (files.length <= 1) return files;

	// 同じ絵の多形式か、別々の画像かを type の重複で見分ける。
	// 全部が「1 形式 1 枚」なら別々の画像とみなして全部返す。
	const types = new Set(files.map((f) => f.type));
	const looksLikeMultiFlavor = types.size === files.length && files.length > 1;
	if (!looksLikeMultiFlavor) return files;

	for (const want of PREFERRED_TYPES) {
		const hit = files.find((f) => f.type === want);
		if (hit) return [hit];
	}
	return [files[0]];
}

/**
 * File を base64 化して添付 1 枚にする。上限超過 / 読めない場合は `null`。
 *
 * 失敗を投げずに `null` に倒すのは、**1 枚の失敗で投入全体を止めない**ため
 * （text は届けたい）。呼び手が filter する。
 */
export async function readImageFile(file: File): Promise<PastedImage | null> {
	if (file.size > MAX_IMAGE_BYTES) {
		console.warn(
			`[chat] 画像が大きすぎるので添付しない: ${file.size} bytes > ${MAX_IMAGE_BYTES}`,
		);
		return null;
	}
	try {
		const buf = await file.arrayBuffer();
		return {
			id: nextId++,
			mediaType: file.type,
			dataBase64: bytesToBase64(new Uint8Array(buf)),
			previewUrl: URL.createObjectURL(file),
			bytes: file.size,
		};
	} catch (e) {
		console.warn("[chat] 画像の読み取りに失敗したので添付しない", e);
		return null;
	}
}

/**
 * Uint8Array → base64（純粋 = テスト可能）。
 *
 * ⚠️ `btoa(String.fromCharCode(...bytes))` の一撃は**スタックを溢れさせる**
 * （数 MB の画像で引数が数百万個になる）。chunk に割って積む。
 */
export function bytesToBase64(bytes: Uint8Array): string {
	const CHUNK = 0x8000;
	let binary = "";
	for (let i = 0; i < bytes.length; i += CHUNK) {
		binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
	}
	return btoa(binary);
}

/** IPC に載せる形（Rust `parse_image_inputs` が読む）。 */
export function toWirePayload(
	images: PastedImage[],
): Array<{ media_type: string; data: string }> {
	return images.map((a) => ({ media_type: a.mediaType, data: a.dataBase64 }));
}
