import OSLog
import SwiftUI

private let logger = Logger(subsystem: "tech.anycreative.vp", category: "VPPane")

// MARK: - データモデル
//
// VP-83 Phase 2.1c: 旧 VPPaneNode/VPPaneLeaf/VPPaneLayout は削除済。
// 新 PaneNode / PaneLeaf / PaneLayout (PaneModel.swift) を直接使用。
// 表示ルールは PaneLayoutMap で分離管理。

// MARK: - 退避ペイン (VP-49)

/// 退避（アイコン化）されたペインの情報
struct MinimizedPane: Identifiable, Equatable {
    let id: UUID
    /// 退避前のリーフ情報（復帰時に使う、新 PaneLeaf）
    let leaf: PaneLeaf
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
func cleanupVPPaneTmux(leaf: PaneLeaf) {
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
    let node: PaneNode
    /// 表示ルール (group id → rule)、未登録は既定 horizontalSplit
    let layoutMap: PaneLayoutMap
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
    private func paneNodeView(for node: PaneNode) -> AnyView {
        switch node {
        case .leaf(let leaf):
            return renderLeaf(leaf)
        case .group(let gid, let children):
            let rule = layoutMap[gid] ?? .horizontalSplit
            return renderGroup(id: gid, children: children, rule: rule)
        }
    }

    /// leaf を kind で分岐 render
    private func renderLeaf(_ leaf: PaneLeaf) -> AnyView {
        let isFocused = leaf.isFocused
        // ベース HD ペイン（paneSessionName == nil）は閉じられない
        let canClose = leaf.paneSessionName != nil || leaf.kind != .agent

        switch leaf.kind {
        case .canvas:
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

        case .preview:
            return AnyView(
                PaneHeaderView(
                    leaf: leaf,
                    isFocused: isFocused,
                    onMinimize: { onMinimizePane?(leaf.id) },
                    onClose: canClose ? { onClosePane?(leaf.id) } : nil
                ) {
                    PreviewRepresentable(url: leaf.previewURL)
                        .id("\(leaf.id):preview:\(leaf.previewURL?.absoluteString ?? "empty")")
                }
            )

        case .shell:
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

        case .agent:
            let tmuxCmd: String? = {
                guard let paneSession = leaf.paneSessionName else { return nil }
                return vpPaneTmuxCommand(paneSessionName: paneSession, cwd: projectPath)
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
        }
    }

    /// group を LayoutRule に応じて render。
    /// VP-83 Phase 2.4: overlay / tab を本実装。
    ///  - horizontalSplit / verticalSplit: 2 children は直接 split、3+ は右結合
    ///  - overlay: 第一子を base layer、残りを ZStack で floating overlay
    ///  - tab: tab bar 表示 + 先頭 child 固定表示 (active 切替 state は次 PR)
    private func renderGroup(id: UUID, children: [PaneNode], rule: LayoutRule) -> AnyView {
        guard !children.isEmpty else {
            return AnyView(Color.clear)
        }
        if children.count == 1 {
            return paneNodeView(for: children[0])
        }

        switch rule.kind {
        case .horizontalSplit, .verticalSplit:
            return renderSplit(id: id, children: children, horizontal: rule.kind == .horizontalSplit)
        case .overlay:
            return renderOverlay(id: id, children: children)
        case .tab:
            return renderTab(id: id, children: children)
        }
    }

    private func renderSplit(id: UUID, children: [PaneNode], horizontal: Bool) -> AnyView {
        if children.count == 2 {
            return AnyView(
                VPPaneSplitView(
                    horizontal: horizontal,
                    splitId: id
                ) {
                    paneNodeView(for: children[0])
                } second: {
                    paneNodeView(for: children[1])
                }
                .id(id)
            )
        }
        // 3 つ以上 → 右結合で split 連鎖
        let first = children[0]
        let rest = PaneNode.group(id: UUID(), children: Array(children.dropFirst()))
        return AnyView(
            VPPaneSplitView(
                horizontal: horizontal,
                splitId: id
            ) {
                paneNodeView(for: first)
            } second: {
                paneNodeView(for: rest)
            }
            .id(id)
        )
    }

    /// Overlay — 第一子を base layer、残りを ZStack で前景 overlay。
    /// overlay 層は material 背景 + 余白 + 影で floating card として表現。
    private func renderOverlay(id: UUID, children: [PaneNode]) -> AnyView {
        let base = children[0]
        let overlayChildren = Array(children.dropFirst())
        return AnyView(
            ZStack {
                paneNodeView(for: base)

                if !overlayChildren.isEmpty {
                    VStack(spacing: 6) {
                        ForEach(Array(overlayChildren.enumerated()), id: \.offset) { _, child in
                            paneNodeView(for: child)
                        }
                    }
                    .padding(10)
                    .background(
                        RoundedRectangle(cornerRadius: 10)
                            .fill(.regularMaterial)
                    )
                    .shadow(color: .black.opacity(0.3), radius: 20, y: 8)
                    .padding(.horizontal, 48)
                    .padding(.vertical, 48)
                }
            }
            .id(id)
        )
    }

    /// Tab — tab bar header + content area (Phase 2.4 MVP: 第一子固定表示、
    /// active 切替 state は次 PR で PaneLayout に活性 index を追加して実装)。
    private func renderTab(id: UUID, children: [PaneNode]) -> AnyView {
        return AnyView(
            VStack(spacing: 0) {
                HStack(spacing: 2) {
                    ForEach(Array(children.enumerated()), id: \.offset) { idx, child in
                        let label = "Pane \(idx + 1)"
                        Text(label)
                            .font(.system(size: 11))
                            .padding(.horizontal, 10)
                            .padding(.vertical, 4)
                            .background(
                                idx == 0
                                    ? Color.colorSurfaceBgEmphasis.opacity(0.5)
                                    : Color.clear
                            )
                            .cornerRadius(4)
                            .foregroundStyle(
                                idx == 0
                                    ? Color.colorTextPrimary
                                    : Color.colorTextTertiary
                            )
                    }
                    Spacer()
                }
                .padding(.horizontal, 6)
                .padding(.vertical, 4)
                .background(Color.colorSurfaceBgSubtle.opacity(0.4))

                // MVP: 第一子を content area として常時表示
                paneNodeView(for: children[0])
            }
            .id(id)
        )
    }
}

// MARK: - PaneHeaderView

/// ペインの Stand 情報（ヘッダー表示用）
struct PaneStandInfo {
    let icon: String      // SF Symbol or emoji
    let label: String     // 表示名
    let color: Color      // アクセントカラー

    /// kind から Stand 情報を導出 (表 label は technical、内部は Stand 名)
    static func from(leaf: PaneLeaf) -> PaneStandInfo {
        switch leaf.kind {
        case .canvas, .preview:
            return PaneStandInfo(icon: "safari", label: "Navigator", color: .cyan)
        case .shell:
            return PaneStandInfo(icon: "terminal", label: "Shell", color: .orange)
        case .agent:
            // paneSessionName があれば追加 Agent pane、なければ Lead Agent
            let label = leaf.paneSessionName != nil ? "Agent" : "Lead Agent"
            return PaneStandInfo(icon: "book", label: label, color: .green)
        }
    }
}

/// 統一ペインヘッダー（VP-48）
///
/// 全ペイン種別に共通のヘッダーバーを付与する。
/// Stand アイコン + タイトル + 操作ボタン（退避・閉じる）。
struct PaneHeaderView<Content: View>: View {
    let leaf: PaneLeaf
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
