/**
 * OS clipboard への書き込み（main / sidebar 両 bundle 共有）。
 *
 * `navigator.clipboard` は WebView の permission policy で **silent fail する**ことがあるので、
 * 既存 `copy` IPC（Rust 側 `arboard`、`terminal.rs:627`）を fallback に併用する二段構え
 * （`main_area.rs` の `doCopy` と同じ形）。
 *
 * ⚠️ **この二段は 1 箇所に置くこと。** 片方だけ直すと「Mac では動くが特定の窓で黙って落ちる」
 * という再現しづらい差が生まれる。LaneHeader（main bundle）と ACTIONS（sidebar bundle）は
 * 別 entry だが、同じ source tree なので esbuild が各 bundle に取り込む。
 */
export function copyText(text: string): void {
	const fallback = () => {
		const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } })
			.ipc;
		ipc?.postMessage(JSON.stringify({ t: "copy", d: text }));
	};
	try {
		navigator.clipboard.writeText(text).catch(fallback);
	} catch {
		fallback();
	}
}
