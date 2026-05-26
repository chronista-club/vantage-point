/**
 * Frame Engine の Scene 切替 keybinding + VP shortcut directive bridge (main view 側)。
 *
 * 2 つの責務:
 * 1. **Scene hotkey** — VP-140 / PR-ε-1: Ctrl+Shift+1..4 / Ctrl+Shift+] / [ で Frame Engine の
 *    Scene 切替 (`attachKeybindings`)
 * 2. **VP shortcut directive bridge** — 規約 v0.3: `Cmd hold + <key>` を捕捉して
 *    `directive:fire` IPC で Rust に投げる (`installMainViewDirectiveBridge`)。 main view からは
 *    sidebar の `window.vpFilePicker` に直接 access できないため Rust 経由で sidebar に inject。
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
import { installDirectiveHandler } from './src/shortcuts/chord';

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

interface WryIpc {
  postMessage(msg: string): void;
}

declare global {
  interface Window {
    ipc?: WryIpc;
  }
}

/**
 * Main view 側 directive dispatcher (= VP 規約 v0.3 directive、 main view 側 bridge)。
 *
 * `Cmd hold + <key>` keydown を捕捉して、 IPC (`directive:fire`) で Rust に投げる。
 * Rust 側 (`app.rs` の `AppEvent::DirectiveFire` arm) が key ごとに dispatch して
 * `sidebar.evaluate_script` で sidebar API (`window.vpFilePicker.open` 等) を inject する。
 *
 * 戻り値: uninstall 関数。
 */
export function installMainViewDirectiveBridge(): () => void {
  return installDirectiveHandler({
    exec: (key) => {
      const ipc = window.ipc;
      if (!ipc) {
        console.warn('[directive bridge] window.ipc 未注入 — 破棄:', key);
        return;
      }
      ipc.postMessage(JSON.stringify({ t: 'directive:fire', key }));
    },
  });
}
