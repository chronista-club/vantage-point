/**
 * Sidebar WebView 専用のキーボードショートカット。
 *
 * - **Cmd+F (Mac) / Ctrl+F (他 OS)**: active lane を root として File Explorer overlay
 *   を開く (`window.vpFilePicker.open(address)`)。 input / textarea / contenteditable に
 *   フォーカス時は noop (二重起動 + テキスト find 干渉を回避)。
 *
 * main WebView (terminal / Canvas) は独立した window object を持ち、 ここで attach した
 * keydown listener は影響しない (WebView ごとに JS world が分離)。
 */
import { sidebar } from './store'

export function installSidebarKeybindings(): void {
  window.addEventListener('keydown', (e) => {
    const isMac = navigator.platform.toUpperCase().includes('MAC')
    const mod = isMac ? e.metaKey : e.ctrlKey
    if (!mod || e.shiftKey || e.altKey) return
    if (e.key.toLowerCase() !== 'f') return

    // input / textarea / contenteditable にフォーカス時は preventDefault しない。
    // FileExplorer 内検索 input でテキスト find / 全選択 と被るのを避ける。
    const target = e.target as HTMLElement | null
    if (target) {
      const tag = target.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || target.isContentEditable) return
    }

    const address = sidebar.active_lane_address
    if (!address) {
      console.debug('[sidebar-keys] Cmd+F: active lane なし、 picker open skip')
      return
    }
    e.preventDefault()
    window.vpFilePicker?.open(address)
  })
}
