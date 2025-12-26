//! Agentモジュール - Claude CLI統合によるチャット機能
//!
//! 3つの実行モードを提供:
//! - **OneShotモード**: `claude -p` で単発プロンプト → 応答
//! - **Interactiveモード**: `claude -p --input-format stream-json` で持続プロセス
//! - **PTYモード**: `claude` (真の対話モード) をPTY経由で端末エミュレーション
//!
//! OneShotとInteractiveモードは `--output-format stream-json` で構造化レスポンスを使用。
//! PTYモードは完全な対話体験のため生の端末I/Oを使用。
//!
//! ## Stream-JSON 入力フォーマット (Interactiveモード用)
//!
//! Claude CLI `--input-format stream-json` はJSONL（改行区切りJSON）を要求。
//! 各メッセージは以下のスキーマに従う必要がある:
//!
//! ```json
//! {"type":"user","message":{"role":"user","content":[{"type":"text","text":"メッセージ"}]}}
//! ```
//!
//! ### 必須フィールド:
//! - `type`: `"user"` (ユーザーメッセージを示す)
//! - `message`: 以下を含むオブジェクト:
//!   - `role`: `"user"` (送信者を識別)
//!   - `content`: コンテンツオブジェクトの配列:
//!     - `type`: `"text"` (コンテンツタイプ)
//!     - `text`: 実際のメッセージテキスト (文字列)
//!
//! ### 使用例:
//! ```bash
//! echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Hello"}]}}' | \
//!   claude -p --output-format=stream-json --input-format=stream-json --verbose
//! ```
//!
//! ### ストリームチェーン:
//! 複数のClaudeインスタンスをパイプで連結可能:
//! ```bash
//! claude -p --output-format stream-json "First task" | \
//!   claude -p --input-format stream-json --output-format stream-json "Process results"
//! ```
//!
//! 参考:
//! - <https://code.claude.com/docs/en/headless.md>
//! - <https://code.claude.com/docs/en/cli-reference.md>

use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

/// エージェント通信用メッセージ型
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// セッション初期化完了（全情報付き）
    SessionInit {
        session_id: String,
        model: Option<String>,
        tools: Vec<String>,
        mcp_servers: Vec<String>,
    },
    /// テキストコンテンツのチャンク
    TextChunk(String),
    /// ツール実行開始
    ToolExecuting { name: String },
    /// ツール実行完了
    ToolResult { name: String, preview: String },
    /// ストリーム完了（最終結果付き）
    Done { result: String, cost: Option<f64> },
    /// エラー発生
    Error(String),
}

/// エージェント実行モード
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum AgentMode {
    /// OneShotモード: 単発プロンプト → 応答、プロセス終了
    OneShot,
    /// Interactiveモード: 持続プロセス、stdin JSON経由で複数ターン
    /// `claude -p --input-format stream-json` を使用
    Interactive,
    /// PTYモード: PTY端末エミュレーションによる真の対話モード
    /// `-p`なしの `claude` で完全な端末体験（デフォルト）
    #[default]
    Pty,
}

/// Claude CLIのパーミッションモード
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum PermissionMode {
    /// デフォルトのパーミッション処理
    #[default]
    Default,
    /// 確認なしで全編集操作を許可
    AcceptEdits,
    /// 全パーミッションチェックをバイパス (--dangerously-skip-permissions)
    BypassPermissions,
    /// プランモード - 計画に集中
    Plan,
}

impl PermissionMode {
    fn as_cli_arg(&self) -> Option<&'static str> {
        match self {
            PermissionMode::Default => None,
            PermissionMode::AcceptEdits => Some("acceptEdits"),
            PermissionMode::BypassPermissions => Some("bypassPermissions"),
            PermissionMode::Plan => Some("plan"),
        }
    }
}

/// Claudeエージェント設定
///
/// Claude CLIオプションにマッピングして柔軟な制御を実現
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    // === 実行モード ===
    /// 実行モード (OneShot, Interactive, Pty)
    pub mode: AgentMode,

    // === セッション制御 ===
    /// エージェントの作業ディレクトリ
    pub working_dir: Option<String>,
    /// 再開するセッションID (--resume <id>)
    pub session_id: Option<String>,
    /// --continueフラグ使用（最新セッション再開）
    pub use_continue: bool,
    /// 再開時にセッションをフォーク (--fork-session)
    pub fork_session: bool,

    // === モデル & プロンプト ===
    /// 使用モデル (--model): "sonnet", "opus", "haiku", またはフルネーム
    pub model: Option<String>,
    /// システムプロンプト (--system-prompt)
    pub system_prompt: Option<String>,
    /// デフォルトシステムプロンプトに追加 (--append-system-prompt)
    pub append_system_prompt: Option<String>,

    // === ツール & MCP ===
    /// 許可ツール (--allowedTools): 例 ["Bash(git:*)", "Edit"]
    pub allowed_tools: Vec<String>,
    /// 禁止ツール (--disallowedTools)
    pub disallowed_tools: Vec<String>,
    /// MCP設定ファイルパス (--mcp-config)
    pub mcp_config: Option<String>,
    /// 厳格なMCP設定のみ使用 (--strict-mcp-config)
    pub strict_mcp_config: bool,

    // === パーミッション & 安全性 ===
    /// パーミッションモード (--permission-mode)
    pub permission_mode: PermissionMode,
    /// 全パーミッションチェックをバイパス (--dangerously-skip-permissions)
    /// 注意: trueの場合、permission_modeは無視される
    pub skip_permissions: bool,
    /// パーミッションプロンプトツール (--permission-prompt-tool)
    /// MCPツールを使用してパーミッション承認を処理
    /// 例: "vantage-point__permission"
    pub permission_prompt_tool: Option<String>,

    // === 予算 & 制限 ===
    /// APIコール最大金額 (--max-budget-usd, printモードのみ)
    pub max_budget_usd: Option<f64>,

    // === 出力制御 ===
    /// 詳細出力を有効化 (--verbose)
    pub verbose: bool,
    /// デバッグモード有効化、オプションでフィルタ指定 (--debug)
    pub debug: Option<String>,
}

/// Claude CLIと通信するエージェント
#[derive(Clone)]
pub struct ClaudeAgent {
    config: AgentConfig,
}

impl ClaudeAgent {
    pub fn new() -> Self {
        Self {
            config: AgentConfig::default(),
        }
    }

    pub fn with_config(config: AgentConfig) -> Self {
        Self { config }
    }

    /// 会話継続用のセッションIDを設定
    pub fn with_session(mut self, session_id: String) -> Self {
        self.config.session_id = Some(session_id);
        self
    }

    /// 作業ディレクトリを設定
    pub fn with_working_dir(mut self, dir: String) -> Self {
        self.config.working_dir = Some(dir);
        self
    }

    /// モデルを設定
    pub fn with_model(mut self, model: String) -> Self {
        self.config.model = Some(model);
        self
    }

    /// Claude CLIにメッセージを送信し、レスポンスをストリーミング
    pub async fn chat(&self, message: &str) -> mpsc::Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel(100);
        let message = message.to_string();
        let config = self.config.clone();

        tokio::spawn(async move {
            let result = run_claude_cli(&message, &config, tx.clone()).await;
            if let Err(e) = result {
                let _ = tx.send(AgentEvent::Error(e.to_string())).await;
            }
        });

        rx
    }
}

impl Default for ClaudeAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// Claude CLI JSONメッセージ型
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ClaudeMessage {
    System {
        session_id: String,
        #[serde(default)]
        subtype: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        tools: Option<Vec<String>>,
        #[serde(default)]
        mcp_servers: Option<Vec<McpServerInfo>>,
    },
    Assistant {
        message: AssistantMessage,
        session_id: String,
    },
    Result {
        #[serde(default)]
        result: Option<String>,
        session_id: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        total_cost_usd: Option<f64>,
    },
}

/// Claude CLIからのMCPサーバー情報
#[derive(Debug, serde::Deserialize)]
struct McpServerInfo {
    name: String,
    #[serde(default)]
    status: String,
}

#[derive(Debug, serde::Deserialize)]
struct AssistantMessage {
    content: Vec<ContentBlock>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    ToolResult {
        tool_use_id: String,
    },
    #[serde(other)]
    Other,
}

/// 設定からコマンドに共通CLIオプションを適用
fn apply_cli_args(cmd: &mut Command, config: &AgentConfig) {
    // === 出力制御 ===
    // stream-json出力にはverboseが必須
    if config.verbose {
        cmd.arg("--verbose");
    }

    // デバッグモード
    if let Some(ref filter) = config.debug {
        if filter.is_empty() {
            cmd.arg("--debug");
        } else {
            cmd.arg("--debug").arg(filter);
        }
    }

    // === パーミッション & 安全性 ===
    if config.skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    } else if let Some(mode) = config.permission_mode.as_cli_arg() {
        cmd.arg("--permission-mode").arg(mode);
    }

    // パーミッションプロンプトツール
    if let Some(ref tool) = config.permission_prompt_tool {
        cmd.arg("--permission-prompt-tool").arg(tool);
    }

    // === セッション制御 ===
    if config.use_continue {
        cmd.arg("--continue");
    } else if let Some(ref session_id) = config.session_id {
        cmd.arg("--resume").arg(session_id);
    }

    if config.fork_session {
        cmd.arg("--fork-session");
    }

    // === モデル & プロンプト ===
    if let Some(ref model) = config.model {
        cmd.arg("--model").arg(model);
    }

    if let Some(ref system_prompt) = config.system_prompt {
        cmd.arg("--system-prompt").arg(system_prompt);
    }

    if let Some(ref append_prompt) = config.append_system_prompt {
        cmd.arg("--append-system-prompt").arg(append_prompt);
    }

    // === ツール & MCP ===
    if !config.allowed_tools.is_empty() {
        cmd.arg("--allowedTools")
            .arg(config.allowed_tools.join(","));
    }

    if !config.disallowed_tools.is_empty() {
        cmd.arg("--disallowedTools")
            .arg(config.disallowed_tools.join(","));
    }

    if let Some(ref mcp_config) = config.mcp_config {
        cmd.arg("--mcp-config").arg(mcp_config);
    }

    if config.strict_mcp_config {
        cmd.arg("--strict-mcp-config");
    }

    // === 予算 ===
    if let Some(budget) = config.max_budget_usd {
        cmd.arg("--max-budget-usd").arg(budget.to_string());
    }

    // === 作業ディレクトリ ===
    if let Some(ref dir) = config.working_dir {
        cmd.current_dir(dir);
    }
}

/// claude CLIをstream-json出力で実行しレスポンスを解析 (OneShotモード)
async fn run_claude_cli(
    prompt: &str,
    config: &AgentConfig,
    tx: mpsc::Sender<AgentEvent>,
) -> anyhow::Result<()> {
    let mut cmd = Command::new("claude");

    // printモードでstream-json出力を使用
    cmd.arg("-p").arg("--output-format").arg("stream-json");

    // stream-jsonにはverboseが必須
    cmd.arg("--verbose");

    // 共通CLIオプションを適用
    apply_cli_args(&mut cmd, config);

    // プロンプトを追加（最後の位置引数である必要あり）
    cmd.arg(prompt);

    // stdioを設定
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    tracing::info!(
        "Claude CLI起動 (session: {:?}, mcp_config: {:?})",
        config.session_id.as_deref().unwrap_or("new"),
        config.mcp_config.as_deref().unwrap_or("default")
    );

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "Claude CLIの起動に失敗。'claude'がインストールされPATHにあるか確認: {}",
            e
        )
    })?;

    let stdout = child.stdout.take().expect("stdoutがキャプチャされていない");
    let stderr = child.stderr.take().expect("stderrがキャプチャされていない");

    // 重複を避けるため最後に送信したテキストを追跡
    let mut last_text = String::new();

    // stdoutを行ごとに読み取り（各行はJSONメッセージ）
    let tx_stdout = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut session_id_sent = false;

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<ClaudeMessage>(&line) {
                Ok(msg) => match msg {
                    ClaudeMessage::System {
                        session_id,
                        subtype,
                        model,
                        tools,
                        mcp_servers,
                    } => {
                        // "init"サブタイプに対してのみ初期化イベントを送信
                        if !session_id_sent && subtype.as_deref() == Some("init") {
                            let mcp_names: Vec<String> = mcp_servers
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|s| s.status == "connected")
                                .map(|s| s.name)
                                .collect();

                            let tools_list = tools.unwrap_or_default();
                            tracing::info!(
                                "Claude CLI初期化: session={}, model={:?}, tools={}, mcp_servers={}",
                                session_id,
                                model,
                                tools_list.len(),
                                mcp_names.len()
                            );

                            let _ = tx_stdout
                                .send(AgentEvent::SessionInit {
                                    session_id: session_id.clone(),
                                    model,
                                    tools: tools_list,
                                    mcp_servers: mcp_names,
                                })
                                .await;
                            session_id_sent = true;
                        }
                    }
                    ClaudeMessage::Assistant { message, .. } => {
                        // コンテンツブロックからテキストとツールイベントを抽出
                        for block in message.content {
                            match block {
                                ContentBlock::Text { text } => {
                                    // 増分テキストを送信（新しいコンテンツのみ）
                                    if text.len() > last_text.len() && text.starts_with(&last_text)
                                    {
                                        let new_text = &text[last_text.len()..];
                                        let _ = tx_stdout
                                            .send(AgentEvent::TextChunk(new_text.to_string()))
                                            .await;
                                    } else if text != last_text {
                                        // テキストが完全に変更された場合、全て送信
                                        let _ = tx_stdout
                                            .send(AgentEvent::TextChunk(text.clone()))
                                            .await;
                                    }
                                    last_text = text;
                                }
                                ContentBlock::ToolUse { name, .. } => {
                                    let _ =
                                        tx_stdout.send(AgentEvent::ToolExecuting { name }).await;
                                }
                                ContentBlock::ToolResult { .. } | ContentBlock::Other => {}
                            }
                        }
                    }
                    ClaudeMessage::Result {
                        result,
                        is_error,
                        total_cost_usd,
                        ..
                    } => {
                        if is_error {
                            let _ = tx_stdout
                                .send(AgentEvent::Error(result.unwrap_or_default()))
                                .await;
                        } else {
                            let _ = tx_stdout
                                .send(AgentEvent::Done {
                                    result: result.unwrap_or_default(),
                                    cost: total_cost_usd,
                                })
                                .await;
                        }
                    }
                },
                Err(e) => {
                    tracing::debug!("Claudeメッセージ解析失敗: {} - line: {}", e, line);
                }
            }
        }
    });

    // stderrを読み取り（デバッグ用）
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!("Claude CLI stderr: {}", line);
        }
    });

    // プロセス完了を待機
    let status = child.wait().await?;

    // 出力タスク完了を待機
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if !status.success() {
        let _ = tx
            .send(AgentEvent::Error(format!(
                "Claude CLIが終了: ステータス {}",
                status
            )))
            .await;
    }

    Ok(())
}

// =============================================================================
// Interactiveモード実装
// =============================================================================

/// stream-json入力用コンテンツブロック
#[derive(Debug, serde::Serialize)]
struct StreamInputContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

/// stream-json入力用メッセージペイロード
#[derive(Debug, serde::Serialize)]
struct StreamInputMessagePayload {
    role: String,
    content: Vec<StreamInputContent>,
}

/// stream-json入力用メッセージフォーマット
///
/// 形式: `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`
#[derive(Debug, serde::Serialize)]
struct StreamInputMessage {
    #[serde(rename = "type")]
    msg_type: String,
    message: StreamInputMessagePayload,
}

impl StreamInputMessage {
    /// stream-json入力用のユーザーメッセージを作成
    fn user(text: &str) -> Self {
        Self {
            msg_type: "user".to_string(),
            message: StreamInputMessagePayload {
                role: "user".to_string(),
                content: vec![StreamInputContent {
                    content_type: "text".to_string(),
                    text: text.to_string(),
                }],
            },
        }
    }
}

/// インタラクティブプロセスの内部状態
struct InteractiveProcess {
    child: Child,
    stdin: tokio::process::ChildStdin,
}

/// インタラクティブClaudeエージェント - 持続的なClaude CLIプロセスを維持
///
/// ClaudeAgent (OneShotモード) と異なり、プロセスを生存させ
/// stdin JSON入力経由で複数の会話ターンを可能にする
pub struct InteractiveClaudeAgent {
    config: AgentConfig,
    process: Arc<Mutex<Option<InteractiveProcess>>>,
    event_tx: mpsc::Sender<AgentEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<AgentEvent>>>,
}

impl InteractiveClaudeAgent {
    /// 指定設定で新しいインタラクティブエージェントを作成
    pub fn new(mut config: AgentConfig) -> Self {
        config.mode = AgentMode::Interactive;
        let (tx, rx) = mpsc::channel(100);
        Self {
            config,
            process: Arc::new(Mutex::new(None)),
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Claude CLIプロセスを起動
    pub async fn start(&self) -> anyhow::Result<()> {
        let mut process_guard = self.process.lock().await;
        if process_guard.is_some() {
            return Ok(()); // 既に実行中
        }

        let mut cmd = Command::new("claude");

        // 双方向stream-jsonでprintモードを使用
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json");

        // stream-jsonにはverboseが必須
        cmd.arg("--verbose");

        // 共通CLIオプションを適用
        apply_cli_args(&mut cmd, &self.config);

        // stdioを設定
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::info!(
            "Interactive Claude CLI起動 (session: {:?}, mcp_config: {:?})",
            self.config.session_id.as_deref().unwrap_or("new"),
            self.config.mcp_config.as_deref().unwrap_or("default")
        );

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!(
                "Claude CLIの起動に失敗。'claude'がインストールされPATHにあるか確認: {}",
                e
            )
        })?;

        let stdin = child.stdin.take().expect("stdinがキャプチャされていない");
        let stdout = child.stdout.take().expect("stdoutがキャプチャされていない");
        let stderr = child.stderr.take().expect("stderrがキャプチャされていない");

        // stdout読み取りタスクを開始
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut last_text = String::new();
            let mut session_id_sent = false;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

                match serde_json::from_str::<ClaudeMessage>(&line) {
                    Ok(msg) => match msg {
                        ClaudeMessage::System {
                            session_id,
                            subtype,
                            model,
                            tools,
                            mcp_servers,
                        } => {
                            if !session_id_sent && subtype.as_deref() == Some("init") {
                                let mcp_names: Vec<String> = mcp_servers
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter(|s| s.status == "connected")
                                    .map(|s| s.name)
                                    .collect();

                                let tools_list = tools.unwrap_or_default();
                                tracing::info!(
                                    "Interactive Claude CLI初期化: session={}, model={:?}",
                                    session_id,
                                    model
                                );

                                let _ = tx
                                    .send(AgentEvent::SessionInit {
                                        session_id: session_id.clone(),
                                        model,
                                        tools: tools_list,
                                        mcp_servers: mcp_names,
                                    })
                                    .await;
                                session_id_sent = true;
                            }
                        }
                        ClaudeMessage::Assistant { message, .. } => {
                            for block in message.content {
                                match block {
                                    ContentBlock::Text { text } => {
                                        if text.len() > last_text.len()
                                            && text.starts_with(&last_text)
                                        {
                                            let new_text = &text[last_text.len()..];
                                            let _ = tx
                                                .send(AgentEvent::TextChunk(new_text.to_string()))
                                                .await;
                                        } else if text != last_text {
                                            let _ =
                                                tx.send(AgentEvent::TextChunk(text.clone())).await;
                                        }
                                        last_text = text;
                                    }
                                    ContentBlock::ToolUse { name, .. } => {
                                        let _ = tx.send(AgentEvent::ToolExecuting { name }).await;
                                    }
                                    ContentBlock::ToolResult { .. } | ContentBlock::Other => {}
                                }
                            }
                        }
                        ClaudeMessage::Result {
                            result,
                            is_error,
                            total_cost_usd,
                            ..
                        } => {
                            // 次のターン用にlast_textをリセット
                            last_text.clear();

                            if is_error {
                                let _ =
                                    tx.send(AgentEvent::Error(result.unwrap_or_default())).await;
                            } else {
                                let _ = tx
                                    .send(AgentEvent::Done {
                                        result: result.unwrap_or_default(),
                                        cost: total_cost_usd,
                                    })
                                    .await;
                            }
                        }
                    },
                    Err(e) => {
                        tracing::debug!("Claudeメッセージ解析失敗: {} - line: {}", e, line);
                    }
                }
            }
        });

        // stderr読み取りタスクを開始
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("Interactive Claude CLI stderr: {}", line);
            }
        });

        *process_guard = Some(InteractiveProcess { child, stdin });

        Ok(())
    }

    /// Claude CLIプロセスにメッセージを送信
    pub async fn send(&self, message: &str) -> anyhow::Result<()> {
        let mut process_guard = self.process.lock().await;
        let process = process_guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("プロセス未起動。先にstart()を呼び出してください。"))?;

        let input = StreamInputMessage::user(message);
        let json = serde_json::to_string(&input)?;

        tracing::debug!("Claude CLIに送信: {}", json);

        process.stdin.write_all(json.as_bytes()).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

        Ok(())
    }

    /// レスポンスをリッスンするためのイベントレシーバーを取得
    pub fn events(&self) -> Arc<Mutex<mpsc::Receiver<AgentEvent>>> {
        self.event_rx.clone()
    }

    /// プロセスが実行中かどうかを確認
    pub async fn is_running(&self) -> bool {
        self.process.lock().await.is_some()
    }

    /// Claude CLIプロセスを停止
    pub async fn stop(&self) -> anyhow::Result<()> {
        let mut process_guard = self.process.lock().await;
        if let Some(mut process) = process_guard.take() {
            process.child.kill().await?;
        }
        Ok(())
    }
}

// =============================================================================
// PTYモード実装 - 真の対話モード
// =============================================================================

use tokio::io::AsyncReadExt;

/// PTYパーミッションプロンプト検出情報
#[derive(Debug, Clone)]
pub struct PtyPermissionPrompt {
    /// ツール名（例: "Bash", "Edit"）
    pub tool_name: String,
    /// 説明/コマンド内容
    pub description: String,
    /// 生のプロンプトテキスト
    pub raw_prompt: String,
}

impl PtyPermissionPrompt {
    /// PTY出力からパーミッションプロンプトを検出
    ///
    /// Claude CLIのパーミッションプロンプトパターンを検出:
    /// - "Allow once (y)" パターン
    /// - "Allow for this session (a)" パターン
    /// - "Reject (n)" パターン
    ///
    /// 注意: 誤検出を防ぐため、厳格なパターンマッチングを使用
    pub fn detect(output: &str) -> Option<Self> {
        // ANSIエスケープシーケンスを除去して検出しやすくする
        let clean = strip_ansi_escapes(output);

        // 厳格なパターン: Claude CLIパーミッションプロンプトの特徴的な選択肢
        // 実際のプロンプト例:
        // ╭─────────────────────────────────────────────────────────────╮
        // │ Claude wants to run: Bash(...)                              │
        // ├─────────────────────────────────────────────────────────────┤
        // │ Allow once (y)                                              │
        // │ Allow for this session (a)                                  │
        // │ Reject (n)                                                  │
        // ╰─────────────────────────────────────────────────────────────╯

        // パターン1: "Allow once" と "(y)" を含む（最も確実なパターン）
        if clean.contains("Allow once") && clean.contains("(y)") {
            let (tool, desc) = extract_tool_info(&clean);
            return Some(Self {
                tool_name: tool,
                description: desc,
                raw_prompt: output.to_string(),
            });
        }

        // パターン2: "Allow for this session" と "(a)" を含む
        if clean.contains("Allow for this session") && clean.contains("(a)") {
            let (tool, desc) = extract_tool_info(&clean);
            return Some(Self {
                tool_name: tool,
                description: desc,
                raw_prompt: output.to_string(),
            });
        }

        // パターン3: "Reject (n)" と "Allow" の両方を含む
        if clean.contains("Reject (n)") && clean.contains("Allow") {
            let (tool, desc) = extract_tool_info(&clean);
            return Some(Self {
                tool_name: tool,
                description: desc,
                raw_prompt: output.to_string(),
            });
        }

        // パターン4: 3つの選択肢 (y), (a), (n) がすべて含まれる
        if clean.contains("(y)") && clean.contains("(a)") && clean.contains("(n)") {
            let (tool, desc) = extract_tool_info(&clean);
            return Some(Self {
                tool_name: tool,
                description: desc,
                raw_prompt: output.to_string(),
            });
        }

        None
    }
}

/// ANSIエスケープシーケンスを除去
fn strip_ansi_escapes(s: &str) -> String {
    // 簡易的なANSIシーケンス除去（\x1b[...m パターン）
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // エスケープシーケンスをスキップ
            if chars.peek() == Some(&'[') {
                chars.next(); // '['を消費
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch.is_alphabetic() {
                        break; // 終端文字に到達
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// ツール名と説明を抽出（ヒューリスティック）
fn extract_tool_info(text: &str) -> (String, String) {
    // "execute" や "run" の後のワードをツール名として抽出
    let tool_patterns = [
        ("execute", " "),
        ("run", " "),
        ("Bash", ":"),
        ("Edit", ":"),
        ("Read", ":"),
        ("Write", ":"),
    ];

    let mut tool_name = "Unknown".to_string();
    let mut description = text.to_string();

    for (pattern, delim) in tool_patterns {
        if let Some(pos) = text.find(pattern) {
            // パターンの後の部分を取得
            let after = &text[pos..];
            if let Some(end) = after.find(delim) {
                let extracted = after[..end].trim().to_string();
                if !extracted.is_empty() && extracted.len() < 50 {
                    tool_name = extracted;
                    break;
                }
            } else {
                // デリミタが見つからない場合、最初の50文字まで
                tool_name = after
                    .chars()
                    .take(50)
                    .collect::<String>()
                    .trim()
                    .to_string();
                break;
            }
        }
    }

    // 説明として"Command:"や"Arguments:"の後の内容を抽出
    if let Some(cmd_pos) = text.find("Command:") {
        let cmd_text = &text[cmd_pos + 8..];
        if let Some(end) = cmd_text.find('\n') {
            description = cmd_text[..end].trim().to_string();
        } else {
            description = cmd_text.trim().to_string();
        }
    }

    (tool_name, description)
}

pub enum PtyEvent {
    /// PTYからの生出力（端末データ）
    Output(String),
    /// パーミッションプロンプト検出
    PermissionPrompt(PtyPermissionPrompt),
    /// プロセス終了
    Exited(i32),
    /// エラー発生
    Error(String),
}

/// PTYベースのClaudeエージェント - 端末エミュレーションによる真の対話モード
///
/// 疑似端末（PTY）付きで `claude`（-pなし）を起動し、
/// 完全な対話体験を実現。以下の用途に有用:
/// - Multiplexer Orchestration（tmuxライクなプロセス管理）
/// - 完全な端末UIキャプチャ
/// - 対話的なプロンプトと確認
///
/// stream-jsonモードと異なり、PTYモードは生の端末I/Oを提供
pub struct PtyClaudeAgent {
    config: AgentConfig,
    /// PTY書き込みハンドル
    pty_writer: Arc<Mutex<Option<pty_process::OwnedWritePty>>>,
    event_tx: mpsc::Sender<PtyEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<PtyEvent>>>,
    /// 子プロセス（tokio::process::Child）
    child: Arc<Mutex<Option<tokio::process::Child>>>,
    /// PTY出力のログファイルパス（tee方式で出力を記録）
    log_path: Option<std::path::PathBuf>,
}

impl PtyClaudeAgent {
    /// 新しいPTYベースエージェントを作成
    pub fn new(mut config: AgentConfig) -> Self {
        config.mode = AgentMode::Pty;
        let (tx, rx) = mpsc::channel(100);
        Self {
            config,
            pty_writer: Arc::new(Mutex::new(None)),
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(rx)),
            child: Arc::new(Mutex::new(None)),
            log_path: None,
        }
    }

    /// ログファイルパスを指定してPTYエージェントを作成
    pub fn with_log_path(mut config: AgentConfig, log_path: std::path::PathBuf) -> Self {
        config.mode = AgentMode::Pty;
        let (tx, rx) = mpsc::channel(100);
        Self {
            config,
            pty_writer: Arc::new(Mutex::new(None)),
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(rx)),
            child: Arc::new(Mutex::new(None)),
            log_path: Some(log_path),
        }
    }

    /// PTY付きで真の対話モードでClaude CLIを起動
    pub async fn start(&self) -> anyhow::Result<()> {
        let mut writer_guard = self.pty_writer.lock().await;
        if writer_guard.is_some() {
            return Ok(()); // 既に実行中
        }

        // PTYを作成（マスターとスレーブを取得）
        let (mut pty, pts) =
            pty_process::open().map_err(|e| anyhow::anyhow!("PTY作成失敗: {}", e))?;

        // 適切な端末サイズにリサイズ
        pty.resize(pty_process::Size::new(24, 120))
            .map_err(|e| anyhow::anyhow!("PTYリサイズ失敗: {}", e))?;

        // コマンドを構築（pty_process::Commandはselfを消費するので再代入が必要）
        let mut cmd = pty_process::Command::new("claude");

        // CLIオプションを適用（-pはPTYモードでは使用しない）
        // PTYモードでは意味のあるフラグのサブセットのみを適用
        if self.config.skip_permissions {
            cmd = cmd.arg("--dangerously-skip-permissions");
        }

        if let Some(ref model) = self.config.model {
            cmd = cmd.arg("--model").arg(model);
        }

        if let Some(ref dir) = self.config.working_dir {
            cmd = cmd.current_dir(dir);
        }

        // 注意: セッション制御フラグ（--resume, --continue）は対話モードでも動作
        if self.config.use_continue {
            cmd = cmd.arg("--continue");
        } else if let Some(ref session_id) = self.config.session_id {
            cmd = cmd.arg("--resume").arg(session_id);
        }

        tracing::info!(
            "PTY Claude CLI起動 (session: {:?}, working_dir: {:?})",
            self.config.session_id.as_deref().unwrap_or("new"),
            self.config.working_dir.as_deref().unwrap_or(".")
        );

        // PTY付きで起動（tokio::process::Childを返す）
        let child: tokio::process::Child = cmd
            .spawn(pts)
            .map_err(|e| anyhow::anyhow!("PTY付きClaude CLI起動失敗: {}", e))?;

        // PTYを読み取り/書き込みに分割
        let (pty_read, pty_write) = pty.into_split();

        // PTYからの読み取りタスクを開始（ログ付きtee方式）
        let tx = self.event_tx.clone();
        let log_path = self.log_path.clone();
        tokio::spawn(async move {
            let mut pty_reader = pty_read;
            let mut buf = [0u8; 4096];

            // ログファイルを開く（指定されている場合）
            let mut log_file = if let Some(ref path) = log_path {
                // 親ディレクトリを作成
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .await
                {
                    Ok(f) => {
                        tracing::info!("PTYログファイル: {:?}", path);
                        Some(f)
                    }
                    Err(e) => {
                        tracing::warn!("PTYログファイル作成失敗: {} - {:?}", e, path);
                        None
                    }
                }
            } else {
                None
            };

            loop {
                match pty_reader.read(&mut buf).await {
                    Ok(0) => {
                        // EOF - プロセスが終了した可能性
                        break;
                    }
                    Ok(n) => {
                        // 文字列に変換（UTF-8を適切に処理）
                        let output = String::from_utf8_lossy(&buf[..n]).to_string();

                        // ログファイルに書き込み（tee方式）
                        if let Some(ref mut file) = log_file {
                            let _ = file.write_all(buf[..n].as_ref()).await;
                            let _ = file.flush().await;
                        }

                        // パーミッションプロンプトを検出
                        if let Some(prompt) = PtyPermissionPrompt::detect(&output) {
                            tracing::info!("Permission prompt detected: {:?}", prompt.tool_name);
                            // パーミッションイベントを先に送信
                            if tx.send(PtyEvent::PermissionPrompt(prompt)).await.is_err() {
                                break;
                            }
                        }

                        // 生出力も常に送信
                        if tx.send(PtyEvent::Output(output)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(PtyEvent::Error(format!("PTY読み取りエラー: {}", e)))
                            .await;
                        break;
                    }
                }
            }
        });

        // 書き込みハンドルと子プロセスを保存
        *writer_guard = Some(pty_write);
        *self.child.lock().await = Some(child);

        Ok(())
    }

    /// Claude CLIプロセスに入力を送信
    pub async fn send(&self, input: &str) -> anyhow::Result<()> {
        let mut writer_guard = self.pty_writer.lock().await;
        let writer = writer_guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("PTY未起動。先にstart()を呼び出してください。"))?;

        // PTYに入力を書き込み（改行がない場合は追加）
        let input_with_newline = if input.ends_with('\n') {
            input.to_string()
        } else {
            format!("{}\n", input)
        };

        writer
            .write_all(input_with_newline.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("PTY書き込み失敗: {}", e))?;

        writer
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("PTYフラッシュ失敗: {}", e))?;

        Ok(())
    }

    /// PTYに生バイトを送信（制御シーケンス等用）
    pub async fn send_raw(&self, data: &[u8]) -> anyhow::Result<()> {
        let mut writer_guard = self.pty_writer.lock().await;
        let writer = writer_guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("PTY未起動。先にstart()を呼び出してください。"))?;

        writer
            .write_all(data)
            .await
            .map_err(|e| anyhow::anyhow!("PTY書き込み失敗: {}", e))?;

        writer
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("PTYフラッシュ失敗: {}", e))?;

        Ok(())
    }

    /// 現在の操作を中断するためにCtrl+Cを送信
    pub async fn interrupt(&self) -> anyhow::Result<()> {
        self.send_raw(&[0x03]).await // ETX (Ctrl+C)
    }

    /// EOFを送信するためにCtrl+Dを送信
    pub async fn send_eof(&self) -> anyhow::Result<()> {
        self.send_raw(&[0x04]).await // EOT (Ctrl+D)
    }

    /// PTY出力をリッスンするためのイベントレシーバーを取得
    pub fn events(&self) -> Arc<Mutex<mpsc::Receiver<PtyEvent>>> {
        self.event_rx.clone()
    }

    /// プロセスが実行中かどうかを確認
    pub async fn is_running(&self) -> bool {
        let child_guard = self.child.lock().await;
        if let Some(ref child) = *child_guard {
            // プロセスが生存しているか確認
            child.id().is_some()
        } else {
            false
        }
    }

    /// プロセスの終了を待機
    pub async fn wait(&self) -> anyhow::Result<i32> {
        let mut child_guard = self.child.lock().await;
        if let Some(ref mut child) = *child_guard {
            let status = child
                .wait()
                .await
                .map_err(|e| anyhow::anyhow!("子プロセス待機失敗: {}", e))?;

            let exit_code = status.code().unwrap_or(-1);

            let _ = self.event_tx.send(PtyEvent::Exited(exit_code)).await;

            Ok(exit_code)
        } else {
            Err(anyhow::anyhow!("子プロセスなし"))
        }
    }

    /// Claude CLIプロセスを停止
    pub async fn stop(&self) -> anyhow::Result<()> {
        // まずCtrl+Cを送信してみる
        if let Err(_) = self.interrupt().await {
            // 失敗した場合は直接killを試みる
        }

        // クリーンアップのため少し待機
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // まだ実行中ならkill
        let mut child_guard = self.child.lock().await;
        if let Some(ref mut child) = *child_guard {
            let _ = child.kill();
        }

        // クリーンアップ
        *child_guard = None;
        *self.pty_writer.lock().await = None;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires claude CLI to be installed
    async fn test_claude_agent_with_session() {
        let agent = ClaudeAgent::new();
        let mut rx = agent.chat("Say hello").await;

        let mut session_id = None;
        let mut output = String::new();
        let mut tools_count = 0;
        let mut mcp_count = 0;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::SessionInit {
                    session_id: sid,
                    model,
                    tools,
                    mcp_servers,
                } => {
                    println!("Session ID: {}", sid);
                    println!("Model: {:?}", model);
                    println!("Tools: {} ({:?})", tools.len(), tools);
                    println!("MCP Servers: {} ({:?})", mcp_servers.len(), mcp_servers);
                    session_id = Some(sid);
                    tools_count = tools.len();
                    mcp_count = mcp_servers.len();
                }
                AgentEvent::TextChunk(chunk) => {
                    output.push_str(&chunk);
                }
                AgentEvent::ToolExecuting { name } => {
                    println!("Tool executing: {}", name);
                }
                AgentEvent::ToolResult { name, preview } => {
                    println!("Tool result: {} - {}", name, preview);
                }
                AgentEvent::Done { result, cost } => {
                    println!("Done! Result: {}, Cost: {:?}", result, cost);
                    break;
                }
                AgentEvent::Error(e) => {
                    panic!("Error: {}", e);
                }
            }
        }

        assert!(session_id.is_some());
        println!("Output: {}", output);
        println!("Tools: {}, MCP: {}", tools_count, mcp_count);
    }

    #[tokio::test]
    #[ignore] // Requires claude CLI to be installed
    async fn test_interactive_claude_agent() {
        let config = AgentConfig {
            mode: AgentMode::Interactive,
            ..Default::default()
        };
        let agent = InteractiveClaudeAgent::new(config);

        // Start the process
        agent.start().await.expect("Failed to start agent");
        assert!(agent.is_running().await);

        // Send first message
        agent.send("Say 'Hello'").await.expect("Failed to send");

        // Receive events
        let events = agent.events();
        let mut rx = events.lock().await;
        let mut got_init = false;
        let mut got_done = false;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::SessionInit { session_id, .. } => {
                    println!("Interactive session: {}", session_id);
                    got_init = true;
                }
                AgentEvent::TextChunk(chunk) => {
                    print!("{}", chunk);
                }
                AgentEvent::Done { .. } => {
                    println!("\n[Turn 1 done]");
                    got_done = true;
                    break;
                }
                AgentEvent::Error(e) => {
                    panic!("Error: {}", e);
                }
                _ => {}
            }
        }

        assert!(got_init);
        assert!(got_done);

        // Send second message (same session)
        drop(rx);
        agent
            .send("Now say 'Goodbye'")
            .await
            .expect("Failed to send second message");

        let events = agent.events();
        let mut rx = events.lock().await;
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::TextChunk(chunk) => {
                    print!("{}", chunk);
                }
                AgentEvent::Done { .. } => {
                    println!("\n[Turn 2 done]");
                    break;
                }
                AgentEvent::Error(e) => {
                    panic!("Error in turn 2: {}", e);
                }
                _ => {}
            }
        }

        // Stop the agent
        agent.stop().await.expect("Failed to stop");
        assert!(!agent.is_running().await);
    }
}
