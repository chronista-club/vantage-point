# View System 設計

> ViewPoint のレイアウト・ワークスペース・フローティングウィンドウ設計

## 概要

ViewPointは、固定3ペインレイアウトをベースに、
中央のタイリング、フローティングウィンドウ、ワークスペースを組み合わせた
柔軟なUI構成を提供する。

## レイアウト構造

### 基本構成

```
┌─────────────────────────────────────────────────────────────┐
│  Workspace 1 (active)   [Workspace 2]  [Workspace 3]  [+]   │
├─────────┬───────────────────────────────────┬───────────────┤
│         │                                   │               │
│  Left   │         Center (Tiling)           │    Right      │
│  Panel  │  ┌─────────────┬─────────────┐    │    Panel      │
│         │  │   Tile 1    │   Tile 2    │    │               │
│  固定幅 │  ├─────────────┼─────────────┤    │   固定幅      │
│  折り   │  │   Tile 3    │   Tile 4    │    │   折り        │
│  畳み   │  └─────────────┴─────────────┘    │   畳み        │
│  可能   │         ↑ 自由分割                │   可能        │
│         │                                   │               │
├─────────┴───────────────────────────────────┴───────────────┤
│                    ┌──────────────────┐                     │
│                    │ Floating Window  │ ← ドラッグ可能      │
│                    │  (任意位置)      │                     │
│                    └──────────────────┘                     │
└─────────────────────────────────────────────────────────────┘
```

## コンポーネント詳細

### 1. Workspace（ワークスペース）

複数のレイアウト構成を保存・切り替え:

```rust
struct Workspace {
    id: String,
    name: String,
    layout: Layout,
    floating_windows: Vec<FloatingWindow>,
    is_active: bool,
}
```

**機能:**
- ワークスペースの作成・削除・名前変更
- ワークスペース間の切り替え（MIDI対応）
- レイアウト状態の自動保存

### 2. Left/Right Panel（サイドパネル）

固定幅の折りたたみ可能パネル:

```rust
struct SidePanel {
    id: String,           // "left" or "right"
    width: u32,           // ピクセル幅
    min_width: u32,       // 最小幅
    max_width: u32,       // 最大幅
    collapsed: bool,      // 折りたたみ状態
    content: PaneContent, // 表示コンテンツ
}
```

**Left Panel 用途例:**
- セッション一覧
- プロジェクト一覧
- ファイルツリー

**Right Panel 用途例:**
- Multiplexer進捗
- Terminal一覧
- デバッグ情報

### 3. Center Tiling（中央タイリング）

tmux/i3風の自由分割エリア:

```rust
struct TilingContainer {
    id: String,
    split: SplitDirection,  // Horizontal or Vertical
    ratio: f32,             // 分割比率 (0.0-1.0)
    children: Vec<TileNode>,
}

enum TileNode {
    Container(TilingContainer),
    Pane(TilePane),
}

struct TilePane {
    id: String,
    content: PaneContent,
    focused: bool,
}
```

**操作:**
- 水平分割 (Horizontal Split)
- 垂直分割 (Vertical Split)
- ペインの移動・入れ替え
- ペインのリサイズ
- ペインのクローズ

### 4. Floating Window（フローティングウィンドウ）

ドラッグ可能なオーバーレイウィンドウ:

```rust
struct FloatingWindow {
    id: String,
    title: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    z_index: u32,
    content: PaneContent,
    minimized: bool,
}
```

**機能:**
- ドラッグで位置移動
- リサイズハンドル
- 最小化・最大化
- 常に手前に表示オプション

## PaneContent（ペインコンテンツ）

各ペインに表示可能なコンテンツタイプ:

```rust
enum PaneContent {
    // テキスト系
    Chat,                     // チャットUI
    Markdown(String),         // Markdownレンダリング
    Html(String),             // HTMLコンテンツ
    Log(Vec<LogEntry>),       // ログビューア

    // ターミナル系
    Terminal(TerminalId),     // PTYターミナル

    // 特殊コンポーネント
    SessionList,              // セッション一覧
    ProjectList,              // プロジェクト一覧
    MultiplexerProgress,      // Multiplexer進捗
    MidiMonitor,              // MIDI入力モニター

    // カスタム
    Custom { component: String, props: serde_json::Value },
}
```

## レイアウトプリセット

よく使うレイアウトのプリセット:

### Default（デフォルト）

```
┌────┬────────────────┬────┐
│    │                │    │
│ L  │     Chat       │ R  │
│    │                │    │
└────┴────────────────┴────┘
```

### Development（開発）

```
┌────┬────────┬───────┬────┐
│    │ Chat   │ Term  │    │
│ L  ├────────┼───────┤ R  │
│    │ Logs   │ Term  │    │
└────┴────────┴───────┴────┘
```

### Multiplexer（並列実行）

```
┌────┬────────────────┬────┐
│    │   Progress     │    │
│ L  ├────────────────┤ R  │
│    │   Results      │    │
└────┴────────────────┴────┘
```

## 操作API

### WebSocket Message

```typescript
// ワークスペース操作
{ type: "workspace_create", name: string }
{ type: "workspace_switch", id: string }
{ type: "workspace_delete", id: string }

// パネル操作
{ type: "panel_toggle", panel_id: "left" | "right" }
{ type: "panel_resize", panel_id: string, width: number }

// タイル操作
{ type: "tile_split", pane_id: string, direction: "h" | "v" }
{ type: "tile_close", pane_id: string }
{ type: "tile_focus", pane_id: string }
{ type: "tile_resize", pane_id: string, ratio: number }
{ type: "tile_swap", pane_id_a: string, pane_id_b: string }

// フローティング操作
{ type: "floating_create", content: PaneContent }
{ type: "floating_move", window_id: string, x: number, y: number }
{ type: "floating_resize", window_id: string, width: number, height: number }
{ type: "floating_close", window_id: string }
{ type: "floating_minimize", window_id: string }
{ type: "floating_maximize", window_id: string }
```

### MCP Tools

```json
{
    "name": "view_split",
    "description": "ペインを分割",
    "inputSchema": {
        "properties": {
            "pane_id": { "type": "string" },
            "direction": { "enum": ["horizontal", "vertical"] }
        }
    }
}
```

```json
{
    "name": "view_set_content",
    "description": "ペインにコンテンツを設定",
    "inputSchema": {
        "properties": {
            "pane_id": { "type": "string" },
            "content_type": { "enum": ["chat", "markdown", "terminal", "log"] },
            "content": { "type": "string" }
        }
    }
}
```

```json
{
    "name": "view_create_floating",
    "description": "フローティングウィンドウを作成",
    "inputSchema": {
        "properties": {
            "title": { "type": "string" },
            "content_type": { "type": "string" },
            "width": { "type": "number" },
            "height": { "type": "number" }
        }
    }
}
```

## MIDI連携

MIDIコントローラーとの統合:

### パッド割り当て例（LPD8）

```
┌─────┬─────┬─────┬─────┐
│ WS1 │ WS2 │ WS3 │ WS4 │  ← ワークスペース切り替え
├─────┼─────┼─────┼─────┤
│ T1  │ T2  │ T3  │ T4  │  ← タイルフォーカス
└─────┴─────┴─────┴─────┘

ノブ:
  K1: Left Panel幅
  K2: Right Panel幅
  K3: 分割比率
  K4: (未割り当て)
```

### MIDI → View アクション

| MIDI Event | View Action |
|------------|-------------|
| Pad 1-4 Note On | ワークスペース 1-4 に切り替え |
| Pad 5-8 Note On | タイル 1-4 にフォーカス |
| Knob 1 CC | Left Panel 幅調整 |
| Knob 2 CC | Right Panel 幅調整 |
| Pad + Shift | タイル分割モード |

## 状態永続化

ワークスペース状態はVantage DBに保存:

```sql
-- SurrealDB Schema
DEFINE TABLE workspaces SCHEMAFULL;
DEFINE FIELD id ON workspaces TYPE string;
DEFINE FIELD name ON workspaces TYPE string;
DEFINE FIELD layout ON workspaces TYPE object;
DEFINE FIELD floating_windows ON workspaces TYPE array;
DEFINE FIELD created_at ON workspaces TYPE datetime;
DEFINE FIELD updated_at ON workspaces TYPE datetime;
```

## Hot Reload対応

The World/Paisley Park再起動時:

1. WebSocket再接続
2. 現在のワークスペースIDをlocalStorageから取得
3. サーバーからワークスペース状態を取得
4. レイアウト復元
5. 各ペインのコンテンツ再取得

## 関連ドキュメント

- [spec/07-the-world.md](../spec/07-the-world.md)
- [design/10-multiplexer.md](./10-multiplexer.md)
- [design/12-midi-mapping.md](./12-midi-mapping.md)
