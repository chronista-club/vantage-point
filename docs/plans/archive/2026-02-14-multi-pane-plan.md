# マルチペイン基盤 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 右サイドパネルをマルチペイン対応にし、AIが複数ペインにコンテンツを配置できるようにする

**Architecture:** MCPツール（split_pane, close_pane）→ HTTP API → WebSocket broadcast → フロントエンドのペイン状態管理。既存のプロトコル（StandMessage::Split, Close）を活用し、フロントエンドにタブUI + 分割ビューを追加する。

**Tech Stack:** Rust (rmcp, axum, serde), JavaScript (vanilla, marked.js, DOMPurify)

**Design doc:** docs/plans/2026-02-14-multi-pane-design.md

---

## 前提知識

### 重要ファイル

| ファイル | 役割 |
|---------|------|
| `crates/vantage-point/src/mcp.rs` | MCPツール定義（show, clear, toggle_pane, permission, restart） |
| `crates/vantage-point/src/protocol/messages.rs` | StandMessage enum, BrowserMessage enum |
| `crates/vantage-point/src/stand/server.rs` | Axumルーター定義 |
| `crates/vantage-point/src/stand/routes/health.rs` | show_handler, toggle_pane_handler等 |
| `web/index.html` | フロントエンド（HTML + CSS + JavaScript） |

### 既存プロトコル

`StandMessage` enumに既に `Split` と `Close` バリアントが存在する:

```rust
// messages.rs:57-63
Split { pane_id: String, direction: SplitDirection, new_pane_id: String },
Close { pane_id: String },

// messages.rs:143-148
pub enum SplitDirection { Horizontal, Vertical }
```

### ビルド＆テスト

```bash
cargo build --release -p vantage-point
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

### セキュリティ注意

フロントエンドのHTML表示には既に DOMPurify によるサニタイズが組み込まれている。
新規ペインのコンテンツ表示にも既存の `renderContent()` 関数を再利用するため、
サニタイズは自動的に適用される。

---

### Task 1: Show メッセージに title フィールドを追加

**Files:**
- Modify: `crates/vantage-point/src/protocol/messages.rs:47-54`
- Modify: `crates/vantage-point/src/mcp.rs:21-38`

**Step 1: messages.rs の Show バリアントに title を追加**

`crates/vantage-point/src/protocol/messages.rs` の `StandMessage::Show` を修正:

```rust
/// Show content in a pane
Show {
    pane_id: String,
    content: Content,
    append: bool,
    /// ペインのタイトル（タブ表示用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
},
```

**Step 2: mcp.rs の ShowParams に title を追加**

`crates/vantage-point/src/mcp.rs` の `ShowParams` に追加:

```rust
/// Pane title (displayed as tab label)
#[schemars(description = "Title for the pane tab (optional, defaults to pane_id)")]
pub title: Option<String>,
```

**Step 3: mcp.rs の show() メソッドで title を渡す**

`crates/vantage-point/src/mcp.rs` の `show()` メソッドで `StandMessage::Show` 構築時に `title` を追加:

```rust
let message = StandMessage::Show {
    pane_id: pane_id.clone(),
    content: content_enum,
    append,
    title: params.title,
};
```

**Step 4: テスト実行**

Run: `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets`

**Step 5: コミット**

```bash
git add crates/vantage-point/src/protocol/messages.rs crates/vantage-point/src/mcp.rs
git commit -m "feat: Show メッセージに title フィールドを追加"
```

---

### Task 2: split_pane MCPツールを追加

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs`

**Step 1: SplitPaneParams 構造体を追加**

`crates/vantage-point/src/mcp.rs` の `RestartParams` の後に追加:

```rust
/// Parameters for the split_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SplitPaneParams {
    /// Split direction
    #[schemars(description = "Split direction: 'horizontal' (left-right) or 'vertical' (top-bottom)")]
    pub direction: String,

    /// Source pane ID to split
    #[schemars(description = "Pane ID to split (default: 'main')")]
    pub source_pane_id: Option<String>,
}
```

**Step 2: split_pane ツールメソッドを追加**

`VantageMcp` の `#[tool_router] impl` ブロック内、`restart` メソッドの後に追加:

```rust
/// Split a pane in the browser viewer
#[tool(
    description = "Split a pane in the Vantage Point browser viewer. Creates a new pane next to the source pane."
)]
async fn split_pane(
    &self,
    rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<SplitPaneParams>,
) -> Result<CallToolResult, McpError> {
    let source_pane_id = params
        .source_pane_id
        .unwrap_or_else(|| "main".to_string());

    let direction = match params.direction.as_str() {
        "vertical" => crate::protocol::SplitDirection::Vertical,
        _ => crate::protocol::SplitDirection::Horizontal,
    };

    // 新しいペインIDを生成（UUIDの先頭セグメント）
    let new_pane_id = format!(
        "pane-{}",
        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0")
    );

    let message = StandMessage::Split {
        pane_id: source_pane_id,
        direction,
        new_pane_id: new_pane_id.clone(),
    };

    let url = self.stand_url.lock().await;
    let result = self
        .client
        .post(format!("{}/api/split-pane", *url))
        .json(&message)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Pane split. New pane ID: '{}'",
                new_pane_id
            ))]))
        }
        Ok(resp) => {
            let status = resp.status();
            Err(McpError::internal_error(
                format!("Stand returned error: {}", status),
                None,
            ))
        }
        Err(e) => Err(McpError::internal_error(
            format!("Failed to connect to Stand: {}", e),
            None,
        )),
    }
}
```

**Step 3: テスト実行**

Run: `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets`

**Step 4: コミット**

```bash
git add crates/vantage-point/src/mcp.rs
git commit -m "feat: split_pane MCPツールを追加"
```

---

### Task 3: close_pane MCPツールを追加

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs`

**Step 1: ClosePaneParams 構造体を追加**

```rust
/// Parameters for the close_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClosePaneParams {
    /// Pane ID to close
    #[schemars(description = "Pane ID to close. The last remaining pane cannot be closed.")]
    pub pane_id: String,
}
```

**Step 2: close_pane ツールメソッドを追加**

```rust
/// Close a pane in the browser viewer
#[tool(
    description = "Close a pane in the Vantage Point browser viewer. The last remaining pane cannot be closed."
)]
async fn close_pane(
    &self,
    rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClosePaneParams>,
) -> Result<CallToolResult, McpError> {
    let message = StandMessage::Close {
        pane_id: params.pane_id.clone(),
    };

    let url = self.stand_url.lock().await;
    let result = self
        .client
        .post(format!("{}/api/close-pane", *url))
        .json(&message)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status() == reqwest::StatusCode::OK => Ok(CallToolResult::success(
            vec![Content::text(format!("Pane '{}' closed", params.pane_id))],
        )),
        Ok(resp) => {
            let status = resp.status();
            Err(McpError::internal_error(
                format!("Stand returned error: {}", status),
                None,
            ))
        }
        Err(e) => Err(McpError::internal_error(
            format!("Failed to connect to Stand: {}", e),
            None,
        )),
    }
}
```

**Step 3: テスト実行**

Run: `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets`

**Step 4: コミット**

```bash
git add crates/vantage-point/src/mcp.rs
git commit -m "feat: close_pane MCPツールを追加"
```

---

### Task 4: HTTPエンドポイントを追加

**Files:**
- Modify: `crates/vantage-point/src/stand/routes/health.rs`
- Modify: `crates/vantage-point/src/stand/server.rs:77-98`

**Step 1: health.rs にハンドラーを追加**

`crates/vantage-point/src/stand/routes/health.rs` の `toggle_pane_handler` の後に追加:

```rust
/// POST /api/split-pane - Split a pane
pub async fn split_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/close-pane - Close a pane
pub async fn close_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}
```

**Step 2: server.rs にルートを追加**

`crates/vantage-point/src/stand/server.rs` の `.route("/api/toggle-pane", ...)` の後に追加:

```rust
.route("/api/split-pane", post(health::split_pane_handler))
.route("/api/close-pane", post(health::close_pane_handler))
```

**Step 3: ビルド確認**

Run: `cargo build -p vantage-point`
Run: `cargo test --workspace`

**Step 4: コミット**

```bash
git add crates/vantage-point/src/stand/routes/health.rs crates/vantage-point/src/stand/server.rs
git commit -m "feat: split-pane, close-pane HTTPエンドポイントを追加"
```

---

### Task 5: MCP ServerInfo を更新

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs:506-518`

**Step 1: ServerInfo の instructions を更新**

`crates/vantage-point/src/mcp.rs` の `get_info()` メソッドの instructions を更新:

```rust
fn get_info(&self) -> ServerInfo {
    ServerInfo {
        instructions: Some(
            "Vantage Point Stand - Display rich content (markdown, HTML, images) in a browser viewer. \
             Use 'show' to display content, 'clear' to clear panes, 'split_pane' to split panes, \
             'close_pane' to close panes, 'toggle_pane' to show/hide panels, \
             'permission' to request user approval, \
             and 'restart' to restart the Stand (useful after rebuilding the binary).".into()
        ),
        capabilities: ServerCapabilities::builder().enable_tools().build(),
        ..Default::default()
    }
}
```

**Step 2: ビルド確認**

Run: `cargo build -p vantage-point`

**Step 3: コミット**

```bash
git add crates/vantage-point/src/mcp.rs
git commit -m "docs: MCPサーバー instructions にマルチペインツールを追記"
```

---

### Task 6: フロントエンド - ペイン状態管理を実装

**Files:**
- Modify: `web/index.html`

この Task はフロントエンドの JavaScript 部分を大幅に変更する。

**Step 1: CSS にタブバーとペイン分割のスタイルを追加**

`web/index.html` の `</style>` タグの直前に CSS を追加する。
タブバー（`.pane-tabs`）、タブ（`.pane-tab`）、分割ハンドル（`.pane-split-handle`）、
ペインコンテナ（`.pane-container`）のスタイル。

**Step 2: HTML のサイドパネル構造を変更**

`#side-panel` 内を以下の構造に変更:
- `.side-panel-header` (既存を維持、タイトルに `id="side-panel-title"` 追加)
- `.pane-tabs#pane-tabs` (新規: タブバー)
- `.pane-container#pane-container` (新規: ペインコンテナ)
  - `.pane-content.active#pane-main` (既存を移動)

**Step 3: JavaScript のペイン状態管理を実装**

paneState オブジェクト（panes Map, activePane, splitMode）を追加し、
以下の関数を実装:

- `createPane(paneId, title)` - ペイン作成（DOM要素＋タブ更新）
- `removePane(paneId)` - ペイン削除
- `switchToPane(paneId)` - タブ切替
- `updateTabs()` - タブバーの再描画
- `enterSplitMode(direction, paneIds)` - 分割モード開始
- `exitSplitMode()` - 分割モード終了

注意: HTML表示は既存の `renderContent()` を再利用する。
この関数内で DOMPurify.sanitize() が呼ばれるため、XSS対策は維持される。
タブの閉じるボタンは `textContent` で `x` 文字を設定する（安全なDOM操作）。

**Step 4: handleMessage の show/clear ケースを更新、split/close ケースを追加**

- `show`: paneState にペインが無ければ `createPane()` で自動作成、`renderContent()` で描画
- `clear`: 既存と同様
- `split`: `createPane()` → `enterSplitMode()`
- `close`: `removePane()`

**Step 5: ビルド＆動作確認**

Run: `cargo build --release -p vantage-point`

手動確認:
1. `vp start -d simple` でStandを起動
2. MCPツールから `show(pane_id="test", content="# Test", title="テスト")` を送信 → タブが出現
3. `split_pane(direction="horizontal")` → 分割ビューが表示
4. `close_pane(pane_id="test")` → ペインが閉じる
5. 従来通り `show(content="# Hello")` → mainに表示される（後方互換）

**Step 6: コミット**

```bash
git add web/index.html
git commit -m "feat: フロントエンドにマルチペイン（タブ+分割）を実装"
```

---

### Task 7: 統合テスト＆最終確認

**Files:**
- Modify: `crates/vantage-point/src/protocol/messages.rs` (テスト追加)

**Step 1: プロトコルのシリアライズテストを追加**

`crates/vantage-point/src/protocol/messages.rs` の `mod tests` 内に以下テストを追加:

- `test_show_with_title_serialization`: title付きShowメッセージのJSON出力確認
- `test_show_without_title_omits_field`: title=NoneではJSONにtitleが含まれないことを確認
- `test_split_message_serialization`: Splitメッセージの正しいシリアライズ確認
- `test_close_message_serialization`: Closeメッセージの正しいシリアライズ確認

**Step 2: 全テスト実行**

Run: `cargo test --workspace`
Run: `cargo fmt --all -- --check`
Run: `cargo clippy --workspace --all-targets`

**Step 3: コミット**

```bash
git add crates/vantage-point/src/protocol/messages.rs
git commit -m "test: マルチペインのプロトコルシリアライズテストを追加"
```

---

## 実行オプション

実装計画は7タスク。

**1. Subagent-Driven（このセッション）** - タスクごとにサブエージェントを起動し、レビューしながら進める

**2. Parallel Session（別セッション）** - 新しいセッションで executing-plans を使ってバッチ実行
