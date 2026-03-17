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

        // シェル引用のエスケープ（シングルクォート内で安全に埋め込む）
        let safeCwd = cwd.replacingOccurrences(of: "'", with: "'\\''")

        // フォールバックチェーン:
        // 1. vp tui: 既存 tmux セッションに ratatui コンソールで接続
        // 2. vp sp start → vp hd start → vp tui: SP + HD を作成してから接続
        // 3. tmux attach: tmux 直接接続（vp がない環境向け）
        // 4. zsh -l -c 'claude || zsh': シェルフォールバック
        view.deferredPtyCwd = cwd
        // passthrough モード: tmux に直接 exec（vp tui の crossterm は Native App PTY 内で動かないため）
        // tmux status off にしてから attach — vp tui のヘッダー/フッターは Native App 側で描画
        let tmuxBin = "/opt/homebrew/bin/tmux"
        view.deferredPtyCommand = "\(tmuxBin) set-option -t \(tmuxSession) status off 2>/dev/null; exec \(tmuxBin) attach-session -t \(tmuxSession)"
        return view
    }

    func updateNSView(_ nsView: TerminalView, context: Context) {
        let wasActive = nsView.isActive
        nsView.isActive = isActive

        // PTY 終了検知 → 自動復旧（クールダウン付き）
        if isActive && nsView.bridgeInitialized
            && !vp_bridge_pty_is_running_session(nsView.sessionId)
            && nsView.lastPtyCwd != nil
        {
            nsView.restartPtyIfNeeded()
        }

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
