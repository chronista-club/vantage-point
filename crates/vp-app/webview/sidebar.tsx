/**
 * vp-app sidebar WebView の entry point (v1.0 柱 2)。
 *
 * 旧 `SIDEBAR_HTML` (app.rs 内 ~1325 行の文字列リテラル) を SolidJS + creoui で
 * 作り直すための足場。 PR-1 では shell layout + Solid store + IPC bridge までを用意し、
 * default では旧 sidebar のまま。 `VP_SIDEBAR_V2=1` で本 bundle に切り替わる。
 *
 * Build:
 *   cd crates/vp-app/webview && bun run build
 * 出力: ../assets/sidebar.bundle.js (vp-app の Rust 側で include_bytes!)
 */
import { render } from 'solid-js/web'
import { Shell, SHELL_CSS } from './src/sidebar/Shell'
import { installIpcBridge } from './src/sidebar/ipc'
import { installSidebarKeybindings } from './src/sidebar/keybindings'

console.info('[vp-sidebar] booting (v1.0 柱2 PR-1 scaffold)')

// boot 失敗時の致命例外を Rust ログ (app.kdl.log) に吐く防御診断。
// `{t:"debug"}` は is_main_ipc_tag が main 扱いで routing → terminal::handle_ipc_message が
// `[xterm debug] {msg}` として tracing に流す。document が opaque origin だと console.error が
// 拾いにくく、かつ render 中 throw は無音で sidebar が空になる (WebView 統合 step 3a 回帰の教訓)
// ため、boot を try/catch で囲み例外を IPC でホストに残す。
function bootLog(msg: string): void {
  try {
    window.ipc?.postMessage(JSON.stringify({ t: 'debug', msg: `[sidebar-boot] ${msg}` }))
  } catch (_) {
    /* ipc 未注入なら諦める */
  }
  console.info(`[sidebar-boot] ${msg}`)
}

try {
  // IPC bridge は component mount より前に登録する。
  // Rust は webview build 直後から window.renderSidebarState を呼びうるため。
  installIpcBridge()
  // Sidebar 専用ショートカット (Cmd+F → File Explorer overlay) を登録。
  installSidebarKeybindings()

  // native WebView の context menu (Reload / Inspect / AutoFill) を抑制する。
  // sidebar の右クリックは独自 ContextMenu に一本化する (VP-204 PR-1)。
  document.addEventListener('contextmenu', (e) => e.preventDefault())

  // shell layout CSS を注入 (creoui token は SIDEBAR_HTML_V2 が inline 済)。
  const style = document.createElement('style')
  style.textContent = SHELL_CSS
  document.head.appendChild(style)

  const root = document.getElementById('sidebar-root')
  if (root) {
    render(() => <Shell />, root)
  } else {
    bootLog('#sidebar-root が見つかりません — Shell mount をスキップ')
  }
} catch (e) {
  bootLog(`BOOT FAILED: ${(e as Error).name}: ${(e as Error).message}\n${(e as Error).stack ?? ''}`)
  throw e
}
