import SwiftUI

/// VP macOS menu bar agent。
///
/// doc 25 §4 / doc 26: Apple Platform Architecture (Model D)。 World = Mac daemon が
/// 唯一の権威で、 この agent は Unison/QUIC で繋がる native client。 menu bar に常駐し
/// (LSUIElement)、 Cocoa main run loop を保持することで CoreMIDI hot-plug が自然に効く
/// 「OS と握手する手」になる。 Rust は一切リンクしない (言語境界 = Unison wire)。
///
/// M1 (このマイルストーン): 起動 → daemon (:32000) に Unison 接続 → `serverIdentity` を
/// menu bar に表示するところまで。 pure Swift agent が Rust daemon と QUIC で握れることの実証。
/// M2 で device channel (CoreMIDI 報告) と world subscribe を足す。
@main
struct VantagePointAgentApp: App {
    /// UI 状態の単一の源。 daemon 接続は actor (`DaemonClient`) に委ね、 結果だけを保持する。
    @StateObject private var model = AgentModel()

    var body: some Scene {
        MenuBarExtra {
            AgentMenuView(model: model)
        } label: {
            // 接続状態を menu bar の icon で示す。
            Image(systemName: model.status.symbolName)
        }
        .menuBarExtraStyle(.window)
    }
}
