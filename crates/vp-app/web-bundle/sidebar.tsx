/**
 * vp-app sidebar WebView の entry point (v1.0 柱 2)。
 *
 * 旧 `SIDEBAR_HTML` (app.rs 内 ~1325 行の文字列リテラル) を SolidJS + creoui で
 * 作り直すための足場。 PR-1 では shell layout + Solid store + IPC bridge までを用意し、
 * default では旧 sidebar のまま。 `VP_SIDEBAR_V2=1` で本 bundle に切り替わる。
 *
 * Build:
 *   cd crates/vp-app/web-bundle && bun run build
 * 出力: ../assets/sidebar.bundle.js (vp-app の Rust 側で include_bytes!)
 */
import { render } from 'solid-js/web'
import { Shell, SHELL_CSS } from './src/sidebar/Shell'
import { installIpcBridge } from './src/sidebar/ipc'

console.info('[vp-sidebar] booting (v1.0 柱2 PR-1 scaffold)')

// IPC bridge は component mount より前に登録する。
// Rust は webview build 直後から window.renderSidebarState を呼びうるため。
installIpcBridge()

// shell layout CSS を注入 (creoui token は SIDEBAR_HTML_V2 が inline 済)。
const style = document.createElement('style')
style.textContent = SHELL_CSS
document.head.appendChild(style)

const root = document.getElementById('sidebar-root')
if (root) {
  render(() => <Shell />, root)
} else {
  console.error('[vp-sidebar] #sidebar-root が見つかりません — Shell mount をスキップ')
}
