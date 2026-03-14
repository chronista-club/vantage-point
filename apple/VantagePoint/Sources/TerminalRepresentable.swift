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
        // クロームは SwiftUI で描画（vp-bridge クロームは standalone TUI 用に温存）

        let cwd = projectPath ?? NSHomeDirectory()
        let projectName = (cwd as NSString).lastPathComponent
        // tmux セッション名: {project}-vp（SP が作成済み）
        let tmuxSession = projectName.replacingOccurrences(of: ".", with: "-") + "-vp"

        // vp tui を起動（ratatui コンソール → tmux セッション）
        // vp tui が未インストールなら tmux attach に直接フォールバック
        view.deferredPtyCwd = cwd
        let vpBin = "\(NSHomeDirectory())/.cargo/bin/vp"
        view.deferredPtyCommand = "\(vpBin) tui --session \(tmuxSession) 2>/dev/null || /opt/homebrew/bin/tmux attach-session -t \(tmuxSession) 2>/dev/null || exec zsh -l"
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
