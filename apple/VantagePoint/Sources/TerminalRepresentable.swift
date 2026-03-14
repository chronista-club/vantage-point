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
    /// クロームヘッダーテキスト（動的更新）
    var headerText: String = ""

    func makeNSView(context: Context) -> TerminalView {
        let view = TerminalView(frame: .zero)
        // Bridge は作成するが、PTY 起動はレイアウト確定後に遅延
        // makeNSView 時点では bounds が .zero → 1x1 グリッドになりシェルが正常に動けない
        view.setupBridge()
        // クローム（ヘッダー1行 + フッター1行）を設定
        // PTY には height - 2 行が通知される
        view.setupChrome(headerRows: 1, footerRows: 1)

        let cwd = projectPath ?? NSHomeDirectory()
        let projectName = (cwd as NSString).lastPathComponent
        // tmux セッション名: {project}-vp（SP が作成済み）
        let tmuxSession = projectName.replacingOccurrences(of: ".", with: "-") + "-vp"

        // ヘッダー/フッターのテキスト設定
        let headerText = "  \(projectName)  │  ~/repos/\(projectName)"
        let footerText = "  ⌘O Canvas │ ⌘↑↓ Project │ ⌘D Split"
        // グレー背景 + 白テキスト (RGBA: 0x333333FF bg, 0xCCCCCCFF fg)
        view.updateChromeText(row: 0, text: headerText, fg: 0xCCCCCCFF, bg: 0x333333FF)

        // フッターは PTY 起動後に行数が確定してから設定（deferred）
        view.deferredFooterText = footerText

        // tmux セッションが存在すれば attach、なければ raw シェル
        view.deferredPtyCwd = cwd
        // tmux attach を試行。失敗時はログインシェルにフォールバック
        // FFI 側で zsh -l -c "command" として実行される
        // .app バンドルから起動すると PATH が最小限のため tmux のフルパスを使用
        view.deferredPtyCommand = "/opt/homebrew/bin/tmux attach-session -t \(tmuxSession) 2>/dev/null || exec zsh -l"
        return view
    }

    func updateNSView(_ nsView: TerminalView, context: Context) {
        let wasActive = nsView.isActive
        nsView.isActive = isActive

        // クロームヘッダーを動的更新（5秒ポーリングで Stand 情報が変わるたびに反映）
        if isActive && !headerText.isEmpty {
            nsView.updateChromeText(row: 0, text: headerText, fg: 0xCCCCCCFF, bg: 0x333333FF)
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
