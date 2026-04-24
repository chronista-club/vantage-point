import Foundation

// MARK: - VP-83 Phase 2 Pane data model
//
// データ (tree) と表示ルール (LayoutMap) を分離する設計。
// - tree: Lead Pane root、children は子 Pane。所属関係のみ。
// - LayoutMap: 各 group をどう見せるか (horizontal/vertical/overlay/tab)、自由に差替可能。
//
// 旧 VPPaneNode (horizontal フラグが node 内に mix) は並存、consumer 移行までの
// bridge は PaneMigration.swift 参照。

/// Pane content の kind (semantic 指定)
enum PaneKind: String, Codable, Equatable {
    /// Heaven's Door 📖 — Claude CLI TUI via tmux
    case agent
    /// Paisley Park 🧭 — Markdown / HTML / 画像
    case canvas
    /// file / image / URL preview (read-only)
    case preview
    /// 素 shell PTY (将来、Phase 3)
    case shell
}

/// Pane の leaf (1 つの content 表示単位に対応)
struct PaneLeaf: Identifiable, Equatable {
    let id: UUID
    /// tmux グループセッション名 (agent/shell 用、nil = ベースセッション)
    let paneSessionName: String?
    /// tmux window 名 (nil = デフォルト)
    let tmuxWindowName: String?
    /// content kind (semantic)
    let kind: PaneKind
    /// focus 状態
    var isFocused: Bool = false
    /// Preview kind 用の URL (file:// or https://)、他 kind では無視
    var previewURL: URL? = nil
}

/// Pane の tree 構造
///
/// Lead Pane が root、children は子 Pane。
/// 表示ルール (split / overlay / tab) は LayoutRule で**分離**、ここは**所属関係のみ**。
///
/// 変更履歴:
/// - VP-83 Phase 2.1: horizontal フラグを node から除去、LayoutRule 側へ移管。
indirect enum PaneNode: Identifiable, Equatable {
    case leaf(PaneLeaf)
    case group(id: UUID, children: [PaneNode])

    var id: UUID {
        switch self {
        case .leaf(let leaf): return leaf.id
        case .group(let id, _): return id
        }
    }

    /// このノード以下のすべての leaf (表示順)
    var leaves: [PaneLeaf] {
        switch self {
        case .leaf(let leaf): return [leaf]
        case .group(_, let children): return children.flatMap { $0.leaves }
        }
    }

    /// leaf の ID 一覧 (表示順)
    var leafIds: [UUID] {
        leaves.map(\.id)
    }

    /// leaf の総数
    var leafCount: Int {
        leaves.count
    }

    /// 指定 ID の leaf を探す (深さ優先)
    func findLeaf(id: UUID) -> PaneLeaf? {
        switch self {
        case .leaf(let leaf):
            return leaf.id == id ? leaf : nil
        case .group(_, let children):
            for child in children {
                if let found = child.findLeaf(id: id) { return found }
            }
            return nil
        }
    }

    /// 指定 leaf に focus を集中 (他 leaf は unfocus)
    func withFocus(on focusedId: UUID) -> PaneNode {
        switch self {
        case .leaf(var leaf):
            leaf.isFocused = (leaf.id == focusedId)
            return .leaf(leaf)
        case .group(let id, let children):
            return .group(id: id, children: children.map { $0.withFocus(on: focusedId) })
        }
    }

    /// 指定 ID の leaf を削除。
    /// group が子 1 つだけになった場合は group を折りたたんで単独 child を返す (degenerate 除去)。
    /// 削除の結果 tree が空になる場合は nil。
    func removing(targetId: UUID) -> PaneNode? {
        switch self {
        case .leaf(let leaf):
            return leaf.id == targetId ? nil : self
        case .group(let id, let children):
            let remaining = children.compactMap { $0.removing(targetId: targetId) }
            if remaining.isEmpty { return nil }
            if remaining.count == 1 { return remaining[0] }
            return .group(id: id, children: remaining)
        }
    }

    /// 指定 group の子として child を append (flat)。
    /// afterId leaf が直下 child にあればその直後に insert、なければ末尾に。
    /// VP-83 Phase 2.4c: 同方向 split 連続時の flat 集約に使用。
    func appendingChild(_ child: PaneNode, toGroup groupId: UUID, after afterId: UUID) -> PaneNode {
        switch self {
        case .leaf:
            return self
        case .group(let gid, let children):
            if gid == groupId {
                var updated = children
                if let idx = updated.firstIndex(where: { $0.id == afterId }) {
                    updated.insert(child, at: idx + 1)
                } else {
                    updated.append(child)
                }
                return .group(id: gid, children: updated)
            }
            return .group(
                id: gid,
                children: children.map {
                    $0.appendingChild(child, toGroup: groupId, after: afterId)
                }
            )
        }
    }

    /// 指定 leaf の**隣に**新 leaf を追加する。
    /// target の leaf を新 group で wrap、children に target と newLeaf を並べる。
    /// 新 group の表示ルール (horizontal/vertical) は呼出し側で LayoutMap に入れる。
    func inserting(newLeaf: PaneLeaf, adjacentTo targetId: UUID, newGroupId: UUID = UUID()) -> PaneNode {
        switch self {
        case .leaf(let leaf) where leaf.id == targetId:
            return .group(id: newGroupId, children: [.leaf(leaf), .leaf(newLeaf)])
        case .leaf:
            return self
        case .group(let id, let children):
            // 再帰: target を持つ child 経路を走査、leaf に到達して wrap する
            return .group(
                id: id,
                children: children.map {
                    $0.inserting(newLeaf: newLeaf, adjacentTo: targetId, newGroupId: newGroupId)
                }
            )
        }
    }
}

// MARK: - 表示ルール

/// group をどう見せるかの種別
enum LayoutKind: String, Codable, Equatable {
    /// children を左右並び (HStack)
    case horizontalSplit
    /// children を上下並び (VStack)
    case verticalSplit
    /// 第一子の上に残りを ZStack で重ね (overlay)
    case overlay
    /// 切替 tab bar で children を順次表示
    case tab
}

/// group に適用する表示ルール
struct LayoutRule: Equatable, Codable {
    let kind: LayoutKind

    static let horizontalSplit = LayoutRule(kind: .horizontalSplit)
    static let verticalSplit = LayoutRule(kind: .verticalSplit)
    static let overlay = LayoutRule(kind: .overlay)
    static let tab = LayoutRule(kind: .tab)
}

/// group id → LayoutRule の map (表示層の責務)
///
/// 未登録の group は既定 `.horizontalSplit` で render する。
typealias PaneLayoutMap = [UUID: LayoutRule]

// MARK: - Lane (project / worker) の Pane state

/// Lane の pane 状態 (新 data model)
///
/// - `root`: Lead Pane が root の不変な所属 tree
/// - `focusedPaneId`: 現在 focus 中の leaf
/// - `layoutMap`: group id → 表示ルール (自由に書き換え可能)
/// - `activeTabIndex`: tab kind の group id → active child index (Phase 2.4b)
struct PaneLayout: Equatable {
    var root: PaneNode
    var focusedPaneId: UUID
    var layoutMap: PaneLayoutMap
    /// Phase 2.4b: tab rule の group で表示中の child index (未登録は 0)
    var activeTabIndex: [UUID: Int] = [:]

    /// Lead Pane 1 つのみ、の初期状態
    static func initial(leadKind: PaneKind = .agent) -> PaneLayout {
        let id = UUID()
        return PaneLayout(
            root: .leaf(PaneLeaf(
                id: id,
                paneSessionName: nil,
                tmuxWindowName: nil,
                kind: leadKind
            )),
            focusedPaneId: id,
            layoutMap: [:],
            activeTabIndex: [:]
        )
    }

    /// group の表示ルールを取得 (未登録なら既定 horizontalSplit)
    func rule(for groupId: UUID) -> LayoutRule {
        layoutMap[groupId] ?? .horizontalSplit
    }

    /// VP-83 refinement 58: spatial focus 移動 (⌃←→↑↓)
    ///
    /// focused leaf の親 group を下から辿り、direction が rule.kind と一致する最初の
    /// group で sibling に移動する。見つからなければ nil (端)。
    func nextLeafId(from fromId: UUID, direction: PaneFocusDirection) -> UUID? {
        guard let path = Self.findPath(to: fromId, in: root) else { return nil }
        // path は root → ... → target leaf の順
        for depth in stride(from: path.count - 1, through: 1, by: -1) {
            let parent = path[depth - 1]
            guard case .group(let gid, let children) = parent else { continue }
            let rule = layoutMap[gid] ?? .horizontalSplit
            let matchesHorizontal = (direction == .left || direction == .right)
                && rule.kind == .horizontalSplit
            let matchesVertical = (direction == .up || direction == .down)
                && rule.kind == .verticalSplit
            guard matchesHorizontal || matchesVertical else { continue }
            let currentChildId = path[depth].id
            guard let idx = children.firstIndex(where: { $0.id == currentChildId }) else { continue }
            let nextIdx: Int?
            switch direction {
            case .left, .up:
                nextIdx = idx > 0 ? idx - 1 : nil
            case .right, .down:
                nextIdx = idx < children.count - 1 ? idx + 1 : nil
            }
            guard let i = nextIdx else { continue }
            return children[i].leaves.first?.id
        }
        return nil
    }

    /// root から leafId に至るノード path を返す (見つからなければ nil)
    private static func findPath(to leafId: UUID, in node: PaneNode) -> [PaneNode]? {
        switch node {
        case .leaf(let leaf):
            return leaf.id == leafId ? [node] : nil
        case .group(_, let children):
            for child in children {
                if let rest = findPath(to: leafId, in: child) {
                    return [node] + rest
                }
            }
            return nil
        }
    }
}

/// pane focus の方向
enum PaneFocusDirection {
    case left, right, up, down
}
