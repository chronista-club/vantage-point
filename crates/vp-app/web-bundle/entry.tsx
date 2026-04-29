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

import { render } from 'solid-js/web'
import { EditorHostProvider, EditorLayer } from 'creo-ui-editor-host'
import { CreoIcon } from 'creo-ui-icons-web'
import { STAND_ICON, type StandKind } from './icons/stand'

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
    'heavens_door',
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
