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
        view.setupBridge()
        let cwd = projectPath ?? NSHomeDirectory()
        view.startPty(cwd: cwd)
        return view
    }

    func updateNSView(_ nsView: TerminalView, context: Context) {
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
