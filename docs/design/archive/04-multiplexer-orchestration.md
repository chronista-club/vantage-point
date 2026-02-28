# Multiplexer Orchestration設計

> PTYモードとAG-UIプロトコルの統合によるマルチエージェント管理

## 概要

Vantage PointのMultiplexer Orchestrationは、tmuxライクなマルチプロセス管理をPtyClaudeAgentで実現する。
複数のClaudeセッションを同時管理し、AG-UIプロトコルでフロントエンドにイベントをストリーミングする。

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│                    Multiplexer Manager                       │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────┐ │
│  │ PtyClaudeAgent  │  │ PtyClaudeAgent  │  │ PTY Agent N │ │
│  │   (Session 1)   │  │   (Session 2)   │  │ (Session N) │ │
│  └────────┬────────┘  └────────┬────────┘  └──────┬──────┘ │
│           │                    │                   │         │
│           └────────────────────┴───────────────────┘         │
│                                │                              │
│                    ┌───────────▼───────────┐                 │
│                    │   PTY Output Parser   │                 │
│                    │  (ANSI + UTF-8処理)   │                 │
│                    └───────────┬───────────┘                 │
│                                │                              │
│                    ┌───────────▼───────────┐                 │
│                    │   AG-UI EventBridge   │                 │
│                    │  (イベント変換層)     │                 │
│                    └───────────┬───────────┘                 │
│                                │                              │
│                    ┌───────────▼───────────┐                 │
│                    │   AG-UI EventBus     │                 │
│                    │  (REQ-CAP-003準拠)   │                 │
│                    └───────────────────────┘                 │
└─────────────────────────────────────────────────────────────┘
                                │
                                ▼
                    ┌───────────────────────┐
                    │    WebSocket Server   │
                    │   (Stand /api/ws)     │
                    └───────────────────────┘
                                │
                                ▼
                    ┌───────────────────────┐
                    │      WebUI Client     │
                    └───────────────────────┘
```

## コアコンポーネント

### 1. Multiplexer Manager

```rust
/// マルチセッション管理
pub struct Multiplexer {
    /// アクティブセッションのマップ
    sessions: HashMap<String, PtyClaudeAgent>,
    /// 現在フォーカスしているセッションID
    active_session: Option<String>,
    /// AG-UIイベント送信チャンネル
    event_tx: broadcast::Sender<AgUiEvent>,
    /// 設定
    config: MultiplexerConfig,
}

/// Multiplexer設定
pub struct MultiplexerConfig {
    /// 最大同時セッション数
    pub max_sessions: usize,
    /// デフォルトのAgentConfig
    pub default_agent_config: AgentConfig,
}

impl Multiplexer {
    /// 新しいセッションを作成
    pub async fn create_session(&mut self, name: String) -> Result<String, Error>;

    /// セッションを終了
    pub async fn destroy_session(&mut self, session_id: &str) -> Result<(), Error>;

    /// アクティブセッションを切り替え
    pub fn switch_session(&mut self, session_id: &str) -> Result<(), Error>;

    /// セッション一覧を取得
    pub fn list_sessions(&self) -> Vec<SessionInfo>;

    /// アクティブセッションに入力を送信
    pub async fn send_input(&self, input: &str) -> Result<(), Error>;

    /// 全セッションにブロードキャスト（Ctrl+C等）
    pub async fn broadcast_interrupt(&self) -> Result<(), Error>;
}
```

### 2. PTY Output Parser

Claude CLIのPTY出力を解析し、構造化イベントに変換する。

```rust
/// PTY出力パーサー
pub struct PtyOutputParser {
    /// ANSIシーケンス処理用バッファ
    vte_parser: vte::Parser,
    /// UTF-8デコード用バッファ
    utf8_buffer: Vec<u8>,
    /// 現在のメッセージ状態
    state: ParserState,
}

/// パーサー状態
enum ParserState {
    /// 通常テキスト出力中
    Text { message_id: String },
    /// ツール実行中
    ToolCall { tool_call_id: String, tool_name: String },
    /// 入力待ち
    AwaitingInput,
    /// 処理中（スピナー等）
    Processing,
}

/// 解析結果
pub enum ParsedOutput {
    /// プレーンテキスト（ANSIシーケンス除去済み）
    Text(String),
    /// ツール呼び出し開始検出
    ToolCallStart { name: String },
    /// ツール呼び出し終了検出
    ToolCallEnd { success: bool },
    /// 入力プロンプト検出
    InputPrompt,
    /// 制御シーケンス（無視可能）
    Control,
}
```

### 3. AG-UI Event Bridge

PTYイベントをAG-UIイベントに変換する。

```rust
/// PTY → AG-UI イベント変換
pub struct AgUiEventBridge {
    /// 現在のrun_id
    run_id: String,
    /// メッセージIDカウンター
    message_counter: u64,
}

impl AgUiEventBridge {
    /// PtyEventをAgUiEventに変換
    pub fn convert(&mut self, pty_event: PtyEvent, parsed: ParsedOutput) -> Vec<AgUiEvent> {
        match parsed {
            ParsedOutput::Text(text) => {
                vec![AgUiEvent::TextMessageContent {
                    run_id: self.run_id.clone(),
                    message_id: self.current_message_id(),
                    delta: text,
                }]
            }
            ParsedOutput::ToolCallStart { name } => {
                let tool_call_id = self.next_tool_call_id();
                vec![AgUiEvent::ToolCallStart {
                    run_id: self.run_id.clone(),
                    tool_call_id,
                    tool_name: name,
                    parent_message_id: Some(self.current_message_id()),
                    timestamp: now_millis(),
                }]
            }
            // ...その他のケース
        }
    }
}
```

## PTY → AG-UI イベントマッピング

| PTYイベント | 検出パターン | AG-UI Event |
|------------|-------------|-------------|
| テキスト出力 | プレーンテキスト | `TextMessageContent` |
| ツール開始 | `⏳ ツール名...` | `ToolCallStart` |
| ツール終了 | `✓` または `✗` | `ToolCallEnd` |
| 入力待ち | `>` プロンプト | `UserPrompt` (input) |
| 権限要求 | `Allow?` パターン | `PermissionRequest` |
| プロセス終了 | `PtyEvent::Exited` | `RunFinished` |
| エラー | `PtyEvent::Error` | `RunError` |

## Claude CLI出力パターン

### ツール実行パターン
```
⏳ Read(file_path="/path/to/file")
[ファイル内容...]
✓ Read completed
```

### 権限要求パターン
```
Claude wants to Edit /path/to/file

Allow this edit? [y/n/e/a]:
```

### 処理中パターン
```
⠋ Thinking...
⠙ Processing...
```

## WebSocket API

### エンドポイント

```
GET /api/multiplexer/ws
```

### メッセージフォーマット

#### クライアント → サーバー
```json
{
  "type": "CREATE_SESSION",
  "name": "main"
}

{
  "type": "SWITCH_SESSION",
  "session_id": "abc123"
}

{
  "type": "INPUT",
  "session_id": "abc123",
  "text": "Hello Claude"
}

{
  "type": "INTERRUPT",
  "session_id": "abc123"
}

{
  "type": "DESTROY_SESSION",
  "session_id": "abc123"
}
```

#### サーバー → クライアント
```json
{
  "type": "SESSION_CREATED",
  "session_id": "abc123",
  "name": "main"
}

{
  "type": "AGUI_EVENT",
  "session_id": "abc123",
  "event": { ... AG-UIイベント ... }
}

{
  "type": "SESSION_LIST",
  "sessions": [
    {"id": "abc123", "name": "main", "status": "running"}
  ]
}
```

## 実装計画

### Phase 1: コア構造 (1-2日)
- [ ] `Multiplexer` struct定義
- [ ] セッション管理API
- [ ] 基本的なイベント配信

### Phase 2: PTY Parser (2-3日)
- [ ] `vte` crate統合
- [ ] Claude CLI出力パターン検出
- [ ] UTF-8バッファリング

### Phase 3: AG-UI Bridge (1-2日)
- [ ] イベント変換層
- [ ] EventBus統合
- [ ] WebSocket API

### Phase 4: WebUI統合 (2-3日)
- [ ] マルチセッションUI
- [ ] タブ/パネル切り替え
- [ ] セッション状態表示

## 依存crate

```toml
[dependencies]
# 既存
pty-process = { version = "0.5.3", features = ["async"] }
tokio = { version = "1", features = ["full"] }

# 追加
vte = "0.15"                    # ANSIシーケンス解析
regex = "1"                     # パターンマッチング
tokio-stream = "0.1"            # ストリーム処理
```

## 要件ID

- REQ-MUX-001: マルチセッション管理
- REQ-MUX-002: セッション切り替え
- REQ-MUX-003: PTY出力パース
- REQ-MUX-004: AG-UIイベント変換
- REQ-MUX-005: WebSocket API
- REQ-MUX-006: セッション永続化（将来）

## 関連ドキュメント

- [01-core-concept.md](../spec/01-core-concept.md)
- [03-agent-protocol-unification.md](03-agent-protocol-unification.md)
- [04-ag-ui-requirements.md](../spec/04-ag-ui-requirements.md)
- [agent.rs](../../crates/vantage-point/src/agent.rs)
- [agui.rs](../../crates/vantage-point/src/agui.rs)
