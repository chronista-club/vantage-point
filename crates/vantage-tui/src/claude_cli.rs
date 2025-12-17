//! Claude CLI子プロセス管理
//!
//! `claude -p --output-format stream-json --verbose` を実行し、
//! 行区切りJSONをパースしてTUIに統合する

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

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

/// Claude CLI子プロセス
pub struct ClaudeCli {
    process: Option<Child>,
}

impl ClaudeCli {
    /// 新しいClaude CLIインスタンスを作成
    pub fn new() -> Self {
        Self {
            process: None,
        }
    }

    /// プロンプトを送信してイベントを受信
    pub fn send_prompt(&mut self, prompt: &str, cwd: Option<&str>) -> Result<Receiver<ClaudeEvent>> {
        let (tx, rx) = mpsc::channel();

        // Build command
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().context("Failed to spawn claude CLI")?;

        let stdout = child.stdout.take().context("Failed to get stdout")?;

        // Spawn thread to read stdout
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) if !line.is_empty() => {
                        if let Some(event) = parse_line(&line) {
                            if tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(ClaudeEvent::Error(format!("Read error: {}", e)));
                        break;
                    }
                    _ => {}
                }
            }
        });

        self.process = Some(child);

        Ok(rx)
    }

    /// プロセスを終了
    pub fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
        }
    }
}

impl Drop for ClaudeCli {
    fn drop(&mut self) {
        self.kill();
    }
}

/// JSON行をパースしてイベントに変換
fn parse_line(line: &str) -> Option<ClaudeEvent> {
    let msg: ClaudeMessage = serde_json::from_str(line).ok()?;

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
