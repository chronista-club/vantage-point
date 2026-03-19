import OSLog
import SwiftUI

private let logger = Logger(subsystem: "tech.anycreative.vp", category: "VPPane")

// MARK: - データモデル

/// VP Pane のリーフ（1つの TerminalView に対応）
struct VPPaneLeaf: Identifiable, Equatable {
    let id: UUID
    /// tmux のグループセッション名（nil = ベースセッションに直接 attach）
    let paneSessionName: String?
    /// tmux window 名（nil = デフォルト window）
    let tmuxWindowName: String?
}

/// VP Pane のツリー構造（NSView レイヤの分割コンテナ）
///
/// tmux のペイン分割とは独立した、SwiftUI レイヤの分割管理。
/// 各リーフは独立した TerminalView（tmux window に attach）を保持する。
/// split の horizontal は HStack（水平並び = 右に追加）を意味する。
indirect enum VPPaneNode: Identifiable, Equatable {
    case leaf(VPPaneLeaf)
    case split(id: UUID, horizontal: Bool, first: VPPaneNode, second: VPPaneNode)

    var id: UUID {
        switch self {
        case .leaf(let leaf): return leaf.id
        case .split(let id, _, _, _): return id
        }
    }

    /// リーフの数
    var leafCount: Int {
        switch self {
        case .leaf: return 1
        case .split(_, _, let first, let second):
            return first.leafCount + second.leafCount
        }
    }

    /// 全リーフの ID を収集（表示順）
    var leafIds: [UUID] {
        switch self {
        case .leaf(let leaf): return [leaf.id]
        case .split(_, _, let first, let second):
            return first.leafIds + second.leafIds
        }
    }

    /// リーフを検索
    func findLeaf(id: UUID) -> VPPaneLeaf? {
        switch self {
        case .leaf(let leaf):
            return leaf.id == id ? leaf : nil
        case .split(_, _, let first, let second):
            return first.findLeaf(id: id) ?? second.findLeaf(id: id)
        }
    }

    /// ターゲットリーフの隣に新しいリーフを挿入（分割）
    func inserting(newLeaf: VPPaneLeaf, adjacentTo targetId: UUID, horizontal: Bool) -> VPPaneNode {
        switch self {
        case .leaf(let leaf) where leaf.id == targetId:
            // ターゲットを分割: 元のリーフ + 新しいリーフ
            return .split(
                id: UUID(),
                horizontal: horizontal,
                first: self,
                second: .leaf(newLeaf)
            )
        case .leaf:
            return self
        case .split(let id, let h, let first, let second):
            return .split(
                id: id,
                horizontal: h,
                first: first.inserting(newLeaf: newLeaf, adjacentTo: targetId, horizontal: horizontal),
                second: second.inserting(newLeaf: newLeaf, adjacentTo: targetId, horizontal: horizontal)
            )
        }
    }

    /// ターゲットリーフを削除（兄弟ノードが親を置き換え）
    func removing(targetId: UUID) -> VPPaneNode? {
        switch self {
        case .leaf(let leaf):
            return leaf.id == targetId ? nil : self
        case .split(let id, let h, let first, let second):
            let newFirst = first.removing(targetId: targetId)
            let newSecond = second.removing(targetId: targetId)
            // 片方が消えたら兄弟が親を置き換え
            if newFirst == nil { return newSecond }
            if newSecond == nil { return newFirst }
            return .split(id: id, horizontal: h, first: newFirst!, second: newSecond!)
        }
    }
}

// MARK: - レイアウト状態

/// プロジェクトごとの VP Pane レイアウト
struct VPPaneLayout: Equatable {
    var root: VPPaneNode
    var focusedPaneId: UUID

    /// 初期レイアウト（1つのペインのみ）
    static func initial() -> VPPaneLayout {
        let id = UUID()
        return VPPaneLayout(
            root: .leaf(VPPaneLeaf(id: id, paneSessionName: nil, tmuxWindowName: nil)),
            focusedPaneId: id
        )
    }
}

// MARK: - tmux コマンド生成

/// VP Pane 用の tmux コマンドを生成
///
/// 追加ペイン用: ベースセッションに新しい window を作成し、
/// グループセッション経由で独立表示する。
/// 各 VP Pane が異なる tmux window を同時に表示するために、
/// tmux のグループセッション機能を使用する。
func vpPaneTmuxCommand(
    tmuxSession: String,
    paneSessionName: String,
    windowName: String,
    cwd: String
) -> String {
    let tmuxBin = "/opt/homebrew/bin/tmux"
    let safeCwd = cwd.replacingOccurrences(of: "'", with: "'\\''")

    return """
        \(tmuxBin) has-session -t \(tmuxSession) 2>/dev/null || \
        \(tmuxBin) new-session -d -s \(tmuxSession) -c '\(safeCwd)'; \
        \(tmuxBin) set-option -t \(tmuxSession) status on 2>/dev/null; \
        \(tmuxBin) new-window -t \(tmuxSession) -n \(windowName) -c '\(safeCwd)' 2>/dev/null; \
        \(tmuxBin) kill-session -t \(paneSessionName) 2>/dev/null; \
        \(tmuxBin) new-session -d -t \(tmuxSession) -s \(paneSessionName); \
        \(tmuxBin) select-window -t \(paneSessionName):\(windowName); \
        exec \(tmuxBin) attach-session -t \(paneSessionName)
        """
}

/// VP Pane の tmux リソースをクリーンアップ
func cleanupVPPaneTmux(tmuxSession: String, leaf: VPPaneLeaf) {
    guard let paneSession = leaf.paneSessionName else { return }
    let tmuxBin = "/opt/homebrew/bin/tmux"

    Task.detached(priority: .utility) {
        // グループセッション削除
        let killSession = Process()
        killSession.executableURL = URL(fileURLWithPath: tmuxBin)
        killSession.arguments = ["kill-session", "-t", paneSession]
        killSession.standardOutput = FileHandle.nullDevice
        killSession.standardError = FileHandle.nullDevice
        try? killSession.run()
        killSession.waitUntilExit()

        // 対応する window を削除
        if let windowName = leaf.tmuxWindowName {
            let killWindow = Process()
            killWindow.executableURL = URL(fileURLWithPath: tmuxBin)
            killWindow.arguments = ["kill-window", "-t", "\(tmuxSession):\(windowName)"]
            killWindow.standardOutput = FileHandle.nullDevice
            killWindow.standardError = FileHandle.nullDevice
            try? killWindow.run()
            killWindow.waitUntilExit()
        }

        logger.info("VP Pane cleanup: session=\(paneSession)")
    }
}

// MARK: - ビュー

/// VP Pane コンテナ（ツリーを再帰的にレンダリング）
///
/// プロジェクトの detail 領域に配置される。
/// 初期状態は 1 つの TerminalView。Cmd+D で分割を追加。
struct VPPaneContainer: View {
    let projectPath: String
    let tmuxSession: String
    let node: VPPaneNode
    let focusedPaneId: UUID
    let isActive: Bool
    let splitNavigatorActive: Bool
    let terminalGeneration: Int

    var body: some View {
        paneNodeView(for: node)
    }

    /// ツリーを再帰的にレンダリング
    ///
    /// 再帰呼び出しで opaque return type が自己参照するため AnyView で型消去する。
    /// ペインの数は通常 2〜4 個なのでパフォーマンスへの影響は無視できる。
    private func paneNodeView(for node: VPPaneNode) -> AnyView {
        switch node {
        case .leaf(let leaf):
            let isFocused = leaf.id == focusedPaneId
            // 追加ペインはグループセッション経由、初期ペインは直接 attach
            let tmuxCmd: String? = {
                guard let paneSession = leaf.paneSessionName,
                      let windowName = leaf.tmuxWindowName else {
                    return nil
                }
                return vpPaneTmuxCommand(
                    tmuxSession: tmuxSession,
                    paneSessionName: paneSession,
                    windowName: windowName,
                    cwd: projectPath
                )
            }()

            return AnyView(
                TerminalRepresentable(
                    projectPath: projectPath,
                    isActive: isActive,
                    isFocused: isFocused,
                    splitNavigatorActive: splitNavigatorActive,
                    tmuxCommand: tmuxCmd,
                    paneId: leaf.id
                )
                .id("\(leaf.id):\(terminalGeneration)")
            )

        case .split(let splitId, let horizontal, let first, let second):
            return AnyView(
                VPPaneSplitView(
                    horizontal: horizontal,
                    splitId: splitId
                ) {
                    paneNodeView(for: first)
                } second: {
                    paneNodeView(for: second)
                }
                .id(splitId)
            )
        }
    }
}

/// 分割ビュー（2つの子ビュー + ドラッグハンドル）
///
/// 既存の Canvas リサイズハンドルと同じ DragGesture パターンを使用。
/// NSSplitView ではなく SwiftUI DragGesture を採用（動的な追加/削除の自由度のため）。
struct VPPaneSplitView<First: View, Second: View>: View {
    let horizontal: Bool
    let splitId: UUID
    @ViewBuilder let first: First
    @ViewBuilder let second: Second

    /// 分割比率（0.0〜1.0）— ビュー内で @State 管理
    @State private var ratio: CGFloat = 0.5
    /// ドラッグ開始時の比率を記憶（累積 translation からの正確な計算用）
    @State private var dragStartRatio: CGFloat?

    /// ハンドル幅（ピクセル）
    private let handleSize: CGFloat = 6
    /// 比率の最小/最大
    private let minRatio: CGFloat = 0.1
    private let maxRatio: CGFloat = 0.9

    var body: some View {
        GeometryReader { geometry in
            let totalSize = horizontal ? geometry.size.width : geometry.size.height
            let available = totalSize - handleSize
            let firstSize = max(50, available * ratio)
            let secondSize = max(50, available - firstSize)

            if horizontal {
                HStack(spacing: 0) {
                    first.frame(width: firstSize)
                    dragHandle(totalSize: totalSize)
                    second.frame(width: secondSize)
                }
            } else {
                VStack(spacing: 0) {
                    first.frame(height: firstSize)
                    dragHandle(totalSize: totalSize)
                    second.frame(height: secondSize)
                }
            }
        }
    }

    /// ドラッグハンドル（分割線）
    private func dragHandle(totalSize: CGFloat) -> some View {
        Rectangle()
            .fill(Color.gray.opacity(0.01))
            .frame(
                width: horizontal ? handleSize : nil,
                height: horizontal ? nil : handleSize
            )
            .contentShape(Rectangle())
            .onHover { hovering in
                if hovering {
                    (horizontal ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown).push()
                } else {
                    NSCursor.pop()
                }
            }
            .gesture(
                DragGesture()
                    .onChanged { value in
                        let startRatio = dragStartRatio ?? ratio
                        if dragStartRatio == nil {
                            dragStartRatio = ratio
                        }
                        let delta = horizontal ? value.translation.width : value.translation.height
                        let available = totalSize - handleSize
                        guard available > 0 else { return }
                        let newRatio = startRatio + delta / available
                        ratio = max(minRatio, min(maxRatio, newRatio))
                    }
                    .onEnded { _ in
                        dragStartRatio = nil
                    }
            )
    }
}
