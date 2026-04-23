# Viewport Semantic Split (VP-83 Phase 2)

> **Status**: Draft (2026-04-24)
> **Previous**: Phase 1 Sidebar Disclosure (refinement 1-55 完了、main `dad21dd` / `fdf4a42` / `8981109`)

## 設計原則

**データと表示ルールを分離する**。

- **データモデル**: Lead Pane が root、他は全部 **子 Pane**。tree 構造のみを表す
- **表示ルール**: tree をどう見せるかの policy。split / overlay / tab / … いずれも表示層の責務
- **見せ方は自由**: 同じ tree を render 切替で horizontal split → overlay → tab に瞬時に変更可能

このレイヤリングで「木は不変だが見せ方は変化」を実現。将来 rule 追加しても data model は touched しない。

## データモデル

### Pane tree — Lead が root

```
Lead Pane (HD agent lead、project ごとに 1 つ、root)
├── Child Pane (any kind)
├── Child Pane (any kind)
└── Child Pane (any kind)
```

`indirect enum PaneNode`:
```swift
indirect enum PaneNode: Identifiable, Equatable {
    case leaf(PaneLeaf)
    case group(id: UUID, children: [PaneNode])
}
```

**group は表示ルールを持たない** — 単に children をまとめるだけ。

### Pane kind (contentType の semantic 拡張)

| kind | Stand metaphor | content |
|------|----------------|---------|
| `agent` | Heaven's Door 📖 | Claude CLI TUI (tmux / TerminalRepresentable) |
| `canvas` | Paisley Park 🧭 | Markdown / HTML / 画像 (CanvasRepresentable) |
| `preview` | Paisley Park 拡張 | file / image / URL preview |
| `shell` | 将来 | 素 zsh / fish PTY |

### 制約

- **Lead Pane** (root の leaf) は kind=`agent`、常に存在、削除不可
- Child Pane は任意の kind、順序あり、add/remove/reorder 自由
- tree 深さは理論上無制限、UI 上は 2-3 層を想定

## 表示ルール層 (分離)

tree の **各 group node に対し**、表示ルールを map で割り当てる:

```swift
enum LayoutKind {
    case horizontalSplit    // children を左右並び
    case verticalSplit      // children を上下並び
    case overlay            // 第一子の上に残りを ZStack
    case tab                // 切替 tab bar で children を順次表示
    // 将来追加: floating / picture-in-picture / grid / ...
}

struct LayoutRule {
    let kind: LayoutKind
    let params: [String: Any]  // split ratio / tab index / overlay opacity 等
}

typealias PaneLayoutMap = [UUID: LayoutRule]
```

- 同じ tree を LayoutMap 差し替えで render 変更 (純粋関数)
- LayoutMap は `@SceneStorage` で window ごとに独立
- 変更は animation 可能 (`.transition`)

### 既定ルール (fallback)

group の LayoutMap 未登録 → 既定 `.horizontalSplit`。

### ユーザー切替

- Command Palette / 右クリック: "Change layout → Split horizontal / Split vertical / Overlay / Tab"
- Keyboard: `⌥L` で直前 group の layout cycle 切替

## 実装 Phase

### 2.1 データモデル refactor

現行 `VPPaneNode.split(horizontal: Bool, ...)` は **表示ルール mix** で原則違反。refactor:

**Before**:
```swift
indirect enum VPPaneNode {
    case leaf(VPPaneLeaf)
    case split(id, horizontal, first, second)  // horizontal が node 内に
}
```

**After**:
```swift
indirect enum PaneNode {
    case leaf(PaneLeaf)
    case group(id, children: [PaneNode])  // 表示ルールなし
}

typealias PaneLayoutMap = [UUID: LayoutRule]
```

### 2.2 Canvas kind 本格実装

- CanvasRepresentable を VPPaneContainer の renderLeaf 分岐に統合
- TopicRouter `/canvas/*` 購読で content 切替

### 2.3 Preview kind

- PreviewRepresentable 新規
- drag & drop で file → preview pane 追加

### 2.4 表示ルール切替 UX

- Command Palette に "Change layout" 追加
- Overlay toggle (`⌘⇧O`) は group LayoutRule を `.overlay` に切替

### 2.5 Shell kind (将来、Phase 3)

## 非スコープ

- Bottom Deck (Phase 2 の次)
- Capture Tools (Phase 2 の次)

## 関連

- `apple/VantagePoint/Sources/VPPaneContainer.swift` — 現行 pane tree 実装
- `apple/VantagePoint/Sources/CanvasRepresentable.swift` — 既存 canvas
- `crates/vantage-point/src/stands.rs` — Stand メタファー根拠
- `TopicRouter` — Unison v2、canvas content 配信経路
