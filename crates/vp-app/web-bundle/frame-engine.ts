/**
 * 3D Frame Layout Engine — Pane を Scene-driven な portable object として扱う layout engine。
 *
 * VP-140 / PR-ε-1。 doc 13 §7 の "Surface 群" を「独立 portable object として scene 配置」 する
 * inversion を実現する core。 LSCM 公理 A1 (portable entity) / A6 (share nothing) と同型構造を
 * UI 層に持ち込む。
 *
 * 設計:
 * - 純 data: PaneTransform / Scene
 * - 純 calculation: applyScene / movePane / cycleScene (state 遷移)
 * - 純 action: subscribe で listener 通知 (DOM 反映は外部 renderer.ts が担う)
 *
 * Pane は分割せず、 frame 全体に対する transform (x/y/w/h は 0..1 比率、 z は layer) で配置される。
 * Scene = Pane 群の transform snapshot + narrative description。
 */

export type PaneId = string;
export type SceneId = string;

export type PaneState = 'normal' | 'minimized' | 'maximized' | 'hidden';

/** Pane 1 つの frame 内 transform (純 data). */
export interface PaneTransform {
  x: number;        // 0..1, frame 比率 (左端基準)
  y: number;        // 0..1, frame 比率 (上端基準)
  w: number;        // 0..1, frame 比率 (幅)
  h: number;        // 0..1, frame 比率 (高さ)
  z: number;        // integer, 大きいほど前面
  opacity: number;  // 0..1
  state: PaneState;
}

/** Pane の identity (scene 跨ぎで保持される). */
export interface PaneNode {
  id: PaneId;
  kind: string;          // 'echoes' | 'pp' | 'ge' | 'hp' | 'preview' | 'canvas' | 'empty'
  laneAddress?: string;  // PR-ε-2 以降で Lane bind するときに使う
  surface?: string;      // PR-ε-2 以降で 'canvas' | 'inline' | 'modal' 等を入れる
}

/** Scene = Pane 群の transform snapshot + narrative description. */
export interface Scene {
  id: SceneId;
  name: string;
  panes: Record<PaneId, PaneTransform>;
  description?: string;
}

export type SceneListener = (sceneId: SceneId, scene: Scene) => void;

/** Pane が Scene に登録されていない時の default = 完全 hidden. */
export const HIDDEN_TRANSFORM: Readonly<PaneTransform> = Object.freeze({
  x: 0, y: 0, w: 1, h: 1, z: 0, opacity: 0, state: 'hidden',
});

/**
 * Pane / Scene を登録し、 applyScene で current Scene を切替える engine。
 *
 * DOM への反映は engine 自身では行わず、 onSceneChange listener (renderer.ts) に委譲する。
 * これにより engine = pure data + calculation、 DOM 反映 = 別 module の action として完全分離。
 */
export class FrameEngine {
  private nodes = new Map<PaneId, PaneNode>();
  private scenes = new Map<SceneId, Scene>();
  private currentSceneId: SceneId | null = null;
  private listeners = new Set<SceneListener>();

  registerPane(node: PaneNode): void {
    this.nodes.set(node.id, node);
  }

  registerScene(scene: Scene): void {
    this.scenes.set(scene.id, scene);
  }

  hasScene(id: SceneId): boolean {
    return this.scenes.has(id);
  }

  getScene(id: SceneId): Scene | undefined {
    return this.scenes.get(id);
  }

  getCurrentSceneId(): SceneId | null {
    return this.currentSceneId;
  }

  listScenes(): SceneId[] {
    return Array.from(this.scenes.keys());
  }

  listPanes(): PaneId[] {
    return Array.from(this.nodes.keys());
  }

  /** Scene を current として apply (= listener 通知). 未知 id なら false. */
  applyScene(id: SceneId): boolean {
    const scene = this.scenes.get(id);
    if (!scene) {
      console.warn(`[frame-engine] unknown scene: ${id}`);
      return false;
    }
    this.currentSceneId = id;
    this.notify(id, scene);
    return true;
  }

  /** 登録順に基づく cyclic 切替 (direction = 1 で next、 -1 で prev). */
  cycleScene(direction: 1 | -1): boolean {
    const ids = this.listScenes();
    if (ids.length === 0) return false;
    const current = this.currentSceneId;
    const baseIdx = current ? ids.indexOf(current) : -1;
    const nextIdx = (baseIdx + direction + ids.length) % ids.length;
    return this.applyScene(ids[nextIdx]!);
  }

  /**
   * 現在 Scene 内の Pane 1 つを runtime patch.
   *
   * 注: Scene 構造を直接 mutate するため、 同 Scene を再 apply した時には patch が残る。
   * Scene を「preset」 として残したい場合は事前に複製してから渡すこと。
   */
  movePane(paneId: PaneId, patch: Partial<PaneTransform>): void {
    if (!this.currentSceneId) return;
    const scene = this.scenes.get(this.currentSceneId);
    if (!scene) return;
    const current = scene.panes[paneId];
    if (!current) {
      console.warn(`[frame-engine] movePane: pane ${paneId} not in scene ${this.currentSceneId}`);
      return;
    }
    scene.panes[paneId] = { ...current, ...patch };
    this.notify(this.currentSceneId, scene);
  }

  /** Scene change listener を登録、 unsubscribe 関数を返す. */
  onSceneChange(listener: SceneListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private notify(id: SceneId, scene: Scene): void {
    for (const listener of this.listeners) {
      try {
        listener(id, scene);
      } catch (e) {
        console.error('[frame-engine] listener error', e);
      }
    }
  }
}
