import Foundation

// MARK: - VP-83 Phase 2.1 Pane data model migration bridge
//
// 旧 VPPaneNode (horizontal フラグが node 内に mix) と新 PaneNode + LayoutMap の
// 相互変換を提供。consumer (MainWindowView, VPPaneContainer) が段階的に新 model へ
// 移行する間の bridge。

extension PaneNode {
    /// 旧 VPPaneNode から 新 PaneNode + LayoutMap に変換
    static func migrate(from legacy: VPPaneNode) -> (PaneNode, PaneLayoutMap) {
        var layoutMap: PaneLayoutMap = [:]
        let node = migrateNode(legacy, layoutMap: &layoutMap)
        return (node, layoutMap)
    }

    private static func migrateNode(_ legacy: VPPaneNode, layoutMap: inout PaneLayoutMap) -> PaneNode {
        switch legacy {
        case .leaf(let legacyLeaf):
            let kind: PaneKind = PaneKind(rawValue: legacyLeaf.contentType) ?? .agent
            return .leaf(PaneLeaf(
                id: legacyLeaf.id,
                paneSessionName: legacyLeaf.paneSessionName,
                tmuxWindowName: legacyLeaf.tmuxWindowName,
                kind: kind,
                isFocused: legacyLeaf.isFocused
            ))
        case .split(let id, let horizontal, let first, let second):
            let firstNode = migrateNode(first, layoutMap: &layoutMap)
            let secondNode = migrateNode(second, layoutMap: &layoutMap)
            layoutMap[id] = horizontal ? .horizontalSplit : .verticalSplit
            return .group(id: id, children: [firstNode, secondNode])
        }
    }

    /// 新 PaneNode + LayoutMap から 旧 VPPaneNode に逆変換 (consumer 未移行用)
    ///
    /// 旧 model は 2-children split のみ表現可能。
    /// children 数が 2 以外 / overlay / tab は lossy 変換:
    /// - children == 1 → その child を昇格
    /// - children >= 3 → 最初の 2 つのみ split に、3 つ目以降は第二子の下に残り展開
    /// - overlay / tab → horizontalSplit として fallback
    func toLegacy(layoutMap: PaneLayoutMap) -> VPPaneNode {
        switch self {
        case .leaf(let leaf):
            return .leaf(VPPaneLeaf(
                id: leaf.id,
                paneSessionName: leaf.paneSessionName,
                tmuxWindowName: leaf.tmuxWindowName,
                contentType: leaf.kind.rawValue,
                isFocused: leaf.isFocused
            ))
        case .group(let id, let children):
            guard !children.isEmpty else {
                // 空 group は degenerate、空 agent leaf でパッチ
                return .leaf(VPPaneLeaf(
                    id: id,
                    paneSessionName: nil,
                    tmuxWindowName: nil,
                    contentType: PaneKind.agent.rawValue
                ))
            }
            // children 1 つだけなら昇格
            if children.count == 1 {
                return children[0].toLegacy(layoutMap: layoutMap)
            }
            // 2 つちょうど → 直接 split
            let rule = layoutMap[id] ?? .horizontalSplit
            let horizontal = rule.kind != .verticalSplit  // vertical 以外は横扱い (overlay/tab も fallback)
            if children.count == 2 {
                return .split(
                    id: id,
                    horizontal: horizontal,
                    first: children[0].toLegacy(layoutMap: layoutMap),
                    second: children[1].toLegacy(layoutMap: layoutMap)
                )
            }
            // 3 つ以上 → 右結合で split 連鎖
            let firstLegacy = children[0].toLegacy(layoutMap: layoutMap)
            let restChildren = Array(children.dropFirst())
            let rest = PaneNode.group(id: UUID(), children: restChildren)
            return .split(
                id: id,
                horizontal: horizontal,
                first: firstLegacy,
                second: rest.toLegacy(layoutMap: layoutMap)
            )
        }
    }
}

extension PaneLayout {
    /// 旧 VPPaneLayout → 新 PaneLayout
    static func migrate(from legacy: VPPaneLayout) -> PaneLayout {
        let (root, layoutMap) = PaneNode.migrate(from: legacy.root)
        return PaneLayout(
            root: root,
            focusedPaneId: legacy.focusedPaneId,
            layoutMap: layoutMap
        )
    }

    /// 新 PaneLayout → 旧 VPPaneLayout (consumer 未移行用、lossy 可)
    func toLegacy() -> VPPaneLayout {
        VPPaneLayout(
            root: root.toLegacy(layoutMap: layoutMap),
            focusedPaneId: focusedPaneId
        )
    }
}
