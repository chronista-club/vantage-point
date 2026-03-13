import AppKit
import SwiftUI
import VPBridge

/// TerminalView (AppKit/Core Text) を SwiftUI で使うためのラッパー
///
/// NSViewRepresentable で TerminalView をホストし、
/// vp-bridge FFI + PTY のライフサイクルを管理する。
/// Liquid Glass の NavigationSplitView detail ペインに配置される。
struct TerminalRepresentable: NSViewRepresentable {
    /// PTY の作業ディレクトリ
    let projectPath: String?
    /// このターミナルがアクティブ（表示中）かどうか
    var isActive: Bool = true

    func makeNSView(context: Context) -> TerminalView {
        let view = TerminalView(frame: .zero)
        // Bridge は作成するが、PTY 起動はレイアウト確定後に遅延
        // makeNSView 時点では bounds が .zero → 1x1 グリッドになりシェルが正常に動けない
        view.setupBridge()
        view.deferredPtyCwd = projectPath ?? NSHomeDirectory()
        return view
    }

    func updateNSView(_ nsView: TerminalView, context: Context) {
        let wasActive = nsView.isActive
        nsView.isActive = isActive

        // アクティブに切り替わった → 即座に再描画（フレームコールバック待ちの間の stale 表示を防ぐ）
        if isActive && !wasActive {
            nsView.needsDisplay = true
        }

        // アクティブなターミナルのみフォーカスを取得
        // ZStack で非表示のビューがフォーカスを奪うのを防ぐ
        guard isActive else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            if let window = nsView.window, window.firstResponder !== nsView {
                window.makeFirstResponder(nsView)
            }
        }
    }

    static func dismantleNSView(_ nsView: TerminalView, coordinator: ()) {
        nsView.stopPty()
    }
}
