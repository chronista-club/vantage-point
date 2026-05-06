/**
 * Frame Engine の Scene 切替 keybinding。
 *
 * VP-140 / PR-ε-1。 Cmd/Ctrl + Shift + 1..4 で 4 default Scene を即時切替、
 * Cmd/Ctrl + Shift + ] / [ で cyclic next/prev。
 *
 * Scene 自体は entry.tsx で register されている前提 (本 module は engine.applyScene を呼ぶだけ)。
 *
 * Note: Scene id は scenes.ts の DEFAULT_SCENES 順に対応. Hotkey と id を hardcode で結ぶより
 *       DEFAULT_SCENES 配列の order を信頼する方が DRY だが、 「Cmd+Shift+3 = PP Overlay」 と
 *       いう UX 約束を keybinding 側で明示することの方が読みやすい (config 化は将来検討)。
 */

import type { FrameEngine, SceneId } from './frame-engine';

/** Cmd/Ctrl + Shift + N → Scene id mapping. event.key は layout-independent な文字を返す. */
const SCENE_HOTKEYS: Record<string, SceneId> = {
  '1': 'lead-focus',
  '2': 'side-review',
  '3': 'pp-overlay',
  '4': 'pp-focus',
};

/**
 * window 等の EventTarget に keydown listener を attach。
 *
 * @returns unsubscribe 関数
 */
export function attachKeybindings(
  engine: FrameEngine,
  target: EventTarget = window,
): () => void {
  const handler = (event: Event): void => {
    const e = event as KeyboardEvent;
    const isCmdOrCtrl = e.metaKey || e.ctrlKey;
    if (!isCmdOrCtrl || !e.shiftKey) return;

    // 数字 hotkey
    const sceneId = SCENE_HOTKEYS[e.key];
    if (sceneId) {
      if (engine.hasScene(sceneId)) {
        e.preventDefault();
        engine.applyScene(sceneId);
      }
      return;
    }

    // cyclic next/prev
    if (e.key === ']') {
      e.preventDefault();
      engine.cycleScene(1);
    } else if (e.key === '[') {
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
