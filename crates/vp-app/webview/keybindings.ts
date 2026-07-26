/**
 * app scope の Scene 切替 keybinding（main view 側）。
 *
 * **Scene hotkey** — Ctrl+Shift+1..4 / Ctrl+Shift+] / [ で app-panes の preset 切替
 * （doc 49 LE-P4 PR1 で FrameEngine → creo-ui-layout に載せ替え。hotkey 体系は VP-140 から不変）。
 *
 * 修飾子の選択（案 B、cross-platform 一貫）:
 * - macOS は **Cmd+Shift+3/4** を screenshot に予約しており、system level で先 hook される
 *   (NSEvent global monitor)。WebView の keydown には never reach するので Cmd+Shift+数字は使えない。
 * - 案 B 採用: Ctrl+Shift+1..4 で全 platform 一貫 + macOS の screenshot と衝突なし。
 *   Cmd 修飾は明示的に reject (`!e.metaKey`) して Mac native shortcut の誤発火も防ぐ。
 */

import { applyAppScene, cycleAppScene } from "./app-panes";

/**
 * Ctrl+Shift+N → Scene id mapping。
 *
 * `event.code`（= 物理キー位置、layout 独立）で照合する。`event.key` は Shift 押下時に
 * "1" → "!", "3" → "#" 等の symbol に変わるため hotkey 判定に使えない（US/JIS 両方該当）。
 */
// doc 52 §10 wave 0: pp scene（side-review / pp-overlay / pp-focus）退役後、Digit2-4 は残る
// agent focus（runner / Devices / Preview）へ。Digit1 = lane workbench（board も console も chat も
// この中の tiling）。
const SCENE_HOTKEY_BY_CODE: Record<string, string> = {
	Digit1: "lead-focus",
	Digit2: "runner-focus",
	Digit3: "devices-focus",
	Digit4: "preview-focus",
};

/**
 * window 等の EventTarget に Scene hotkey listener を attach。
 *
 * @returns unsubscribe 関数
 */
export function attachKeybindings(target: EventTarget = window): () => void {
	const handler = (event: Event): void => {
		const e = event as KeyboardEvent;
		// 案 B: Ctrl+Shift 必須（Cmd は明示 reject、macOS Cmd+Shift+3/4 screenshot との誤発火回避）
		if (!e.ctrlKey || e.metaKey || !e.shiftKey) return;

		const sceneId = SCENE_HOTKEY_BY_CODE[e.code];
		if (sceneId) {
			e.preventDefault();
			applyAppScene(sceneId);
			return;
		}

		// cyclic next/prev（`e.code` で BracketRight = `]`、BracketLeft = `[` の物理位置を判定）
		if (e.code === "BracketRight") {
			e.preventDefault();
			cycleAppScene(1);
		} else if (e.code === "BracketLeft") {
			e.preventDefault();
			cycleAppScene(-1);
		}
	};

	// capture phase で取って xterm.js 等の inner listener より先に判定する
	target.addEventListener("keydown", handler, true);
	return () => {
		target.removeEventListener("keydown", handler, true);
	};
}
