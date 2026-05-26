/**
 * Sidebar WebView 専用のキーボードショートカット (= VP 規約 v0.3 directive dispatcher、 sidebar 側)。
 *
 * sidebar 上の `Cmd hold + <key>` keydown を `installDirectiveHandler` で捕捉して、 key ごとに
 * sidebar 内 API を direct に呼ぶ (Rust 経由なし)。 同じ directive は main view からも発火可能だが、
 * main view 側は IPC bridge で Rust に投げ、 Rust が sidebar に evaluate_script で inject する
 * (web-bundle/keybindings.ts の main view side を参照)。
 */
import { sidebar } from './store'
import { installDirectiveHandler } from '../shortcuts/chord'

export function installSidebarKeybindings(): void {
  installDirectiveHandler({
    exec: (key) => {
      if (key === 'f') {
        // `f` directive = File Explorer overlay を open + sidebar focus 移動。
        // (sidebar の場合、 既に sidebar focus 中なので「移動」 部分は no-op、 open のみ)
        const address = sidebar.active_lane_address
        if (!address) {
          console.debug('[directive f] active lane なし、 picker open skip')
          return
        }
        window.vpFilePicker?.open(address)
        return
      }
      if (key === 'p') {
        // `p` directive = send current/selected to PP。
        // sidebar 側の責務: File Explorer picker visible 中なら selected file を Canvas に投擲。
        // picker visible でない時は no-op (= 将来 lane-row focus context 等で別挙動を加える余地)。
        if (window.vpFilePicker?.sendSelectedToPP) {
          window.vpFilePicker.sendSelectedToPP()
        } else {
          console.debug('[directive p] no current selection to send (picker not visible)')
        }
        return
      }
      if (key === 'e' || key === 'g' || key === 'h' || key === 'w') {
        // v0.4 規約: e/g/h/w (Stand focus / TheWorld status) は **main view focus 限定**。
        // sidebar 側からの cross-WebView focus shift は cross-WebView focus 復帰 bridge
        // と同じレイヤーの将来課題なので、 v0.4 では no-op + debug log。
        // user が sidebar から main view (Stand) に飛びたい場合は、 まず Cmd hold f で
        // picker 開いて Esc で閉じる (= main view に focus 戻る) → 改めて Cmd hold e/g/h/w
        console.debug(
          `[directive ${key}] sidebar focus 中: v0.4 では main view focus 限定`,
        )
        return
      }
      console.debug('[directive] sidebar 側で未実装:', key)
    },
  })
}
