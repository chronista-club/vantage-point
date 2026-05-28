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
import { attachKeybindings, installMainViewDirectiveBridge } from './keybindings'
import { renderPP, clearPP, appendPP } from './pp'
import {
  handleMessage as handleCanvasMessage,
  setActiveLaneName,
  requestPersistedState,
} from './canvas-handler'
import { mountHistoryStrip, HISTORY_STRIP_CSS } from './HistoryStrip'
// pp-content-persist follow-up: PP body の markdown renderer を creoui-md-view に置換。
// esbuild の text loader (build.mjs 設定) で文字列として import し、 head の <style> に注入。
import CREOUI_MD_VIEW_CSS from 'creoui-md-view/styles.css'

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
// VP 規約 v0.3 directive bridge: main view で `Cmd hold + key` を捕捉して
// `directive:fire` IPC 経由で Rust → sidebar に inject する。 main view focus 中
// (terminal / Canvas / cc 入力欄等) でも picker を開けるようにする (= Pane focus 時の Cmd hold f)。
installMainViewDirectiveBridge()

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

/**
 * LaneAddress::Display 形 (`<project>/lead` / `<project>/wing/<name>`) を、
 * canvas-handler が IPC で使う flat lane_name (`null` = lead, `string` = wing) に翻訳する。
 * pp-content-persist で lead/wing 別の SurrealDB record を引くための key 整形。
 */
function laneNameFromAddress(addr: string | null): string | null {
  if (!addr) return null
  if (addr.endsWith('/lead')) return null
  const m = addr.match(/\/wing\/(.+)$/)
  if (m) return m[1] ?? null
  return null
}
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
      // wiremsg Stage 2: canvas (PP body) の供給は Rust 側 spawn_canvas_subscription が
      // per-SP で担うため、Lane 切替時の JS 側 WS 付替は不要 (旧 setWantedLane を撤去)。
      // 保存済 Scene を restore、 初訪問 Lane は lead-focus を default にする
      const target = laneScenes.get(newLane) ?? 'lead-focus'
      frameEngine.applyScene(target)
      // pp-content-persist: lane 切替時に canvas-handler の lane scope を更新 + Rust に load 要求。
      // load 結果は `pp:state:loaded` IPC が vpCanvas.handleMessage に push してくる。
      // LaneAddress::Display 形 (`<project>/lead` or `<project>/wing/<name>`) を flat lane_name に翻訳。
      const laneName = laneNameFromAddress(newLane)
      setActiveLaneName(laneName)
      requestPersistedState(laneName)
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

// wiremsg Stage 2: Rust 注入口。Rust 側 spawn_canvas_subscription が active project の
// canvas ProcessMessage ごとに `window.vpCanvas.handleMessage(msg)` を evaluate_script で呼ぶ。
// DevTools から手動 trigger も可: window.vpCanvas.handleMessage({type:'show',content:{markdown:'# hi'}})
;(window as unknown as { vpCanvas: { handleMessage: typeof handleCanvasMessage } }).vpCanvas = {
  handleMessage: handleCanvasMessage,
}

// doc 19 PP Canvas Stack Model: HistoryStrip CSS を head に注入 + DOMContentLoaded で mount。
// PP pane の DOM (#pp-history-strip) は main_area.rs HTML 側で保証される。
const historyStripStyle = document.createElement('style')
historyStripStyle.textContent = HISTORY_STRIP_CSS
document.head.appendChild(historyStripStyle)

// pp-content-persist follow-up: creoui-md-view base styles を注入。 PP body の markdown を
// CreoMarkdown で render する際の typography / spacing は本 CSS の `--*` token を期待する
// (= fallback で system default に degrade)。
const creouiMdViewStyle = document.createElement('style')
creouiMdViewStyle.textContent = CREOUI_MD_VIEW_CSS
document.head.appendChild(creouiMdViewStyle)

// pp-content-persist follow-up: PP body の typography を **みぞれ / みぞれ 等幅** に固定。
// system install 済の font family を `font-family` 直指定 (= vp-asset:// 経由の font fetch 不要)。
// Mizolet (英字 family 名) も並記して、 system locale に依らず引けるように。
// 配色 / spacing / margin は creoui-md-view の design token に従う (= 上で注入済)。
const ppFontStyle = document.createElement('style')
ppFontStyle.textContent = `
/* pp-content-persist: PP body markdown — みぞれ family を全 text に適用、 code / pre は等幅。
   WebKit で日本語 family 名先頭は resolve 不安定なため、 英字 alias (Mizolet / Mizolet-Mono)
   を先頭に書き、 日本語 fallback / system-ui に degrade させる。 */
#pp-content .creo-md,
#pp-content .creo-md p,
#pp-content .creo-md li,
#pp-content .creo-md blockquote,
#pp-content .creo-md h1,
#pp-content .creo-md h2,
#pp-content .creo-md h3,
#pp-content .creo-md h4,
#pp-content .creo-md h5,
#pp-content .creo-md h6,
#pp-content .creo-md table,
#pp-content .creo-md a {
  font-family: Mizolet, 'みぞれ', system-ui, sans-serif;
}
#pp-content .creo-md code,
#pp-content .creo-md pre,
#pp-content .creo-md .creo-md-inline-code {
  font-family: 'Mizolet-Mono', 'みぞれ 等幅', 'VPMono35', monospace;
}
/* mermaid SVG wrapper の余白 — placeholder 置換後の見栄え */
#pp-content .creo-md-mermaid { margin: 1em 0; }
#pp-content .creo-md-mermaid svg { max-width: 100%; height: auto; }
#pp-content .creo-md-mermaid-error {
  font-family: 'みぞれ 等幅', 'Mizolet-Mono', monospace;
  color: var(--color-text-secondary, #c66);
  background: var(--color-surface-bg-subtle, #1a1a22);
  padding: 8px; border-radius: 4px; white-space: pre-wrap;
}
`
document.head.appendChild(ppFontStyle)

// 起動時 default Scene apply + HistoryStrip mount
const applyDefaultScene = (): void => {
  installSetActivePaneBridge()
  const ok = frameEngine.applyScene('lead-focus')
  const paneCount = document.querySelectorAll('[data-frame-id]').length
  // 診断 log: Frame Engine が apply された事実と、 data-frame-id 要素の存在を確認できるようにする。
  // user 環境で 「画面が黒い」 等の issue 時に DevTools console で path を即時切り分けできるよう常時出力。
  console.info(
    `[frame-engine] applied default scene = lead-focus (ok=${ok}); panes detected = ${paneCount}`,
  )
  // doc 19: PP body 下の history strip を SolidJS で mount。
  mountHistoryStrip()
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
        // doc 19 PP Canvas Stack Model: clear は items + cursor + DOM の 3 つを全 reset
        // する semantic。 `clearPP()` 直叩きだと canvasState (items / cursor) が残り、
        // strip は表示されたまま main だけ空になる非対称が起きる (= team-b review で発覚)。
        // canvas-handler の `handleMessage({type:'clear'})` 経路で stack 含めて全 reset する。
        handleCanvasMessage({ type: 'clear', pane_id: 'main' })
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
