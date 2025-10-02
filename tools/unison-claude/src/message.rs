//! メッセージ型の定義
//!
//! Claude APIで使用するメッセージとその構築

use serde::{Deserialize, Serialize};
use std::fmt;

/// メッセージの役割
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// ユーザーメッセージ
    User,
    /// アシスタント（Claude）のメッセージ
    Assistant,
    /// システムメッセージ
    System,
}

impl Role {
    /// 文字列表現を取得
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = RoleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),
            "system" => Ok(Role::System),
            _ => Err(RoleParseError {
                value: s.to_string(),
            }),
        }
    }
}

/// ロール解析エラー
#[derive(Debug, Clone)]
pub struct RoleParseError {
    value: String,
}

impl fmt::Display for RoleParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Unknown role: {}", self.value)
    }
}

impl std::error::Error for RoleParseError {}

/// チャットメッセージ
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// メッセージの役割
    pub role: Role,
    /// メッセージの内容
    pub content: String,
    /// メッセージのメタデータ（オプション）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MessageMetadata>,
}

impl Message {
    /// 新しいメッセージを作成
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            metadata: None,
        }
    }

    /// ユーザーメッセージを作成
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    /// アシスタントメッセージを作成
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    /// システムメッセージを作成
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    /// メタデータを設定
    pub fn with_metadata(mut self, metadata: MessageMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// メッセージの長さ（文字数）を取得
    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// メッセージが空かどうか
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// トークン数の推定値を取得（概算）
    pub fn estimated_tokens(&self) -> usize {
        // 簡易的な推定: 4文字 = 1トークン
        (self.content.len() + 3) / 4
    }
}

/// メッセージのメタデータ
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageMetadata {
    /// メッセージID（オプション）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// タイムスタンプ（オプション）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    /// カスタムタグ
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// その他のメタデータ
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

impl Default for MessageMetadata {
    fn default() -> Self {
        Self {
            id: None,
            timestamp: None,
            tags: None,
            extra: None,
        }
    }
}

/// メッセージビルダー
pub struct MessageBuilder {
    role: Option<Role>,
    content: String,
    metadata: MessageMetadata,
}

impl MessageBuilder {
    /// 新しいビルダーを作成
    pub fn new() -> Self {
        Self {
            role: None,
            content: String::new(),
            metadata: MessageMetadata::default(),
        }
    }

    /// ロールを設定
    pub fn role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// コンテンツを設定
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// コンテンツを追加
    pub fn append_content(mut self, content: impl AsRef<str>) -> Self {
        if !self.content.is_empty() {
            self.content.push('\n');
        }
        self.content.push_str(content.as_ref());
        self
    }

    /// IDを設定
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.metadata.id = Some(id.into());
        self
    }

    /// タイムスタンプを設定
    pub fn timestamp(mut self, timestamp: u64) -> Self {
        self.metadata.timestamp = Some(timestamp);
        self
    }

    /// 現在のタイムスタンプを設定
    pub fn with_current_timestamp(mut self) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.metadata.timestamp = Some(timestamp);
        self
    }

    /// タグを追加
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.metadata
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// 複数のタグを追加
    pub fn tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let tag_vec = self.metadata.tags.get_or_insert_with(Vec::new);
        tag_vec.extend(tags.into_iter().map(|t| t.into()));
        self
    }

    /// カスタムメタデータを設定
    pub fn extra(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        let extra = self
            .metadata
            .extra
            .get_or_insert_with(|| serde_json::json!({}));
        if let serde_json::Value::Object(map) = extra {
            map.insert(key.into(), value);
        }
        self
    }

    /// メッセージを構築
    pub fn build(self) -> Result<Message, MessageBuildError> {
        let role = self.role.ok_or(MessageBuildError::MissingRole)?;
        if self.content.is_empty() {
            return Err(MessageBuildError::EmptyContent);
        }

        let metadata = if self.metadata == MessageMetadata::default() {
            None
        } else {
            Some(self.metadata)
        };

        Ok(Message {
            role,
            content: self.content,
            metadata,
        })
    }
}

impl Default for MessageBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// メッセージ構築エラー
#[derive(Debug, Clone)]
pub enum MessageBuildError {
    /// ロールが設定されていない
    MissingRole,
    /// コンテンツが空
    EmptyContent,
}

impl fmt::Display for MessageBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageBuildError::MissingRole => write!(f, "Message role is required"),
            MessageBuildError::EmptyContent => write!(f, "Message content cannot be empty"),
        }
    }
}

impl std::error::Error for MessageBuildError {}

/// メッセージのコレクション用ヘルパー
pub struct Messages(Vec<Message>);

impl Messages {
    /// 新しいコレクションを作成
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// メッセージを追加
    pub fn add(mut self, message: Message) -> Self {
        self.0.push(message);
        self
    }

    /// ユーザーメッセージを追加
    pub fn user(self, content: impl Into<String>) -> Self {
        self.add(Message::user(content))
    }

    /// アシスタントメッセージを追加
    pub fn assistant(self, content: impl Into<String>) -> Self {
        self.add(Message::assistant(content))
    }

    /// システムメッセージを追加
    pub fn system(self, content: impl Into<String>) -> Self {
        self.add(Message::system(content))
    }

    /// 内部のVecを取得
    pub fn into_vec(self) -> Vec<Message> {
        self.0
    }

    /// 参照を取得
    pub fn as_slice(&self) -> &[Message] {
        &self.0
    }

    /// 総トークン数の推定値を取得
    pub fn estimated_total_tokens(&self) -> usize {
        self.0.iter().map(|m| m.estimated_tokens()).sum()
    }
}

impl Default for Messages {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<Message>> for Messages {
    fn from(messages: Vec<Message>) -> Self {
        Self(messages)
    }
}

impl IntoIterator for Messages {
    type Item = Message;
    type IntoIter = std::vec::IntoIter<Message>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_parsing() {
        assert_eq!("user".parse::<Role>().unwrap(), Role::User);
        assert_eq!("ASSISTANT".parse::<Role>().unwrap(), Role::Assistant);
        assert_eq!("System".parse::<Role>().unwrap(), Role::System);
        assert!("unknown".parse::<Role>().is_err());
    }

    #[test]
    fn test_message_creation() {
        let msg = Message::user("Hello, Claude!");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "Hello, Claude!");
        assert!(msg.metadata.is_none());

        let msg = Message::assistant("Hello! How can I help you?");
        assert_eq!(msg.role, Role::Assistant);
    }

    #[test]
    fn test_message_builder() {
        let msg = MessageBuilder::new()
            .role(Role::User)
            .content("Test message")
            .id("msg-123")
            .tag("test")
            .tag("example")
            .build()
            .unwrap();

        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "Test message");
        assert!(msg.metadata.is_some());
        let metadata = msg.metadata.unwrap();
        assert_eq!(metadata.id, Some("msg-123".to_string()));
        assert_eq!(metadata.tags.unwrap().len(), 2);
    }

    #[test]
    fn test_message_builder_errors() {
        // ロールが設定されていない
        let result = MessageBuilder::new().content("Test").build();
        assert!(matches!(result, Err(MessageBuildError::MissingRole)));

        // コンテンツが空
        let result = MessageBuilder::new().role(Role::User).build();
        assert!(matches!(result, Err(MessageBuildError::EmptyContent)));
    }

    #[test]
    fn test_messages_collection() {
        let messages = Messages::new()
            .user("Hello")
            .assistant("Hi there!")
            .user("How are you?")
            .assistant("I'm doing well, thank you!");

        let vec = messages.into_vec();
        assert_eq!(vec.len(), 4);
        assert_eq!(vec[0].role, Role::User);
        assert_eq!(vec[1].role, Role::Assistant);
    }

    #[test]
    fn test_estimated_tokens() {
        let msg = Message::user("Hello, world!"); // 13 characters
        assert_eq!(msg.estimated_tokens(), 4); // (13 + 3) / 4 = 4

        let msg = Message::user("A"); // 1 character
        assert_eq!(msg.estimated_tokens(), 1); // (1 + 3) / 4 = 1
    }
}
