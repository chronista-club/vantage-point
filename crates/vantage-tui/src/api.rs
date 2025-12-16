use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::tools::{self, ToolResult, ToolUse};

#[derive(Debug, Serialize)]
struct MessageRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<tools::Tool>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    content: Vec<ResponseContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

pub struct AnthropicClient {
    api_key: String,
    client: reqwest::Client,
    history: Vec<ChatMessage>,
    tools_enabled: bool,
}

/// Callback for tool execution events
pub enum ToolEvent {
    Executing(String),      // Tool name being executed
    Result(String, String), // Tool name, result preview
}

impl AnthropicClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
            history: Vec::new(),
            tools_enabled: true,
        }
    }

    pub fn enable_tools(&mut self, enabled: bool) {
        self.tools_enabled = enabled;
    }

    pub async fn chat<F>(&mut self, user_message: &str, on_tool_event: F) -> Result<String>
    where
        F: Fn(ToolEvent),
    {
        // Add user message to history
        self.history.push(ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text(user_message.to_string()),
        });

        loop {
            let request = MessageRequest {
                model: "claude-3-haiku-20240307".to_string(),
                max_tokens: 4096,
                system: SYSTEM_PROMPT.to_string(),
                messages: self.history.clone(),
                tools: if self.tools_enabled {
                    Some(tools::get_tools())
                } else {
                    None
                },
            };

            let response = self
                .client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&request)
                .send()
                .await?;

            if !response.status().is_success() {
                let error_text = response.text().await?;
                return Err(anyhow::anyhow!("API error: {}", error_text));
            }

            let response: MessageResponse = response.json().await?;

            // Process response
            let mut text_response = String::new();
            let mut tool_uses: Vec<ToolUse> = Vec::new();

            for block in &response.content {
                match block {
                    ResponseContentBlock::Text { text } => {
                        text_response.push_str(text);
                    }
                    ResponseContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push(ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                }
            }

            // If there are tool uses, execute them and continue
            if !tool_uses.is_empty() {
                // Add assistant message with tool uses to history
                let assistant_blocks: Vec<ContentBlock> = response
                    .content
                    .iter()
                    .map(|b| match b {
                        ResponseContentBlock::Text { text } => ContentBlock::Text {
                            text: text.clone(),
                        },
                        ResponseContentBlock::ToolUse { id, name, input } => {
                            ContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            }
                        }
                    })
                    .collect();

                self.history.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Blocks(assistant_blocks),
                });

                // Execute tools and collect results
                let mut tool_results: Vec<ContentBlock> = Vec::new();
                for tool_use in &tool_uses {
                    on_tool_event(ToolEvent::Executing(tool_use.name.clone()));

                    let result = tools::execute_tool(tool_use).await;
                    let preview = result.content.chars().take(100).collect::<String>();
                    on_tool_event(ToolEvent::Result(tool_use.name.clone(), preview));

                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: result.tool_use_id,
                        content: result.content,
                    });
                }

                // Add tool results to history
                self.history.push(ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Blocks(tool_results),
                });

                // Continue the loop to get Claude's response to tool results
                continue;
            }

            // No tool uses, we have a final text response
            self.history.push(ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text(text_response.clone()),
            });

            return Ok(text_response);
        }
    }
}

const SYSTEM_PROMPT: &str = r#"あなたはVantage Pointの開発アシスタントです。
ユーザーの開発作業を支援するために、提供されたツールを活用してください。

## 利用可能なツール

### ファイル操作
- read_file: ファイルを読む
- write_file: ファイルを書く
- list_dir: ディレクトリ一覧
- search_files: ファイル検索

### シェル
- run_command: シェルコマンド実行

### GitHub (chronista-club/vantage-point)
- gh_list_issues: イシュー一覧
- gh_create_issue: イシュー作成
- gh_list_milestones: マイルストーン一覧
- gh_create_milestone: マイルストーン作成

## プロジェクト構成
- Phase 0: Agent基盤 (現在)
- Phase 1: クロスデバイス同期
- Phase 2: Vision Pro空間体験

## ルール
- ユーザーの指示に従い、適切なツールを使用
- ファイル操作前に確認を行う
- エラーが発生した場合は分かりやすく説明
- 作業の進捗を報告
- イシュー作成時は適切なマイルストーンを設定

## 現在の作業ディレクトリ
/Users/makoto/repos/vantage-point"#;
