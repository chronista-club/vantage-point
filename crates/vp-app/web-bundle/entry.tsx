/**
 * vp-app WebView 用 entry point.
 *
 * SolidJS + creo-ui-editor-host を bundle して、main WebView の `<div id="editor-root">`
 * に EditorLayer を mount する。
 *
 * 起動: Ctrl+Shift+E で Editor Mode が toggle される (creo-ui-editor-host の default keybind)。
 *
 * 主要 features (creo-ui-editor-host から継承):
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
import { EditorHostProvider, EditorLayer } from 'creo-ui-editor-host'
import { CreoIcon } from 'creo-ui-icons-web'
import { STAND_ICON, type StandKind } from './icons/stand'
import { FrameEngine, type PaneId } from './frame-engine'
import { DEFAULT_SCENES, EMPTY_SCENE, generateAllFocusScenes } from './scenes'
import { attachRenderer } from './renderer'
import { attachKeybindings } from './keybindings'
import { renderPP, clearPP, appendPP } from './pp'

console.info('[vp-bundle] imports resolved')
;(window as unknown as { vpBundleStatus?: Record<string, boolean> }).vpBundleStatus!.importsResolved = true

// ===== VP-140 / PR-ε-1: 3D Frame Layout Engine init =====
// EditorLayer mount より前に Pane / Scene を register しておき、 DOMContentLoaded で
// default Scene を apply する。 setActivePane bridge も window に登録 (legacy 互換)。
//
// data-pane-id 規約 (main_area.rs HTML 側で付与):
//   echoes  → pane-terminal      (Echoes Stand = lane terminal host)
//   pp      → pane-paisley-park  (Paisley Park 🧭 / Information Router)
//   canvas  → pane-canvas        (汎用 Canvas surface placeholder)
//   ge      → pane-gold-experience (Gold Experience 🌿)
//   hp      → pane-hermit-purple   (Hermit Purple 🍇)
//   preview → pane-preview        (iframe preview)
//   empty   → pane-empty          (no selection)
const FRAME_PANE_IDS: PaneId[] = ['echoes', 'pp', 'canvas', 'ge', 'hp', 'preview', 'empty']
const FOCUSABLE_PANE_IDS: PaneId[] = ['echoes', 'pp', 'canvas', 'ge', 'hp', 'preview']

const frameEngine = new FrameEngine()
FRAME_PANE_IDS.forEach((id) => frameEngine.registerPane({ id, kind: id }))
DEFAULT_SCENES.forEach((s) => frameEngine.registerScene(s))
frameEngine.registerScene(EMPTY_SCENE)
generateAllFocusScenes(FOCUSABLE_PANE_IDS).forEach((s) => frameEngine.registerScene(s))

// DOM 反映 + keybindings hook
attachRenderer(frameEngine, document)
attachKeybindings(frameEngine, window)

// ===== legacy setActivePane bridge =====
// 既存 main_area.rs JS が定義する window.setActivePane を wrap して、
// 旧 logic (showLane / preview iframe src 切替 / sendSlotRect) を保ったまま
// Frame Engine に Scene 切替を発火させる。
const KIND_TO_PANE: Record<string, PaneId> = {
  terminal: 'echoes',
  canvas: 'canvas',
  paisley_park: 'pp',
  gold_experience: 'ge',
  hermit_purple: 'hp',
  preview: 'preview',
  empty: 'empty',
}

interface SetActivePaneInfo {
  kind?: string | null
  pane_id?: string | null
  preview_url?: string | null
}

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
    const paneId = KIND_TO_PANE[info.kind]
    if (!paneId) {
      console.warn('[frame-engine] unknown kind for setActivePane:', info.kind)
      frameEngine.applyScene('empty')
      return
    }
    frameEngine.applyScene(`${paneId}-focus`)
  }
}

// 起動時 default Scene apply
const applyDefaultScene = (): void => {
  installSetActivePaneBridge()
  const ok = frameEngine.applyScene('lead-focus')
  const paneCount = document.querySelectorAll('[data-pane-id]').length
  // 診断 log: Frame Engine が apply された事実と、 data-pane-id 要素の存在を確認できるようにする。
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

// R3-c POC: creo-ui-icons-web → iconify-icon Web Component → WKWebView の経路を E2E 実証する panel。
// 各 Stand を default + active の 2 weight で並べ、 Phosphor 6 weight 切替が WKWebView で render
// されることを目視確認する。 sidebar の Nerd Font を置換するわけではなく、 「SVG icon が動く」事実
// を vp-app 内で確立する debug overlay。 不要になったら削除する。
function IconPocPanel() {
  const stands: StandKind[] = [
    'echoes', // PR-pre2 (VP-118): 旧 heavens_door
    'paisley_park',
    'gold_experience',
    'hermit_purple',
    'whitesnake',
    'theworld',
  ]
  return (
    <div
      style={{
        position: 'fixed',
        bottom: '8px',
        right: '8px',
        padding: '6px 10px',
        background: 'rgba(20, 20, 20, 0.85)',
        'border-radius': '6px',
        'font-size': '20px',
        color: '#cfd8dc',
        'z-index': 99999,
        display: 'flex',
        gap: '10px',
        'align-items': 'center',
        'box-shadow': '0 2px 8px rgba(0,0,0,0.3)',
      }}
      title="R3-c POC: creo-ui-icons-web 動作確認 (Stand × default + active)"
    >
      {stands.map((s) => (
        <span style={{ display: 'inline-flex', gap: '2px' }}>
          <CreoIcon name={STAND_ICON[s].default} size={20} />
          <CreoIcon name={STAND_ICON[s].active} size={20} color="#7eb6ff" />
        </span>
      ))}
    </div>
  )
}

const root = document.getElementById('editor-root')
if (root) {
  render(() => <App />, root)
} else {
  console.warn('[vp-app] #editor-root が見つかりません — EditorLayer mount をスキップ')
}

// POC panel は body 直下に独立 mount (EditorLayer と無関係)。
// R5 dogfood phase 中は常時 ON (Phosphor 6 Stand × default+active = 12 icon を showcase)。
// 不要になったら下記 render() を削除 or `if (localStorage.getItem('vp-icon-poc') === '1')` 等で gate 化。
render(() => <IconPocPanel />, document.body)
