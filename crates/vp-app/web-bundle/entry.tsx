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
