/**
 * Frame Engine の Scene change を DOM に apply する renderer。
 *
 * VP-140 / PR-ε-1。 純 action layer ─ engine.onSceneChange の listener として登録され、
 * `<div class="pane" data-pane-id="...">` element に対して left/top/width/height/opacity/z-index
 * を inline style で書き込む。
 *
 * CSS transform scale ではなく left/top/width/height を採用した理由:
 * - xterm.js は scale された container 内では blurry に render される (DOM resolution と GPU
 *   compositing が一致しない)。 width/height で実 layout box を変える方が xterm.js fitAddon と
 *   親和的。
 * - top/left/width/height は CSS transition で滑らかに interpolate される (GPU-accelerated では
 *   ないが S1 では十分)。
 *
 * Legacy bridge (.active class):
 * - 既存 main_area HTML は `.pane.active` に対して display:block を効かせていた。 Frame Engine 化
 *   後は opacity が visibility を司るため `.active` は表示制御から離れたが、 `sendSlotRect()` 等の
 *   既存 JS が `.pane.active` selector を使っているため、 renderer が「primary pane (z 最大かつ
 *   opacity > 0 の pane)」 に `.active` class を付ける compat layer を残す。
 */

import type { FrameEngine, PaneId, PaneTransform, Scene } from './frame-engine';
import { HIDDEN_TRANSFORM } from './frame-engine';

/**
 * engine.onSceneChange を hook して DOM 反映を行う renderer を attach。
 *
 * @returns unsubscribe 関数
 */
export function attachRenderer(
  engine: FrameEngine,
  root: ParentNode = document,
): () => void {
  const apply = (_id: string, scene: Scene): void => {
    const elements = root.querySelectorAll<HTMLElement>('.pane[data-pane-id]');
    let primaryEl: HTMLElement | null = null;
    let primaryZ = -Infinity;

    elements.forEach((el) => {
      const paneId = el.dataset.paneId as PaneId | undefined;
      if (!paneId) return;
      const t = scene.panes[paneId] ?? HIDDEN_TRANSFORM;
      writePaneStyle(el, t);
      // primary pane (= legacy .active) を決める: visible で z 最大の pane
      if (t.state !== 'hidden' && t.opacity > 0 && t.z >= primaryZ) {
        primaryEl = el;
        primaryZ = t.z;
      }
    });

    // 旧 .active class を primary pane に付け替え (sendSlotRect 等の legacy compat)
    elements.forEach((el) => {
      el.classList.toggle('active', el === primaryEl);
    });
  };

  return engine.onSceneChange(apply);
}

/** PaneTransform を element に inline style として書き込む. */
function writePaneStyle(el: HTMLElement, t: PaneTransform): void {
  el.style.left = `${t.x * 100}%`;
  el.style.top = `${t.y * 100}%`;
  el.style.width = `${t.w * 100}%`;
  el.style.height = `${t.h * 100}%`;
  el.style.zIndex = String(t.z);
  el.style.opacity = String(t.opacity);
  el.classList.toggle('hidden', t.state === 'hidden');
  el.classList.toggle('maximized', t.state === 'maximized');
  el.classList.toggle('minimized', t.state === 'minimized');
}
