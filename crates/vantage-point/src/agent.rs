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

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

/// Claude CLIのパスを取得（設定値 or デフォルトパス）
fn get_claude_cli_path(config_path: Option<&str>) -> String {
    // 設定で指定されていればそれを使用
    if let Some(path) = config_path {
        return path.to_string();
    }

    // ~/.local/bin/claude を優先チェック（npm -g インストール先）
    if let Ok(home) = std::env::var("HOME") {
        let local_bin = format!("{}/.local/bin/claude", home);
        if std::path::Path::new(&local_bin).exists() {
            return local_bin;
        }
    }

    // デフォルトはPATHから検索
    "claude".to_string()
}

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
    /// ユーザー入力リクエスト（AskUserQuestion等）
    /// UIでユーザーに質問を表示し、回答をAgentに返す必要がある
    UserInputRequest {
        /// リクエストID（レスポンス時に使用）
        request_id: String,
        /// リクエストタイプ
        request_type: Option<String>,
        /// 質問内容
        prompt: Option<String>,
        /// 選択肢
        options: Vec<UserInputOptionInfo>,
    },
    /// ストリーム完了（最終結果付き）
    Done { result: String, cost: Option<f64> },
    /// エラー発生
    Error(String),
}

/// ユーザー入力の選択肢情報
#[derive(Debug, Clone)]
pub struct UserInputOptionInfo {
    pub value: String,
    pub label: Option<String>,
    pub description: Option<String>,
}

/// エージェント実行モード
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum AgentMode {
    /// OneShotモード: 単発プロンプト → 応答、プロセス終了
    OneShot,
    /// Interactiveモード: 持続プロセス、stdin JSON経由で複数ターン
    /// `claude -p --input-format stream-json` を使用（デフォルト）
    #[default]
    Interactive,
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

    // === 入出力フォーマット ===
    /// 入力フォーマット (--input-format): "stream-json" で双方向通信
    /// AskUserQuestionなどのインタラクティブツールに対応するために使用
    pub input_format: Option<String>,
    /// 部分メッセージを含める (--include-partial-messages)
    /// ストリーミング中の部分的な応答も受信
    pub include_partial_messages: bool,

    // === モデル設定 ===
    /// フォールバックモデル (--fallback-model)
    /// 主モデルが利用不可の場合に使用
    pub fallback_model: Option<String>,

    // === 追加ディレクトリ ===
    /// 追加読み取りディレクトリ (--add-dir)
    /// 複数指定可能
    pub add_dirs: Vec<String>,

    // === 出力制御 ===
    /// 詳細出力を有効化 (--verbose)
    pub verbose: bool,
    /// デバッグモード有効化、オプションでフィルタ指定 (--debug)
    pub debug: Option<String>,

    // === CLI パス ===
    /// Claude CLIのフルパス（mise/asdf等のGUI非対応環境用）
    /// 未指定の場合は"claude"でPATHから検索
    pub claude_cli_path: Option<String>,
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
    /// ユーザー入力リクエスト（AskUserQuestion等）
    /// --input-format stream-json 使用時にClaudeが入力を求める
    #[serde(rename = "user_input_request")]
    UserInputRequest {
        /// リクエストID（レスポンス時に使用）
        request_id: String,
        /// リクエストタイプ（"question", "confirmation" 等）
        #[serde(default)]
        request_type: Option<String>,
        /// 質問/プロンプト内容
        #[serde(default)]
        prompt: Option<String>,
        /// 選択肢（選択式の場合）
        #[serde(default)]
        options: Option<Vec<UserInputOption>>,
    },
}

/// ユーザー入力リクエストの選択肢
#[derive(Debug, serde::Deserialize)]
pub struct UserInputOption {
    /// 選択肢のID/値
    pub value: String,
    /// 表示ラベル
    #[serde(default)]
    pub label: Option<String>,
    /// 説明
    #[serde(default)]
    pub description: Option<String>,
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

    // === 入出力フォーマット ===
    if let Some(ref format) = config.input_format {
        cmd.arg("--input-format").arg(format);
    }

    if config.include_partial_messages {
        cmd.arg("--include-partial-messages");
    }

    // === フォールバックモデル ===
    if let Some(ref model) = config.fallback_model {
        cmd.arg("--fallback-model").arg(model);
    }

    // === 追加ディレクトリ ===
    for dir in &config.add_dirs {
        cmd.arg("--add-dir").arg(dir);
    }

    // === 作業ディレクトリ ===
    if let Some(ref dir) = config.working_dir {
        cmd.current_dir(dir);
    }
}

// =============================================================================
// セッションID取得
// =============================================================================

/// ~/.claude.json からプロジェクトのセッションIDを取得
///
/// Claude CLIは各プロジェクトのセッションIDを ~/.claude.json に保存する。
/// この関数はプロジェクトパスに対応するセッションIDを検索する。
///
/// # 戻り値
/// - `Some(session_id)` - セッションが見つかった場合
/// - `None` - セッションが見つからない、またはファイルが存在しない場合
pub fn get_session_id_for_project(project_path: impl AsRef<Path>) -> Option<String> {
    let claude_config_path = get_claude_config_path()?;
    let project_path = project_path.as_ref();

    // ~/.claude.json を読み込み
    let content = std::fs::read_to_string(&claude_config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;

    // projects配列からプロジェクトパスに一致するエントリを検索
    let projects = config.get("projects")?.as_array()?;

    for project in projects {
        let path = project.get("path")?.as_str()?;

        // パスが一致するか確認（正規化して比較）
        let config_path = PathBuf::from(path);
        if paths_match(&config_path, project_path) {
            // セッションIDを取得
            if let Some(session_id) = project.get("sessionId").and_then(|s| s.as_str()) {
                if !session_id.is_empty() {
                    tracing::debug!(
                        "プロジェクト {:?} のセッションID発見: {}",
                        project_path,
                        session_id
                    );
                    return Some(session_id.to_string());
                }
            }
        }
    }

    tracing::debug!(
        "プロジェクト {:?} のセッションIDが見つかりません",
        project_path
    );
    None
}

/// ~/.claude.json のパスを取得
fn get_claude_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

/// 2つのパスが同じディレクトリを指すか確認
fn paths_match(path1: &Path, path2: &Path) -> bool {
    // 両方を正規化して比較
    match (path1.canonicalize(), path2.canonicalize()) {
        (Ok(p1), Ok(p2)) => p1 == p2,
        _ => {
            // 正規化に失敗した場合は文字列比較
            path1.to_string_lossy() == path2.to_string_lossy()
        }
    }
}

/// claude CLIをstream-json出力で実行しレスポンスを解析 (OneShotモード)
///
/// `config.input_format` が "stream-json" の場合、双方向通信モードで動作。
/// AskUserQuestion などのインタラクティブツールに対応可能。
async fn run_claude_cli(
    prompt: &str,
    config: &AgentConfig,
    tx: mpsc::Sender<AgentEvent>,
) -> anyhow::Result<()> {
    // Claude CLIパスを取得（設定 or デフォルト）
    let claude_path = get_claude_cli_path(config.claude_cli_path.as_deref());
    let mut cmd = Command::new(&claude_path);
    // 親プロセスの環境変数を引き継ぐ（mise/asdf等のPATHを含む）
    cmd.envs(std::env::vars());

    // printモードでstream-json出力を使用
    cmd.arg("-p").arg("--output-format").arg("stream-json");

    // stream-jsonにはverboseが必須
    cmd.arg("--verbose");

    // 共通CLIオプションを適用（input-format含む）
    apply_cli_args(&mut cmd, config);

    // プロンプトを追加（最後の位置引数である必要あり）
    cmd.arg(prompt);

    // 双方向通信モードかどうか
    let bidirectional = config.input_format.as_deref() == Some("stream-json");

    // stdioを設定
    if bidirectional {
        cmd.stdin(Stdio::piped());
    }
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
                    ClaudeMessage::UserInputRequest {
                        request_id,
                        request_type,
                        prompt,
                        options,
                    } => {
                        // ユーザー入力リクエストをイベントとして送信
                        tracing::info!(
                            "ユーザー入力リクエスト: request_id={}, type={:?}",
                            request_id,
                            request_type
                        );
                        let options_info = options
                            .unwrap_or_default()
                            .into_iter()
                            .map(|o| UserInputOptionInfo {
                                value: o.value,
                                label: o.label,
                                description: o.description,
                            })
                            .collect();
                        let _ = tx_stdout
                            .send(AgentEvent::UserInputRequest {
                                request_id,
                                request_type,
                                prompt,
                                options: options_info,
                            })
                            .await;
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

/// stream-json入力用 user_input_result メッセージ
///
/// Claude CLI がパーミッション確認などで user_input_request を送信した際の応答
/// 形式: `{"type":"user_input_result","request_id":"...","result":{"type":"confirmation","confirmed":true}}`
#[derive(Debug, serde::Serialize)]
struct UserInputResultMessage {
    #[serde(rename = "type")]
    msg_type: String,
    request_id: String,
    result: UserInputResultPayload,
}

/// user_input_result のペイロード
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UserInputResultPayload {
    /// 確認ダイアログへの応答
    Confirmation { confirmed: bool },
    /// テキスト入力への応答
    Text { value: String },
    /// 選択への応答
    Selection { selected: Vec<String> },
}

impl UserInputResultMessage {
    /// 確認応答（許可/拒否）を作成
    fn confirmation(request_id: &str, confirmed: bool) -> Self {
        Self {
            msg_type: "user_input_result".to_string(),
            request_id: request_id.to_string(),
            result: UserInputResultPayload::Confirmation { confirmed },
        }
    }

    /// テキスト入力応答を作成
    #[allow(dead_code)]
    fn text(request_id: &str, value: &str) -> Self {
        Self {
            msg_type: "user_input_result".to_string(),
            request_id: request_id.to_string(),
            result: UserInputResultPayload::Text {
                value: value.to_string(),
            },
        }
    }

    /// 選択応答を作成
    #[allow(dead_code)]
    fn selection(request_id: &str, selected: Vec<String>) -> Self {
        Self {
            msg_type: "user_input_result".to_string(),
            request_id: request_id.to_string(),
            result: UserInputResultPayload::Selection { selected },
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

        // Claude CLIパスを取得（設定 or デフォルト）
        let claude_path = get_claude_cli_path(self.config.claude_cli_path.as_deref());
        let mut cmd = Command::new(&claude_path);
        // 親プロセスの環境変数を引き継ぐ（mise/asdf等のPATHを含む）
        cmd.envs(std::env::vars());

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
                        ClaudeMessage::UserInputRequest {
                            request_id,
                            request_type,
                            prompt,
                            options,
                        } => {
                            // ユーザー入力リクエストを転送
                            let options_info = options
                                .unwrap_or_default()
                                .into_iter()
                                .map(|o| UserInputOptionInfo {
                                    value: o.value,
                                    label: o.label,
                                    description: o.description,
                                })
                                .collect();
                            let _ = tx
                                .send(AgentEvent::UserInputRequest {
                                    request_id,
                                    request_type,
                                    prompt,
                                    options: options_info,
                                })
                                .await;
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

    /// ユーザー入力リクエストへの確認応答を送信
    ///
    /// Claude CLIが `user_input_request` で確認を求めた場合に使用。
    /// `confirmed: true` で許可、`false` で拒否。
    pub async fn send_user_input_result(
        &self,
        request_id: &str,
        confirmed: bool,
    ) -> anyhow::Result<()> {
        let mut process_guard = self.process.lock().await;
        let process = process_guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("プロセス未起動。先にstart()を呼び出してください。"))?;

        let input = UserInputResultMessage::confirmation(request_id, confirmed);
        let json = serde_json::to_string(&input)?;

        tracing::info!("user_input_result送信: {} -> confirmed={}", request_id, confirmed);
        tracing::debug!("JSON: {}", json);

        process.stdin.write_all(json.as_bytes()).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

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
