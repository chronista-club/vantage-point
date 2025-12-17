//! Claude CLI子プロセス管理 (tokio版)
//!
//! `claude -p --output-format stream-json --verbose` を実行し、
//! 行区切りJSONをパースしてTUIに統合する

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::process::Stdio;
use std::time::SystemTime;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// ログファイルパス
const LOG_FILE: &str = "/tmp/vantage-tui-claude.log";

/// スレッドセーフなロガー
pub fn log_to_file(message: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
    {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "[{}] {}", timestamp, message);
    }
}

/// Claude CLI出力のメッセージ型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeMessage {
    /// システム初期化メッセージ
    #[serde(rename = "system")]
    System(SystemMessage),

    /// アシスタントメッセージ
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),

    /// 結果メッセージ
    #[serde(rename = "result")]
    Result(ResultMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessage {
    pub subtype: String,
    pub cwd: Option<String>,
    pub session_id: String,
    pub tools: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<McpServer>>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub message: ApiMessage,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub model: String,
    pub id: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: serde_json::Value },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultMessage {
    pub subtype: String,
    pub result: Option<String>,
    pub session_id: String,
    pub total_cost_usd: Option<f64>,
    pub num_turns: Option<i32>,
    pub is_error: bool,
}

/// TUIに送るイベント
#[derive(Debug, Clone)]
pub enum ClaudeEvent {
    /// 初期化完了
    Init { model: String, tools: Vec<String>, mcp_servers: Vec<String> },
    /// テキスト出力
    Text(String),
    /// ツール実行開始
    ToolExecuting { name: String },
    /// ツール実行結果
    ToolResult { name: String, preview: String },
    /// 完了
    Done { result: String, cost: f64 },
    /// エラー
    Error(String),
}

/// Claude CLI子プロセス (tokio版)
pub struct ClaudeCli {
    process: Option<Child>,
    event_rx: Option<mpsc::Receiver<ClaudeEvent>>,
}

impl ClaudeCli {
    /// 新しいClaude CLIインスタンスを作成
    pub fn new() -> Self {
        Self {
            process: None,
            event_rx: None,
        }
    }

    /// プロンプトを送信（非同期）
    pub async fn send_prompt(&mut self, prompt: &str, cwd: Option<&str>) -> Result<()> {
        log_to_file("=== NEW PROMPT ===");
        log_to_file(&format!("Prompt: {}", prompt));
        log_to_file(&format!("CWD: {:?}", cwd));

        // Kill any existing process first
        self.kill().await;

        let (tx, rx) = mpsc::channel(100);
        self.event_rx = Some(rx);

        // Build command
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().context("Failed to spawn claude CLI")?;

        let stdout = child.stdout.take().context("Failed to get stdout")?;
        let stderr = child.stderr.take().context("Failed to get stderr")?;

        self.process = Some(child);

        // Spawn task to read stderr
        let tx_stderr = tx.clone();
        tokio::spawn(async move {
            log_to_file("STDERR task started");
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.is_empty() {
                    log_to_file(&format!("STDERR: {}", line));
                    let _ = tx_stderr.send(ClaudeEvent::Error(format!("stderr: {}", line))).await;
                }
            }
            log_to_file("STDERR task ended");
        });

        // Spawn task to read stdout
        tokio::spawn(async move {
            log_to_file("STDOUT task started");
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) if !line.is_empty() => {
                        let preview: String = line.chars().take(200).collect();
                        log_to_file(&format!("STDOUT RAW: {}", preview));
                        if let Some(event) = parse_line(&line) {
                            log_to_file(&format!("PARSED EVENT: {:?}", event));
                            if tx.send(event).await.is_err() {
                                log_to_file("SEND ERROR: channel closed");
                                break;
                            }
                        } else {
                            log_to_file("PARSE RETURNED None");
                        }
                    }
                    Ok(Some(_)) => {
                        // Empty line, continue
                    }
                    Ok(None) => {
                        log_to_file("STDOUT EOF");
                        break;
                    }
                    Err(e) => {
                        log_to_file(&format!("STDOUT READ ERROR: {}", e));
                        let _ = tx.send(ClaudeEvent::Error(format!("Read error: {}", e))).await;
                        break;
                    }
                }
            }
            log_to_file("STDOUT task ended");
        });

        Ok(())
    }

    /// イベントを非ブロッキングで受信
    pub fn try_recv(&mut self) -> Option<ClaudeEvent> {
        if let Some(ref mut rx) = self.event_rx {
            match rx.try_recv() {
                Ok(event) => Some(event),
                Err(mpsc::error::TryRecvError::Empty) => None,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.event_rx = None;
                    None
                }
            }
        } else {
            None
        }
    }

    /// 全てのイベントを非ブロッキングで収集
    pub fn collect_events(&mut self) -> Vec<ClaudeEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.try_recv() {
            events.push(event);
        }
        events
    }

    /// イベントチャンネルをクリア
    pub fn clear_channel(&mut self) {
        self.event_rx = None;
    }

    /// プロセスを終了
    pub async fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            log_to_file("Killing process");
            let _ = process.kill().await;
        }
        self.event_rx = None;
    }

    /// プロセスが実行中かどうか
    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }
}

impl Drop for ClaudeCli {
    fn drop(&mut self) {
        // Note: async drop not possible, but kill_on_drop handles this
    }
}

/// JSON行をパースしてイベントに変換
fn parse_line(line: &str) -> Option<ClaudeEvent> {
    let msg: ClaudeMessage = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => {
            let preview: String = line.chars().take(100).collect();
            log_to_file(&format!("JSON parse error: {} for line: {}", e, preview));
            return None;
        }
    };

    match msg {
        ClaudeMessage::System(sys) => {
            if sys.subtype == "init" {
                let tools = sys.tools.unwrap_or_default();
                let mcp_servers = sys.mcp_servers
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| s.status == "connected")
                    .map(|s| s.name)
                    .collect();
                return Some(ClaudeEvent::Init {
                    model: sys.model.unwrap_or_else(|| "unknown".to_string()),
                    tools,
                    mcp_servers,
                });
            }
            None
        }
        ClaudeMessage::Assistant(assistant) => {
            let mut text_parts = Vec::new();
            let mut tool_events = Vec::new();

            for block in assistant.message.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(text);
                    }
                    ContentBlock::ToolUse { name, .. } => {
                        tool_events.push(ClaudeEvent::ToolExecuting { name });
                    }
                    ContentBlock::ToolResult { .. } => {
                        // Tool results are usually in subsequent messages
                    }
                }
            }

            // Return text if we have any
            if !text_parts.is_empty() {
                return Some(ClaudeEvent::Text(text_parts.join("\n")));
            }

            // Return first tool event if any
            if !tool_events.is_empty() {
                return tool_events.into_iter().next();
            }

            None
        }
        ClaudeMessage::Result(result) => {
            if result.subtype == "success" {
                return Some(ClaudeEvent::Done {
                    result: result.result.unwrap_or_default(),
                    cost: result.total_cost_usd.unwrap_or(0.0),
                });
            } else if result.is_error {
                return Some(ClaudeEvent::Error("Execution error".to_string()));
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_system_init() {
        let json = r#"{"type":"system","subtype":"init","cwd":"/test","session_id":"abc","tools":["Read","Write"],"mcp_servers":[{"name":"test","status":"connected"}],"model":"claude-opus-4-5-20251101"}"#;
        let event = parse_line(json);
        assert!(matches!(event, Some(ClaudeEvent::Init { .. })));
    }

    #[test]
    fn test_parse_assistant() {
        let json = r#"{"type":"assistant","message":{"model":"claude-opus-4-5-20251101","id":"msg_123","content":[{"type":"text","text":"Hello!"}]},"session_id":"abc"}"#;
        let event = parse_line(json);
        assert!(matches!(event, Some(ClaudeEvent::Text(_))));
    }

    #[test]
    fn test_parse_result() {
        let json = r#"{"type":"result","subtype":"success","result":"Done","session_id":"abc","total_cost_usd":0.01,"num_turns":1,"is_error":false}"#;
        let event = parse_line(json);
        assert!(matches!(event, Some(ClaudeEvent::Done { .. })));
    }
}
