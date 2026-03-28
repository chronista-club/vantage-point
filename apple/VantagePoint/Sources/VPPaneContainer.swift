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
    /// Stand 種別（"agent" = TerminalView, "canvas" = CanvasRepresentable, "shell" = 将来用）
    let contentType: String
    /// このペインがフォーカスされているか
    var isFocused: Bool = false
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

    /// 全リーフを収集（表示順）
    var leaves: [VPPaneLeaf] {
        switch self {
        case .leaf(let leaf): return [leaf]
        case .split(_, _, let first, let second):
            return first.leaves + second.leaves
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

    /// 全リーフの isFocused を focusedPaneId に基づいて設定
    func withFocus(on focusedId: UUID) -> VPPaneNode {
        switch self {
        case .leaf(var leaf):
            leaf.isFocused = leaf.id == focusedId
            return .leaf(leaf)
        case .split(let id, let h, let first, let second):
            return .split(id: id, horizontal: h,
                          first: first.withFocus(on: focusedId),
                          second: second.withFocus(on: focusedId))
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
            root: .leaf(VPPaneLeaf(id: id, paneSessionName: nil, tmuxWindowName: nil, contentType: "agent")),
            focusedPaneId: id
        )
    }
}

// MARK: - 退避ペイン (VP-49)

/// 退避（アイコン化）されたペインの情報
struct MinimizedPane: Identifiable, Equatable {
    let id: UUID
    /// 退避前のリーフ情報（復帰時に使う）
    let leaf: VPPaneLeaf
    /// 退避前の分割位置を記録（隣接リーフ ID + 方向）
    let adjacentToId: UUID?
    let horizontal: Bool
    /// Stand 情報（表示用キャッシュ）
    let standInfo: PaneStandInfo

    static func == (lhs: MinimizedPane, rhs: MinimizedPane) -> Bool {
        lhs.id == rhs.id && lhs.leaf == rhs.leaf
    }
}

/// Pane Dock — 退避ペインのアイコンバー (VP-49)
///
/// Canvas 下部に Fixed 配置。退避ペインをアイコンとして表示し、
/// クリックで元の分割位置に復帰する。
struct PaneDock: View {
    let minimizedPanes: [MinimizedPane]
    let onRestore: (MinimizedPane) -> Void

    var body: some View {
        HStack(spacing: 8) {
            ForEach(minimizedPanes) { pane in
                Button {
                    withAnimation(.spring(duration: 0.3)) {
                        onRestore(pane)
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: pane.standInfo.icon)
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(pane.standInfo.color)
                        Text(pane.standInfo.label)
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(
                        RoundedRectangle(cornerRadius: 6)
                            .fill(Color.white.opacity(0.06))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 6)
                            .stroke(pane.standInfo.color.opacity(0.3), lineWidth: 0.5)
                    )
                }
                .buttonStyle(.plain)
                .help("\(pane.standInfo.label) を復帰")
                .transition(.scale.combined(with: .opacity))
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(Color.black.opacity(0.3))
    }
}

// MARK: - tmux コマンド生成

/// VP Pane 用の tmux コマンドを生成
///
/// 追加 HD ペイン用: 独立した tmux セッションを作成して attach する。
/// ベースセッションとは分離され、冪等（既にセッションがあれば再利用）。
func vpPaneTmuxCommand(
    paneSessionName: String,
    cwd: String
) -> String {
    let tmuxBin = "/opt/homebrew/bin/tmux"
    let safeCwd = cwd.replacingOccurrences(of: "'", with: "'\\''")

    // ステータスバー: セッション名 + window:pane ID のみ表示
    let statusFormat = "#S ❯ #I:#P"

    return """
        \(tmuxBin) has-session -t \(paneSessionName) 2>/dev/null || \
        \(tmuxBin) new-session -d -s \(paneSessionName) -c '\(safeCwd)'; \
        \(tmuxBin) set-option -t \(paneSessionName) status on 2>/dev/null; \
        \(tmuxBin) set-option -t \(paneSessionName) status-left '\(statusFormat) ' 2>/dev/null; \
        \(tmuxBin) set-option -t \(paneSessionName) status-right '' 2>/dev/null; \
        exec \(tmuxBin) attach-session -t \(paneSessionName)
        """
}

/// VP Pane の tmux リソースをクリーンアップ
func cleanupVPPaneTmux(leaf: VPPaneLeaf) {
    guard let paneSession = leaf.paneSessionName else { return }
    let tmuxBin = "/opt/homebrew/bin/tmux"

    Task.detached(priority: .utility) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tmuxBin)
        process.arguments = ["kill-session", "-t", paneSession]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
        process.waitUntilExit()

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
    let node: VPPaneNode
    let isActive: Bool
    let splitNavigatorActive: Bool
    let terminalGeneration: Int
    /// SP の HTTP ポート（Canvas 表示用、nil なら未接続）
    let port: UInt16?
    /// ペイン退避コールバック（VP-49: Dock に格納）
    var onMinimizePane: ((UUID) -> Void)?
    /// ペイン削除コールバック
    var onClosePane: ((UUID) -> Void)?

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
            let isFocused = leaf.isFocused
            // ベース HD ペイン（paneSessionName == nil）は閉じられない
            let canClose = leaf.paneSessionName != nil || leaf.contentType != "agent"

            // Canvas ペイン: CanvasRepresentable を表示
            if leaf.contentType == "canvas" || leaf.contentType == "pp" {
                return AnyView(
                    PaneHeaderView(
                        leaf: leaf,
                        isFocused: isFocused,
                        onMinimize: { onMinimizePane?(leaf.id) },
                        onClose: canClose ? { onClosePane?(leaf.id) } : nil
                    ) {
                        CanvasRepresentable(port: port)
                            .id("\(leaf.id):canvas:\(port ?? 0)")
                    }
                )
            }

            // Shell ペイン (The Hand): 素シェルを直接起動（tmux 不要）
            if leaf.contentType == "shell" {
                return AnyView(
                    PaneHeaderView(
                        leaf: leaf,
                        isFocused: isFocused,
                        onMinimize: { onMinimizePane?(leaf.id) },
                        onClose: { onClosePane?(leaf.id) }
                    ) {
                        TerminalRepresentable(
                            projectPath: projectPath,
                            isActive: isActive,
                            isFocused: isFocused,
                            splitNavigatorActive: splitNavigatorActive,
                            tmuxCommand: "exec zsh -l",
                            paneId: leaf.id,
                            sendMouseEvents: false
                        )
                        .id("\(leaf.id):shell")
                    }
                )
            }

            // Agent ペイン (HD): 独立 tmux セッションに attach
            let tmuxCmd: String? = {
                guard let paneSession = leaf.paneSessionName else {
                    return nil
                }
                return vpPaneTmuxCommand(
                    paneSessionName: paneSession,
                    cwd: projectPath
                )
            }()

            return AnyView(
                PaneHeaderView(
                    leaf: leaf,
                    isFocused: isFocused,
                    onMinimize: { onMinimizePane?(leaf.id) },
                    onClose: canClose ? { onClosePane?(leaf.id) } : nil
                ) {
                    TerminalRepresentable(
                        projectPath: projectPath,
                        isActive: isActive,
                        isFocused: isFocused,
                        splitNavigatorActive: splitNavigatorActive,
                        tmuxCommand: tmuxCmd,
                        paneId: leaf.id
                    )
                    .id("\(leaf.id):\(terminalGeneration)")
                }
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

// MARK: - PaneHeaderView

/// ペインの Stand 情報（ヘッダー表示用）
struct PaneStandInfo {
    let icon: String      // SF Symbol or emoji
    let label: String     // 表示名
    let color: Color      // アクセントカラー

    /// contentType から Stand 情報を導出
    static func from(leaf: VPPaneLeaf) -> PaneStandInfo {
        switch leaf.contentType {
        case "canvas", "pp":
            return PaneStandInfo(icon: "safari", label: "Paisley Park", color: .cyan)
        case "shell":
            return PaneStandInfo(icon: "terminal", label: "The Hand", color: .orange)
        default: // "agent", "hd"
            // paneSessionName があれば追加 HD ペイン
            let label = leaf.paneSessionName != nil ? "Heaven's Door" : "Lead-HD"
            return PaneStandInfo(icon: "book", label: label, color: .green)
        }
    }
}

/// 統一ペインヘッダー（VP-48）
///
/// 全ペイン種別に共通のヘッダーバーを付与する。
/// Stand アイコン + タイトル + 操作ボタン（退避・閉じる）。
struct PaneHeaderView<Content: View>: View {
    let leaf: VPPaneLeaf
    let isFocused: Bool
    let onMinimize: (() -> Void)?
    let onClose: (() -> Void)?
    @ViewBuilder let content: Content

    private let headerHeight: CGFloat = 28

    private var standInfo: PaneStandInfo {
        PaneStandInfo.from(leaf: leaf)
    }

    var body: some View {
        VStack(spacing: 0) {
            // ヘッダーバー
            HStack(spacing: 6) {
                // フォーカスインジケーター（アクセントカラーの縦バー）
                if isFocused {
                    RoundedRectangle(cornerRadius: 1)
                        .fill(standInfo.color)
                        .frame(width: 2, height: 14)
                }

                // Stand アイコン + タイトル
                Image(systemName: standInfo.icon)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(isFocused ? standInfo.color : standInfo.color.opacity(0.5))

                Text(standInfo.label)
                    .font(.system(size: 11, weight: isFocused ? .semibold : .medium))
                    .foregroundStyle(isFocused ? .primary : .secondary)
                    .lineLimit(1)

                Spacer()

                // 退避ボタン
                if let onMinimize {
                    headerButton(icon: "minus", action: onMinimize)
                        .help("ペインを退避")
                }

                // 閉じるボタン（ベース HD ペインは削除不可）
                if let onClose {
                    headerButton(icon: "xmark", action: onClose)
                        .help("ペインを閉じる")
                }
            }
            .padding(.horizontal, 8)
            .frame(height: headerHeight)
            .background(
                isFocused
                    ? Color.white.opacity(0.06)
                    : Color.white.opacity(0.03)
            )

            // 区切り線
            Divider().opacity(0.3)

            // ペイン本体
            content
        }
    }

    /// ヘッダー操作ボタン（ミニマルスタイル）
    private func headerButton(icon: String, action: @escaping () -> Void) -> some View {
        HoverButton(icon: icon, action: action)
    }
}

/// ホバー時に不透明度が上がるヘッダーボタン
private struct HoverButton: View {
    let icon: String
    let action: () -> Void
    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 16, height: 16)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(isHovering ? 1.0 : 0.6)
        .onHover { hovering in
            withAnimation(.easeInOut(duration: 0.1)) {
                isHovering = hovering
            }
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
