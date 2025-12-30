//! AG-UI Protocol Adapter
//!
//! AgentEvent → AgUiEvent 変換アダプター
//! Claude CLI の出力を AG-UI プロトコル準拠のイベントに変換する。
//!
//! ## 変換マッピング
//! | AgentEvent     | AgUiEvent                                |
//! |----------------|------------------------------------------|
//! | SessionInit    | RunStarted + TextMessageStart            |
//! | TextChunk      | TextMessageContent                       |
//! | ToolExecuting  | ToolCallStart                            |
//! | ToolResult     | ToolCallEnd                              |
//! | Done           | TextMessageEnd + RunFinished             |
//! | Error          | RunError                                 |

use std::collections::HashMap;

use crate::agent::AgentEvent;
use crate::agui::{AgUiError, AgUiEvent, MessageRole};

/// Claude CLI → AG-UI アダプター
///
/// ステートフルなアダプターで、run_id や message_id を追跡する。
#[derive(Debug, Default)]
pub struct ClaudeAgUiAdapter {
    /// 現在の run_id
    run_id: Option<String>,
    /// 現在の message_id（テキストストリーム用）
    current_message_id: Option<String>,
    /// 進行中のツールコール（tool_call_id → tool_name）
    pending_tool_calls: HashMap<String, String>,
    /// メッセージカウンター（ユニークID生成用）
    message_counter: u64,
    /// ツールコールカウンター（ユニークID生成用）
    tool_call_counter: u64,
    /// テキストメッセージが開始済みか
    text_message_started: bool,
}

impl ClaudeAgUiAdapter {
    /// 新しいアダプターを作成
    pub fn new() -> Self {
        Self::default()
    }

    /// run_id を指定してアダプターを作成
    pub fn with_run_id(run_id: impl Into<String>) -> Self {
        Self {
            run_id: Some(run_id.into()),
            ..Default::default()
        }
    }

    /// 現在の run_id を取得
    pub fn run_id(&self) -> Option<&str> {
        self.run_id.as_deref()
    }

    /// 新しいメッセージIDを生成
    fn next_message_id(&mut self) -> String {
        self.message_counter += 1;
        format!("msg-{}", self.message_counter)
    }

    /// 新しいツールコールIDを生成
    fn next_tool_call_id(&mut self) -> String {
        self.tool_call_counter += 1;
        format!("tool-{}", self.tool_call_counter)
    }

    /// AgentEvent を AgUiEvent に変換
    ///
    /// 1つのAgentEventが複数のAgUiEventを生成する可能性がある
    pub fn convert(&mut self, event: AgentEvent) -> Vec<AgUiEvent> {
        match event {
            AgentEvent::SessionInit {
                session_id,
                model: _,
                tools: _,
                mcp_servers: _,
            } => self.handle_session_init(session_id),

            AgentEvent::TextChunk(text) => self.handle_text_chunk(text),

            AgentEvent::ToolExecuting { name } => self.handle_tool_executing(name),

            AgentEvent::ToolResult { name, preview } => self.handle_tool_result(name, preview),

            AgentEvent::Done { result: _, cost: _ } => self.handle_done(),

            AgentEvent::Error(message) => self.handle_error(message),
        }
    }

    /// セッション初期化を処理
    fn handle_session_init(&mut self, session_id: String) -> Vec<AgUiEvent> {
        self.run_id = Some(session_id.clone());
        self.text_message_started = false;

        vec![AgUiEvent::RunStarted {
            run_id: session_id,
            thread_id: None,
            timestamp: now_millis(),
        }]
    }

    /// テキストチャンクを処理
    fn handle_text_chunk(&mut self, text: String) -> Vec<AgUiEvent> {
        let run_id = match &self.run_id {
            Some(id) => id.clone(),
            None => {
                // run_id がない場合は自動生成
                let id = format!("run-{}", uuid_v7());
                self.run_id = Some(id.clone());
                id
            }
        };

        let mut events = Vec::new();

        // テキストメッセージがまだ開始されていない場合は開始
        if !self.text_message_started {
            let message_id = self.next_message_id();
            self.current_message_id = Some(message_id.clone());
            self.text_message_started = true;

            events.push(AgUiEvent::TextMessageStart {
                run_id: run_id.clone(),
                message_id,
                role: MessageRole::Assistant,
                timestamp: now_millis(),
            });
        }

        // テキストコンテンツを追加
        if let Some(message_id) = &self.current_message_id {
            events.push(AgUiEvent::TextMessageContent {
                run_id,
                message_id: message_id.clone(),
                delta: text,
            });
        }

        events
    }

    /// ツール実行開始を処理
    fn handle_tool_executing(&mut self, name: String) -> Vec<AgUiEvent> {
        let run_id = self.run_id.clone().unwrap_or_else(|| "unknown".to_string());
        let tool_call_id = self.next_tool_call_id();

        // 進行中のツールコールを記録
        self.pending_tool_calls
            .insert(tool_call_id.clone(), name.clone());

        // テキストメッセージが進行中の場合は一旦終了
        let mut events = Vec::new();
        if self.text_message_started {
            if let Some(message_id) = &self.current_message_id {
                events.push(AgUiEvent::TextMessageEnd {
                    run_id: run_id.clone(),
                    message_id: message_id.clone(),
                    timestamp: now_millis(),
                });
            }
            self.text_message_started = false;
        }

        events.push(AgUiEvent::ToolCallStart {
            run_id,
            tool_call_id,
            tool_name: name,
            parent_message_id: self.current_message_id.clone(),
            timestamp: now_millis(),
        });

        events
    }

    /// ツール実行結果を処理
    fn handle_tool_result(&mut self, name: String, preview: String) -> Vec<AgUiEvent> {
        let run_id = self.run_id.clone().unwrap_or_else(|| "unknown".to_string());

        // 対応するツールコールIDを検索
        let tool_call_id = self
            .pending_tool_calls
            .iter()
            .find(|(_, n)| **n == name)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| format!("tool-{}", name));

        // 進行中のツールコールから削除
        self.pending_tool_calls.remove(&tool_call_id);

        vec![AgUiEvent::ToolCallEnd {
            run_id,
            tool_call_id,
            result: Some(serde_json::Value::String(preview)),
            error: None,
            timestamp: now_millis(),
        }]
    }

    /// 完了を処理
    fn handle_done(&mut self) -> Vec<AgUiEvent> {
        let run_id = self.run_id.clone().unwrap_or_else(|| "unknown".to_string());
        let mut events = Vec::new();

        // テキストメッセージが進行中の場合は終了
        if self.text_message_started {
            if let Some(message_id) = &self.current_message_id {
                events.push(AgUiEvent::TextMessageEnd {
                    run_id: run_id.clone(),
                    message_id: message_id.clone(),
                    timestamp: now_millis(),
                });
            }
            self.text_message_started = false;
        }

        // Run終了
        events.push(AgUiEvent::RunFinished {
            run_id,
            timestamp: now_millis(),
        });

        // 状態をリセット
        self.reset();

        events
    }

    /// エラーを処理
    fn handle_error(&mut self, message: String) -> Vec<AgUiEvent> {
        let run_id = self.run_id.clone().unwrap_or_else(|| "unknown".to_string());
        let mut events = Vec::new();

        // テキストメッセージが進行中の場合は終了
        if self.text_message_started {
            if let Some(message_id) = &self.current_message_id {
                events.push(AgUiEvent::TextMessageEnd {
                    run_id: run_id.clone(),
                    message_id: message_id.clone(),
                    timestamp: now_millis(),
                });
            }
            self.text_message_started = false;
        }

        events.push(AgUiEvent::RunError {
            run_id,
            error: AgUiError {
                code: "AGENT_ERROR".to_string(),
                message,
                details: None,
            },
            timestamp: now_millis(),
        });

        // 状態をリセット
        self.reset();

        events
    }

    /// 状態をリセット
    pub fn reset(&mut self) {
        self.run_id = None;
        self.current_message_id = None;
        self.pending_tool_calls.clear();
        self.text_message_started = false;
        // カウンターはリセットしない（グローバルユニーク性を保つ）
    }
}

/// 現在時刻をミリ秒で取得
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// UUID v7風のID生成（簡易版）
fn uuid_v7() -> String {
    let timestamp = now_millis();
    let random: u32 = rand::random();
    format!("{:x}-{:x}", timestamp, random)
}

// シンプルな乱数生成（std依存のみ）
mod rand {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    pub fn random<T: From<u32>>() -> T {
        let mut hasher = DefaultHasher::new();
        SystemTime::now().hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        T::from((hasher.finish() & 0xFFFFFFFF) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_init_converts_to_run_started() {
        let mut adapter = ClaudeAgUiAdapter::new();
        let events = adapter.convert(AgentEvent::SessionInit {
            session_id: "test-session".to_string(),
            model: Some("claude-opus-4-5-20251101".to_string()),
            tools: vec![],
            mcp_servers: vec![],
        });

        assert_eq!(events.len(), 1);
        match &events[0] {
            AgUiEvent::RunStarted { run_id, .. } => {
                assert_eq!(run_id, "test-session");
            }
            _ => panic!("Expected RunStarted event"),
        }
    }

    #[test]
    fn test_text_chunk_flow() {
        let mut adapter = ClaudeAgUiAdapter::with_run_id("run-1");

        // 最初のテキストチャンクで TextMessageStart + TextMessageContent
        let events = adapter.convert(AgentEvent::TextChunk("Hello".to_string()));
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgUiEvent::TextMessageStart { .. }));
        assert!(matches!(&events[1], AgUiEvent::TextMessageContent { delta, .. } if delta == "Hello"));

        // 2回目のテキストチャンクは TextMessageContent のみ
        let events = adapter.convert(AgentEvent::TextChunk(" World".to_string()));
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], AgUiEvent::TextMessageContent { delta, .. } if delta == " World")
        );
    }

    #[test]
    fn test_tool_execution_flow() {
        let mut adapter = ClaudeAgUiAdapter::with_run_id("run-1");

        // ツール実行開始
        let events = adapter.convert(AgentEvent::ToolExecuting {
            name: "read_file".to_string(),
        });
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgUiEvent::ToolCallStart { tool_name, .. } => {
                assert_eq!(tool_name, "read_file");
            }
            _ => panic!("Expected ToolCallStart event"),
        }

        // ツール実行結果
        let events = adapter.convert(AgentEvent::ToolResult {
            name: "read_file".to_string(),
            preview: "file content...".to_string(),
        });
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AgUiEvent::ToolCallEnd { .. }));
    }

    #[test]
    fn test_done_closes_text_message() {
        let mut adapter = ClaudeAgUiAdapter::with_run_id("run-1");

        // テキストメッセージを開始
        adapter.convert(AgentEvent::TextChunk("Hello".to_string()));

        // Done で TextMessageEnd + RunFinished
        let events = adapter.convert(AgentEvent::Done {
            result: "completed".to_string(),
            cost: Some(0.01),
        });
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], AgUiEvent::TextMessageEnd { .. }));
        assert!(matches!(&events[1], AgUiEvent::RunFinished { .. }));
    }

    #[test]
    fn test_error_generates_run_error() {
        let mut adapter = ClaudeAgUiAdapter::with_run_id("run-1");

        let events = adapter.convert(AgentEvent::Error("Something went wrong".to_string()));
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgUiEvent::RunError { error, .. } => {
                assert_eq!(error.message, "Something went wrong");
            }
            _ => panic!("Expected RunError event"),
        }
    }
}
