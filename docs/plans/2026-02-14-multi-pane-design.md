# マルチペイン基盤設計

> AIとユーザーの共同キャンバスを実現するための基盤

## 概要

Vantage Pointの右サイドパネルをマルチペイン対応に拡張する。
現在の単一ペイン（`pane_id = "main"`）から、複数ペインのタブ切替・分割表示を可能にする。

## 設計方針

- **シンプルID方式**: ペインをフラットなIDで管理
- **AI提案→ユーザー調整**: AIがペインを作成・配置し、ユーザーがタブで切替
- **段階的拡張**: 基本のタブ+分割から始めて、試しながら育てる
- **後方互換**: pane_id省略で従来通り "main" に表示

## MCPツール設計

### 既存ツール（拡張）

| ツール | 変更点 |
|--------|--------|
| `show` | `title` パラメータ追加。存在しないpane_idを指定した場合タブとして自動作成 |
| `clear` | 変更なし（pane_id対応は既存） |
| `toggle_pane` | 変更なし |

### 新規ツール

| ツール | パラメータ | 動作 |
|--------|-----------|------|
| `split_pane` | `direction` ("horizontal"/"vertical"), `source_pane_id` (任意、デフォ"main") | 右パネル内でペインを分割。新しいpane_idを返す |
| `close_pane` | `pane_id` | ペインを閉じる。最後の1つは閉じない |

### ツール利用例

```
# シンプルな表示（従来互換）
show(content="# Hello")

# 名前付きペインに表示（自動作成）
show(pane_id="design", content="# 設計書", title="設計")

# ペインを上下分割
split_pane(direction="vertical")  → 新pane_id "pane-1" を返す

# 分割先に表示
show(pane_id="pane-1", content="# Diff結果")

# ペインを閉じる
close_pane(pane_id="pane-1")
```

## プロトコル設計

### StandMessage enum 拡張

```rust
pub enum StandMessage {
    // 既存
    Show { pane_id: String, content: Content, append: bool },
    Clear { pane_id: String },
    TogglePane { pane_id: String, visible: Option<bool> },

    // 新規
    SplitPane {
        source_pane_id: String,
        new_pane_id: String,
        direction: SplitDirection,
    },
    ClosePane {
        pane_id: String,
    },
}

pub enum SplitDirection {
    Horizontal,  // 左右に分割
    Vertical,    // 上下に分割
}
```

### Show メッセージ拡張

`title` フィールドを追加:

```json
{
  "type": "show",
  "pane_id": "design-doc",
  "title": "設計書",
  "content": { "Markdown": "..." },
  "append": false
}
```

## HTTPエンドポイント

| メソッド | パス | ハンドラ | 動作 |
|----------|------|---------|------|
| POST | `/api/show` | show_handler | 既存（変更なし） |
| POST | `/api/split-pane` | split_pane_handler | 新規 |
| POST | `/api/close-pane` | close_pane_handler | 新規 |
| POST | `/api/toggle-pane` | toggle_pane_handler | 既存（変更なし） |

## フロントエンド設計

### DOM構造

```html
<div id="side-panel">
  <!-- タブバー: ペインが2つ以上で表示 -->
  <div class="pane-tabs">
    <button class="pane-tab active" data-pane-id="main">Main</button>
    <button class="pane-tab" data-pane-id="design">設計書</button>
  </div>

  <!-- ペインコンテナ -->
  <div class="pane-container">
    <!-- タブモード: アクティブなペインのみ表示 -->
    <div class="pane active" id="pane-main">...</div>
    <div class="pane" id="pane-design">...</div>
  </div>

  <!-- 分割モード: 2つのペインを同時表示 -->
  <!-- split_pane実行時にpane-containerが分割レイアウトに切り替わる -->
</div>
```

### ペイン管理（JavaScript）

```javascript
// ペイン状態管理
const paneState = {
  panes: new Map(),      // pane_id → { element, title, content }
  activePane: 'main',    // 現在アクティブなタブ
  splitMode: null,       // null | { direction, panes: [id, id] }
};

// show受信時: ペインが無ければ自動作成
function handleShow(msg) {
  if (!paneState.panes.has(msg.pane_id)) {
    createPane(msg.pane_id, msg.title);
  }
  renderContent(getPaneElement(msg.pane_id), msg.content, msg.append);
}
```

### 動作ルール

1. **ペイン1つ**: タブ非表示、全面表示（現在と同じ振る舞い）
2. **ペイン2つ以上**: タブバー表示、クリックで切替
3. **split_pane実行時**: 分割ビューで2つ同時表示
4. **close_pane実行時**: ペインを削除、最後の1つは閉じない

## 実装スコープ

### Phase 1（今回）

1. StandMessage enum に SplitPane, ClosePane 追加
2. Show メッセージに title フィールド追加
3. split_pane, close_pane のMCPツール実装
4. 対応するHTTPエンドポイント追加
5. フロントエンドにタブUI実装
6. フロントエンドに分割表示実装

### Phase 2（将来）

- ドラッグ&ドロップでのペイン移動
- レイアウト永続化（再起動後も維持）
- Mermaidダイアグラムのレンダリング対応
- ユーザーによるリサイズ（ドラッグハンドル）
- ダッシュボード/進捗表示の専用UI

## 関連ドキュメント

- コアコンセプト: docs/spec/01-core-concept.md
- アーキテクチャ: docs/design/01-architecture.md

---

*作成日: 2026-02-14*
*ステータス: Approved*
