/**
 * vp-app WebView 用 entry point.
 *
 * SolidJS + @chronista-club/creoui-editor-host を bundle して、main WebView の `<div id="editor-root">`
 * に EditorLayer を mount する。
 *
 * 起動: Ctrl+Shift+E で Editor Mode が toggle される (@chronista-club/creoui-editor-host の default keybind)。
 *
 * 主要 features (@chronista-club/creoui-editor-host から継承):
 * - DOM auto-discover: 既知の CSS 変数 (--typography-family-mono など) を自動 bind
 * - DevTools Console REPL: window.creoEditor.slider(...) 等で field 動的追加
 * - URL shareable state: #creo=... で URL 1 本で共有
 * - Cross-tab sync: 同 origin の複数 tab で values 追従
 * - Theme switching: 8 theme (mint-dark/light, sora-*, contrast-*, oldschool-*)
 *
 * Build:
 *   cd crates/vp-app/web-bundle && bun install && bun run build
 *
 * 出力: ../assets/editor-host.bundle.js (vp-app の Rust 側で include_str!)
 */

// VP-140 diagnostic: bundle が parse + execute されたことを最速で confirm する。
// import 後のいかなる runtime error があっても、 この 1 行は console に出る。
console.info('[vp-bundle] booting (VP-140 diagnostic)')
;(window as unknown as { vpBundleStatus?: Record<string, boolean> }).vpBundleStatus = {
  booted: true,
  importsResolved: false,
  vpFrameSet: false,
}
window.addEventListener('error', (e) => {
  console.error('[vp-bundle] window.error', e.message, e.filename, e.lineno, e.error)
})
window.addEventListener('unhandledrejection', (e) => {
  console.error('[vp-bundle] unhandledrejection', e.reason)
})

import { render } from 'solid-js/web'
import { EditorHostProvider, EditorLayer } from '@chronista-club/creoui-editor-host'
import { FrameEngine, type PaneId, type SceneId } from './frame-engine'
import { DEFAULT_SCENES, EMPTY_SCENE, generateAllFocusScenes } from './scenes'
import { attachRenderer } from './renderer'
import { attachKeybindings } from './keybindings'
import { renderPP, clearPP, appendPP } from './pp'
import {
  connectShowWs,
  disconnectShowWs,
  getLanePort,
  getShowSubscriberStatus,
  setWantedLane,
} from './show-subscriber'

console.info('[vp-bundle] imports resolved')
;(window as unknown as { vpBundleStatus?: Record<string, boolean> }).vpBundleStatus!.importsResolved = true

// ===== VP-140 / PR-ε-1: 3D Frame Layout Engine init =====
// EditorLayer mount より前に Pane / Scene を register しておき、 DOMContentLoaded で
// default Scene を apply する。 setActivePane bridge も window に登録 (legacy 互換)。
//
// data-frame-id 規約 (main_area.rs HTML 側で付与):
//   echoes  → pane-terminal      (Echoes Stand = lane terminal host)
//   pp      → pane-paisley-park  (Paisley Park 🧭 / Information Router、 PP body = Smart Canvas surface)
//   ge      → pane-gold-experience (Gold Experience 🌿)
//   hp      → pane-hermit-purple   (Hermit Purple 🍇)
//   preview → pane-preview        (iframe preview)
//   empty   → pane-empty          (no selection)
// 注: 旧 data-pane-id (main_area.rs inline JS が Lane address 等に書き換える native overlay sync 用)
// と attribute を分離。 同名にすると Lane click で legacy 側が hijack して Frame Engine の Scene lookup が
// undefined → HIDDEN_TRANSFORM で pane が見えなくなる回帰を起こすため (VP-141 fix)。
//
// VP-142 cleanup (PR-ε-4): legacy "canvas" pane 削除。 VP-42 era の placeholder だったが、 PR-ε-3 で
// PP body (`pane-paisley-park` 内 `<div id="pp-content">`) が Smart Canvas surface を物理化したため
// vestigial。 doc 13 §10 Q-3 (Smart Canvas 配置) も WebView 主 = PP body で確定済。
const FRAME_PANE_IDS: PaneId[] = ['echoes', 'pp', 'ge', 'hp', 'preview', 'empty']
const FOCUSABLE_PANE_IDS: PaneId[] = ['echoes', 'pp', 'ge', 'hp', 'preview']

const frameEngine = new FrameEngine()
FRAME_PANE_IDS.forEach((id) => frameEngine.registerPane({ id, kind: id }))
DEFAULT_SCENES.forEach((s) => frameEngine.registerScene(s))
frameEngine.registerScene(EMPTY_SCENE)
generateAllFocusScenes(FOCUSABLE_PANE_IDS).forEach((s) => frameEngine.registerScene(s))

// DOM 反映 + keybindings hook
attachRenderer(frameEngine, document)
attachKeybindings(frameEngine, window)

// ===== legacy setActivePane bridge + per-Lane Scene state =====
// 既存 main_area.rs JS が定義する window.setActivePane を wrap して、
// 旧 logic (showLane / preview iframe src 切替 / sendSlotRect) を保ったまま
// Frame Engine に Scene 切替を発火させる。
//
// per-Lane Scene state preservation (VP-141 follow-up):
// - 各 Lane が独立に「最後にいた Scene」 を覚える Map
// - kind=terminal Lane 切替時に旧 Lane の Scene を save、 新 Lane の保存済 Scene (or default lead-focus)
//   を restore する → user が Lane を跨いでも Side Review / PP Overlay 等の layout 選択が記憶される
// - onSceneChange listener で manual Scene 切替 (Cmd+Shift+N) も active Lane の state に反映
// - kind != terminal (PP/GE/HP click 等) は Lane を跨がない fixed-Pane focus、 laneScenes は更新しない
const KIND_TO_PANE: Record<string, PaneId> = {
  terminal: 'echoes',
  paisley_park: 'pp',
  gold_experience: 'ge',
  hermit_purple: 'hp',
  preview: 'preview',
  empty: 'empty',
  // VP-142 cleanup: legacy "canvas" kind 削除 (Smart Canvas surface = PP body に物理化済)
}

interface SetActivePaneInfo {
  kind?: string | null
  pane_id?: string | null
  preview_url?: string | null
}

/** 現 active Lane の address (Lane 跨ぎの save+restore base). null = まだ Lane click していない. */
let activeLaneAddress: string | null = null
/** Lane address → 最後にその Lane が table に乗っていた SceneId. */
const laneScenes = new Map<string, SceneId>()

// onSceneChange で active Lane の Scene state を継続 update。
// bridge 内で applyScene を呼ぶ場合も含めて全 Scene 切替で fire するが、 同じ値を再 set しても
// 害なし (idempotent)、 manual hotkey 切替時にも自然に反映される。
frameEngine.onSceneChange((sceneId) => {
  if (activeLaneAddress && sceneId !== 'empty') {
    laneScenes.set(activeLaneAddress, sceneId)
  }
})

const installSetActivePaneBridge = (): void => {
  const w = window as unknown as {
    setActivePane?: (info: SetActivePaneInfo | null) => void
  }
  const original = w.setActivePane
  w.setActivePane = (info) => {
    // 旧 logic を先に呼ぶ (showLane / preview iframe / sendSlotRect 等)
    if (typeof original === 'function') {
      try {
        original(info)
      } catch (e) {
        console.warn('[frame-engine] legacy setActivePane error', e)
      }
    }
    // Frame Engine に Scene を発火
    if (!info || !info.kind || info.kind === 'empty') {
      frameEngine.applyScene('empty')
      return
    }
    // kind=terminal: Lane 切替判定 + 保存済 Scene の restore + show-subscriber 付替
    if (info.kind === 'terminal' && info.pane_id) {
      const newLane = info.pane_id
      // Lane が変わった場合、 旧 Lane の現 Scene を save (`onSceneChange` でも save される筈だが
      // 二重 set は idempotent、 timing race に対する保険として明示)
      if (activeLaneAddress && activeLaneAddress !== newLane) {
        const currentScene = frameEngine.getCurrentSceneId()
        if (currentScene && currentScene !== 'empty') {
          laneScenes.set(activeLaneAddress, currentScene)
        }
      }
      activeLaneAddress = newLane
      // VP-142 (PR-ε-3): 新 Lane の SP port に show-subscriber を付替。 mcp__show 経由 broadcast を
      // PP body に流すための WS 接続。 setWantedLane は registry に port があれば即 connect、 なければ
      // wanted slot を保持して後で ensureLane wrap が race recovery で auto connect する (startup
      // auto-select で registry 未 populate のタイミング救済)。
      setWantedLane(newLane)
      // 保存済 Scene を restore、 初訪問 Lane は lead-focus を default にする
      const target = laneScenes.get(newLane) ?? 'lead-focus'
      frameEngine.applyScene(target)
      return
    }
    // kind != terminal (PP/GE/HP/canvas/preview click 等): fixed-Pane focus、 Lane state は更新しない
    const paneId = KIND_TO_PANE[info.kind]
    if (!paneId) {
      console.warn('[frame-engine] unknown kind for setActivePane:', info.kind)
      frameEngine.applyScene('empty')
      return
    }
    frameEngine.applyScene(`${paneId}-focus`)
  }
}

// DevTools 検査用 (window.vpLaneScenes で per-Lane state を inspect 可能)
;(window as unknown as { vpLaneScenes: Map<string, SceneId> }).vpLaneScenes = laneScenes

// DevTools 検査用 (VP-142): show-subscriber state を window 経由で inspect / 手動制御可能に。
//   window.vpShow.status()         → { port, readyState, registrySize }
//   window.vpShow.connect(33002)    → 任意 port に接続
//   window.vpShow.disconnect()      → close
;(window as unknown as {
  vpShow: {
    status: typeof getShowSubscriberStatus
    connect: typeof connectShowWs
    disconnect: typeof disconnectShowWs
    getLanePort: typeof getLanePort
  }
}).vpShow = {
  status: getShowSubscriberStatus,
  connect: connectShowWs,
  disconnect: disconnectShowWs,
  getLanePort,
}

// 起動時 default Scene apply
const applyDefaultScene = (): void => {
  installSetActivePaneBridge()
  const ok = frameEngine.applyScene('lead-focus')
  const paneCount = document.querySelectorAll('[data-frame-id]').length
  // 診断 log: Frame Engine が apply された事実と、 data-frame-id 要素の存在を確認できるようにする。
  // user 環境で 「画面が黒い」 等の issue 時に DevTools console で path を即時切り分けできるよう常時出力。
  console.info(
    `[frame-engine] applied default scene = lead-focus (ok=${ok}); panes detected = ${paneCount}`,
  )
}
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', applyDefaultScene, { once: true })
} else {
  applyDefaultScene()
}

// DevTools 検査用 (window.vpFrame.applyScene('side-review') 等で手動 trigger 可能)
;(window as unknown as { vpFrame: FrameEngine }).vpFrame = frameEngine
;(window as unknown as { vpBundleStatus?: Record<string, boolean> }).vpBundleStatus!.vpFrameSet = true
console.info('[vp-bundle] vpFrame attached to window — bundle init complete')

// ===== VP-141 / PR-ε-2: PP markdown render API =====
// window.vpPP で PP body の renderPP / clearPP / appendPP を公開。 PR-ε-3 で /ws/show 経由
// mcp__show が来た時の inject point として使う。 DevTools console から手動 trigger 可能:
//   window.vpPP.renderPP("# Hello\n\n**bold**")
;(window as unknown as { vpPP: { renderPP: typeof renderPP; clearPP: typeof clearPP; appendPP: typeof appendPP } }).vpPP = {
  renderPP,
  clearPP,
  appendPP,
}

// ===== Pane action button delegation =====
// 各 pane の `[data-action]` button を click delegation で hook。 S2 では Clear のみ実装、
// data-target 属性で対象 surface を識別 (`pp` = Paisley Park body)。 将来的に Pin / Lane 切替
// 等を追加する場合も同 delegation で wire 可能。
document.addEventListener(
  'click',
  (event) => {
    const target = event.target as HTMLElement | null
    const btn = target?.closest('[data-action]') as HTMLElement | null
    if (!btn) return
    const action = btn.dataset.action
    const dataTarget = btn.dataset.target
    if (action === 'clear') {
      if (dataTarget === 'pp') {
        clearPP()
      } else {
        console.warn('[vp-bundle] clear: unknown target', dataTarget)
      }
    }
  },
  // bubbling で取る (capture せず) — pane-header 内 button click は default で bubble する
  false,
)

function App() {
  return (
    <EditorHostProvider>
      <EditorLayer />
    </EditorHostProvider>
  )
}

const root = document.getElementById('editor-root')
if (root) {
  render(() => <App />, root)
} else {
  console.warn('[vp-app] #editor-root が見つかりません — EditorLayer mount をスキップ')
}
