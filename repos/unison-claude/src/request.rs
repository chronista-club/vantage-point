//! リクエストモジュール
//!
//! Claude APIへのリクエストを構築・管理

use crate::{
    error::{ClaudeError, Result},
    message::{Message, Role},
    model::Model,
    ClaudeClient,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// チャットリクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// 使用するモデル
    pub model: Model,

    /// メッセージの配列
    pub messages: Vec<Message>,

    /// 最大生成トークン数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// 温度パラメータ（0.0 - 1.0）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Top-pサンプリング
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Top-kサンプリング
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,

    /// ストリーミングを有効にするか
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,

    /// 停止シーケンス
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,

    /// メタデータ
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl ChatRequest {
    /// 新しいリクエストを作成
    pub fn new(model: Model, messages: Vec<Message>) -> Self {
        Self {
            model,
            messages,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stream: None,
            stop_sequences: None,
            metadata: None,
        }
    }

    /// リクエストを検証
    pub fn validate(&self) -> Result<()> {
        if self.messages.is_empty() {
            return Err(ClaudeError::NoMessages);
        }

        // 最初のメッセージがシステムメッセージの場合、次はユーザーメッセージである必要がある
        if self.messages.first().map(|m| m.role) == Some(Role::System) {
            if self.messages.len() < 2 {
                return Err(ClaudeError::Validation(
                    "System message must be followed by a user message".to_string(),
                ));
            }
            if self.messages[1].role != Role::User {
                return Err(ClaudeError::Validation(
                    "System message must be followed by a user message".to_string(),
                ));
            }
        }

        // メッセージの順序を検証（ユーザーとアシスタントが交互である必要がある）
        let mut expected_role = Role::User;
        for (i, msg) in self.messages.iter().enumerate() {
            if msg.role == Role::System && i > 0 {
                return Err(ClaudeError::Validation(
                    "System message can only be the first message".to_string(),
                ));
            }

            if msg.role != Role::System {
                if msg.role != expected_role {
                    return Err(ClaudeError::Validation(format!(
                        "Expected {} message at position {}, got {}",
                        expected_role, i, msg.role
                    )));
                }
                expected_role = if expected_role == Role::User {
                    Role::Assistant
                } else {
                    Role::User
                };
            }
        }

        // 最後のメッセージはユーザーメッセージである必要がある
        if let Some(last_msg) = self.messages.last() {
            if last_msg.role != Role::User {
                return Err(ClaudeError::Validation(
                    "Last message must be from user".to_string(),
                ));
            }
        }

        // パラメータの範囲を検証
        if let Some(temp) = self.temperature {
            if !(0.0..=1.0).contains(&temp) {
                return Err(ClaudeError::Validation(
                    "Temperature must be between 0.0 and 1.0".to_string(),
                ));
            }
        }

        if let Some(top_p) = self.top_p {
            if !(0.0..=1.0).contains(&top_p) {
                return Err(ClaudeError::Validation(
                    "Top-p must be between 0.0 and 1.0".to_string(),
                ));
            }
        }

        if let Some(max_tokens) = self.max_tokens {
            let model_max = self.model.max_output_tokens() as u32;
            if max_tokens > model_max {
                return Err(ClaudeError::Validation(format!(
                    "Max tokens {} exceeds model limit of {}",
                    max_tokens, model_max
                )));
            }
        }

        Ok(())
    }
}

/// チャットリクエストビルダー
pub struct ChatRequestBuilder {
    client: ClaudeClient,
    model: Option<Model>,
    messages: Vec<Message>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<u32>,
    stream: bool,
    stop_sequences: Vec<String>,
    metadata: HashMap<String, serde_json::Value>,
}

impl ChatRequestBuilder {
    /// 新しいビルダーを作成
    pub fn new(client: ClaudeClient) -> Self {
        Self {
            client,
            model: None,
            messages: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stream: false,
            stop_sequences: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// モデルを設定
    pub fn model(mut self, model: Model) -> Self {
        self.model = Some(model);
        self
    }

    /// メッセージを追加
    pub fn message(mut self, role: Role, content: impl Into<String>) -> Self {
        self.messages.push(Message::new(role, content));
        self
    }

    /// システムメッセージを設定
    pub fn system(mut self, content: impl Into<String>) -> Self {
        // 既存のシステムメッセージを削除
        self.messages.retain(|m| m.role != Role::System);
        // 先頭に新しいシステムメッセージを追加
        self.messages.insert(0, Message::system(content));
        self
    }

    /// ユーザーメッセージを追加
    pub fn user(mut self, content: impl Into<String>) -> Self {
        self.message(Role::User, content)
    }

    /// アシスタントメッセージを追加
    pub fn assistant(mut self, content: impl Into<String>) -> Self {
        self.message(Role::Assistant, content)
    }

    /// 複数のメッセージを追加
    pub fn messages(mut self, messages: impl IntoIterator<Item = Message>) -> Self {
        self.messages.extend(messages);
        self
    }

    /// 最大トークン数を設定
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// 温度を設定
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Top-pを設定
    pub fn top_p(mut self, top_p: f32) -> Self {
        self.top_p = Some(top_p);
        self
    }

    /// Top-kを設定
    pub fn top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// ストリーミングを有効化
    pub fn stream(mut self) -> Self {
        self.stream = true;
        self
    }

    /// 停止シーケンスを追加
    pub fn stop_sequence(mut self, sequence: impl Into<String>) -> Self {
        self.stop_sequences.push(sequence.into());
        self
    }

    /// 複数の停止シーケンスを追加
    pub fn stop_sequences(mut self, sequences: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.stop_sequences.extend(sequences.into_iter().map(|s| s.into()));
        self
    }

    /// メタデータを追加
    pub fn metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// リクエストを構築
    fn build_request(&self) -> Result<ChatRequest> {
        let model = self.model.unwrap_or_else(|| self.client.current_model());

        let mut request = ChatRequest::new(model, self.messages.clone());
        request.max_tokens = self.max_tokens;
        request.temperature = self.temperature;
        request.top_p = self.top_p;
        request.top_k = self.top_k;
        request.stream = if self.stream { Some(true) } else { None };

        if !self.stop_sequences.is_empty() {
            request.stop_sequences = Some(self.stop_sequences.clone());
        }

        if !self.metadata.is_empty() {
            request.metadata = Some(self.metadata.clone());
        }

        request.validate()?;

        Ok(request)
    }

    /// リクエストを送信
    pub async fn send(self) -> Result<crate::response::ChatResponse> {
        let request = self.build_request()?;
        self.client.send_chat_request(request).await
    }

    /// ストリーミングでリクエストを送信
    pub async fn send_stream(mut self) -> Result<crate::response::StreamResponse> {
        self.stream = true;
        let request = self.build_request()?;
        self.client.send_chat_stream(request).await
    }
}

/// Function Callingのためのツール定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// ツールの名前
    pub name: String,

    /// ツールの説明
    pub description: String,

    /// 入力スキーマ（JSON Schema）
    pub input_schema: serde_json::Value,
}

/// Vision（画像）入力のための構造体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    /// 画像のタイプ
    pub r#type: ImageType,

    /// 画像データ
    pub data: ImageData,
}

/// 画像のタイプ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageType {
    /// JPEG画像
    Jpeg,
    /// PNG画像
    Png,
    /// GIF画像
    Gif,
    /// WebP画像
    Webp,
}

/// 画像データ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImageData {
    /// Base64エンコードされた画像
    Base64(String),
    /// 画像のURL
    Url(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_request_validation() {
        // 空のメッセージ
        let request = ChatRequest::new(Model::default(), vec![]);
        assert!(request.validate().is_err());

        // 正常なメッセージ
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Hi there!"),
            Message::user("How are you?"),
        ];
        let request = ChatRequest::new(Model::default(), messages);
        assert!(request.validate().is_ok());

        // 不正な順序
        let messages = vec![
            Message::user("Hello"),
            Message::user("How are you?"), // 連続したユーザーメッセージ
        ];
        let request = ChatRequest::new(Model::default(), messages);
        assert!(request.validate().is_err());

        // システムメッセージが先頭以外
        let messages = vec![
            Message::user("Hello"),
            Message::system("System prompt"), // 不正な位置
        ];
        let request = ChatRequest::new(Model::default(), messages);
        assert!(request.validate().is_err());
    }

    #[test]
    fn test_parameter_validation() {
        let messages = vec![Message::user("Test")];

        // 温度が範囲外
        let mut request = ChatRequest::new(Model::default(), messages.clone());
        request.temperature = Some(1.5);
        assert!(request.validate().is_err());

        // Top-pが範囲外
        let mut request = ChatRequest::new(Model::default(), messages.clone());
        request.top_p = Some(-0.1);
        assert!(request.validate().is_err());

        // 最大トークンが多すぎる
        let mut request = ChatRequest::new(Model::Claude3Haiku, messages);
        request.max_tokens = Some(10000);
        assert!(request.validate().is_err());
    }
}
