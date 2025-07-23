//! エラー型の定義
//!
//! Claude統合ライブラリで使用するエラー型

use std::time::Duration;
use thiserror::Error;

/// Claude統合ライブラリのエラー型
#[derive(Error, Debug)]
pub enum ClaudeError {
    /// 無効なリクエスト
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// 認証エラー
    #[error("Authentication failed: {0}")]
    Authentication(String),

    /// レート制限エラー
    #[error("Rate limit exceeded, retry after {retry_after:?}")]
    RateLimit {
        /// リトライまでの待機時間
        retry_after: Option<Duration>,
    },

    /// サーバーエラー
    #[error("Server error: {0}")]
    ServerError(String),

    /// ネットワークエラー
    #[error("Network error: {0}")]
    Network(String),

    /// タイムアウトエラー
    #[error("Request timed out after {0:?}")]
    Timeout(Duration),

    /// 設定エラー
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// シリアライゼーションエラー
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// デシリアライゼーションエラー
    #[error("Deserialization error: {0}")]
    Deserialization(String),

    /// ストリーミングエラー
    #[error("Streaming error: {0}")]
    Streaming(String),

    /// 検証エラー
    #[error("Validation error: {0}")]
    Validation(String),

    /// メッセージが空
    #[error("No messages provided")]
    NoMessages,

    /// メッセージが長すぎる
    #[error("Message too long: {length} (max: {max_length})")]
    MessageTooLong {
        /// 実際の長さ
        length: usize,
        /// 最大長
        max_length: usize,
    },

    /// モデルがサポートされていない
    #[error("Unsupported model: {0}")]
    UnsupportedModel(String),

    /// 機能がサポートされていない
    #[error("Unsupported feature: {0}")]
    UnsupportedFeature(String),

    /// ファイル読み込みエラー
    #[error("Failed to read file: {0}")]
    FileRead(String),

    /// 画像処理エラー
    #[error("Image processing error: {0}")]
    ImageProcessing(String),

    /// Base64エンコード/デコードエラー
    #[error("Base64 error: {0}")]
    Base64(String),

    /// 環境変数エラー
    #[error("Environment variable error: {0}")]
    Environment(String),

    /// 未知のエラー
    #[error("Unknown error: {0}")]
    Unknown(String),

    /// HTTPエラー
    #[error("HTTP error: status={status}, message={message}")]
    Http {
        /// HTTPステータスコード
        status: u16,
        /// エラーメッセージ
        message: String,
    },

    /// IO エラー
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Reqwestエラー
    #[error("HTTP client error: {0}")]
    Reqwest(#[from] reqwest::Error),

    /// JSON解析エラー
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// その他のエラー
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ClaudeError {
    /// エラーがリトライ可能かどうかを判定
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ClaudeError::RateLimit { .. }
                | ClaudeError::ServerError(_)
                | ClaudeError::Network(_)
                | ClaudeError::Timeout(_)
                | ClaudeError::Http { status, .. } if *status >= 500
        )
    }

    /// エラーがクライアントエラーかどうかを判定
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            ClaudeError::InvalidRequest(_)
                | ClaudeError::Authentication(_)
                | ClaudeError::Configuration(_)
                | ClaudeError::Validation(_)
                | ClaudeError::NoMessages
                | ClaudeError::MessageTooLong { .. }
                | ClaudeError::UnsupportedModel(_)
                | ClaudeError::UnsupportedFeature(_)
                | ClaudeError::Http { status, .. } if *status >= 400 && *status < 500
        )
    }

    /// エラーコードを取得
    pub fn error_code(&self) -> &'static str {
        match self {
            ClaudeError::InvalidRequest(_) => "invalid_request",
            ClaudeError::Authentication(_) => "authentication_failed",
            ClaudeError::RateLimit { .. } => "rate_limit_exceeded",
            ClaudeError::ServerError(_) => "server_error",
            ClaudeError::Network(_) => "network_error",
            ClaudeError::Timeout(_) => "timeout",
            ClaudeError::Configuration(_) => "configuration_error",
            ClaudeError::Serialization(_) => "serialization_error",
            ClaudeError::Deserialization(_) => "deserialization_error",
            ClaudeError::Streaming(_) => "streaming_error",
            ClaudeError::Validation(_) => "validation_error",
            ClaudeError::NoMessages => "no_messages",
            ClaudeError::MessageTooLong { .. } => "message_too_long",
            ClaudeError::UnsupportedModel(_) => "unsupported_model",
            ClaudeError::UnsupportedFeature(_) => "unsupported_feature",
            ClaudeError::FileRead(_) => "file_read_error",
            ClaudeError::ImageProcessing(_) => "image_processing_error",
            ClaudeError::Base64(_) => "base64_error",
            ClaudeError::Environment(_) => "environment_error",
            ClaudeError::Unknown(_) => "unknown_error",
            ClaudeError::Http { .. } => "http_error",
            ClaudeError::Io(_) => "io_error",
            ClaudeError::Reqwest(_) => "http_client_error",
            ClaudeError::Json(_) => "json_error",
            ClaudeError::Other(_) => "other_error",
        }
    }

    /// レート制限エラーを作成
    pub fn rate_limit(retry_after_seconds: Option<u64>) -> Self {
        ClaudeError::RateLimit {
            retry_after: retry_after_seconds.map(Duration::from_secs),
        }
    }

    /// HTTPエラーを作成
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        ClaudeError::Http {
            status,
            message: message.into(),
        }
    }

    /// メッセージ長エラーを作成
    pub fn message_too_long(length: usize, max_length: usize) -> Self {
        ClaudeError::MessageTooLong { length, max_length }
    }
}

/// Result型のエイリアス
pub type Result<T> = std::result::Result<T, ClaudeError>;

/// エラー変換のヘルパートレイト
pub trait ErrorContext<T> {
    /// エラーにコンテキストを追加
    fn context(self, context: impl Into<String>) -> Result<T>;

    /// エラーに遅延評価のコンテキストを追加
    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String;
}

impl<T, E> ErrorContext<T> for std::result::Result<T, E>
where
    E: Into<ClaudeError>,
{
    fn context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|e| {
            let err: ClaudeError = e.into();
            ClaudeError::Unknown(format!("{}: {}", context.into(), err))
        })
    }

    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|e| {
            let err: ClaudeError = e.into();
            ClaudeError::Unknown(format!("{}: {}", f(), err))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_is_retryable() {
        assert!(ClaudeError::rate_limit(Some(5)).is_retryable());
        assert!(ClaudeError::ServerError("Internal error".to_string()).is_retryable());
        assert!(ClaudeError::Network("Connection failed".to_string()).is_retryable());
        assert!(ClaudeError::Timeout(Duration::from_secs(30)).is_retryable());
        assert!(ClaudeError::http(500, "Internal Server Error").is_retryable());
        assert!(ClaudeError::http(503, "Service Unavailable").is_retryable());

        assert!(!ClaudeError::InvalidRequest("Bad request".to_string()).is_retryable());
        assert!(!ClaudeError::Authentication("Invalid API key".to_string()).is_retryable());
        assert!(!ClaudeError::http(400, "Bad Request").is_retryable());
        assert!(!ClaudeError::http(401, "Unauthorized").is_retryable());
    }

    #[test]
    fn test_error_is_client_error() {
        assert!(ClaudeError::InvalidRequest("Bad request".to_string()).is_client_error());
        assert!(ClaudeError::Authentication("Invalid API key".to_string()).is_client_error());
        assert!(ClaudeError::Configuration("Missing config".to_string()).is_client_error());
        assert!(ClaudeError::Validation("Invalid input".to_string()).is_client_error());
        assert!(ClaudeError::NoMessages.is_client_error());
        assert!(ClaudeError::message_too_long(10000, 5000).is_client_error());
        assert!(ClaudeError::http(400, "Bad Request").is_client_error());
        assert!(ClaudeError::http(404, "Not Found").is_client_error());

        assert!(!ClaudeError::ServerError("Internal error".to_string()).is_client_error());
        assert!(!ClaudeError::Network("Connection failed".to_string()).is_client_error());
        assert!(!ClaudeError::http(500, "Internal Server Error").is_client_error());
    }

    #[test]
    fn test_error_code() {
        assert_eq!(ClaudeError::NoMessages.error_code(), "no_messages");
        assert_eq!(
            ClaudeError::RateLimit { retry_after: None }.error_code(),
            "rate_limit_exceeded"
        );
        assert_eq!(
            ClaudeError::message_too_long(1000, 500).error_code(),
            "message_too_long"
        );
    }

    #[test]
    fn test_error_context() {
        let result: Result<()> = Err(ClaudeError::Unknown("test".to_string()));
        let with_context = result.context("additional info");
        assert!(with_context.is_err());
        assert!(with_context
            .unwrap_err()
            .to_string()
            .contains("additional info"));
    }
}
