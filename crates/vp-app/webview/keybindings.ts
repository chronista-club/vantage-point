/**
 * Frame Engine の Scene 切替 keybinding (main view 側)。
 *
 * **Scene hotkey** — VP-140 / PR-ε-1: Ctrl+Shift+1..4 / Ctrl+Shift+] / [ で Frame Engine の
 * Scene 切替 (`attachKeybindings`)。
 *
 * WebView 統合 (step 3a) 後: 旧 `installMainViewDirectiveBridge` (`Cmd hold + key` を `directive:fire`
 * IPC で Rust に往復させた main-view 専用 bridge) は撤去。統合 DOM では sidebar の in-process directive
 * dispatcher (`src/sidebar/actions/registry.ts`) が同一 window の keydown を直接捕捉して全 directive を
 * 処理するため、Rust 往復は不要 (残すと二重発火)。
 *
 * 修飾子の選択 (Scene hotkey、 案 B、 cross-platform 一貫):
 * - macOS は **Cmd+Shift+3/4** を screenshot に予約しており、 system level で先 hook される
 *   (NSEvent global monitor)。 WebView の keydown には never reach するので Cmd+Shift+ 数字は使えない。
 * - 案 B 採用: Ctrl+Shift+1..4 で全 platform 一貫 + macOS の screenshot と衝突なし。
 *   Cmd 修飾は明示的に reject (`!e.metaKey`) して Mac native shortcut の誤発火も防ぐ。
 *
 * Note: Scene id は scenes.ts の DEFAULT_SCENES 順に対応. Hotkey と id を hardcode で結ぶより
 *       DEFAULT_SCENES 配列の order を信頼する方が DRY だが、 「Ctrl+Shift+3 = PP Overlay」 と
 *       いう UX 約束を keybinding 側で明示することの方が読みやすい (config 化は将来検討)。
 */

import type { FrameEngine, SceneId } from './frame-engine';

/**
 * Ctrl+Shift+N → Scene id mapping。
 *
 * `event.code` (= 物理キー位置、 layout 独立) で照合する。 `event.key` は Shift 押下時に
 * "1" → "!", "3" → "#" 等の symbol に変わるため hotkey 判定に使えない (US/JIS 両方該当)。
 * 物理 1〜4 キーは `Digit1` / `Digit2` / `Digit3` / `Digit4` で安定 match。
 */
const SCENE_HOTKEY_BY_CODE: Record<string, SceneId> = {
  Digit1: 'lead-focus',
  Digit2: 'side-review',
  Digit3: 'pp-overlay',
  Digit4: 'pp-focus',
};

/**
 * window 等の EventTarget に Scene hotkey listener を attach。
 *
 * @returns unsubscribe 関数
 */
export function attachKeybindings(
  engine: FrameEngine,
  target: EventTarget = window,
): () => void {
  const handler = (event: Event): void => {
    const e = event as KeyboardEvent;
    // 案 B: Ctrl+Shift 必須 (Cmd は明示 reject、 macOS Cmd+Shift+3/4 screenshot との誤発火回避)
    if (!e.ctrlKey || e.metaKey || !e.shiftKey) return;

    // 数字 hotkey (`e.code` で物理キー位置を判定、 Shift 押下時の symbol 化を回避)
    const sceneId = SCENE_HOTKEY_BY_CODE[e.code];
    if (sceneId) {
      if (engine.hasScene(sceneId)) {
        e.preventDefault();
        engine.applyScene(sceneId);
      }
      return;
    }

    // cyclic next/prev (`e.code` で BracketRight = `]`、 BracketLeft = `[` の物理位置を判定)
    if (e.code === 'BracketRight') {
      e.preventDefault();
      engine.cycleScene(1);
    } else if (e.code === 'BracketLeft') {
      e.preventDefault();
      engine.cycleScene(-1);
    }
  };

  // capture phase で取って xterm.js 等の inner listener より先に判定する
  target.addEventListener('keydown', handler, true);
  return () => {
    target.removeEventListener('keydown', handler, true);
  };
}

