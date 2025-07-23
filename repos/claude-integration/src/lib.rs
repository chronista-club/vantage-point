//! # Claude Integration Library
//!
//! Anthropic Claude AIとの統合を簡単にするRustライブラリ
//!
//! ## 特徴
//!
//! - 非同期API
//! - ストリーミングレスポンス対応
//! - Function Calling対応
//! - Vision（画像認識）対応
//! - 自動リトライとレート制限
//! - 型安全なメッセージ構築
//!
//! ## 使用例
//!
//! ```rust,no_run
//! use claude_integration::{ClaudeClient, Message, Role};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = ClaudeClient::new("your-api-key")?;
//!
//!     let response = client
//!         .chat()
//!         .message(Role::User, "Hello, Claude!")
//!         .send()
//!         .await?;
//!
//!     println!("Claude: {}", response.content);
//!     Ok(())
//! }
//! ```

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod client;
pub mod config;
pub mod error;
pub mod message;
pub mod model;
pub mod rate_limit;
pub mod request;
pub mod response;
pub mod streaming;

#[cfg(feature = "markdown")]
pub mod markdown;

#[cfg(feature = "unison")]
pub mod unison;

pub use client::{ClaudeClient, ClaudeClientBuilder};
pub use config::ClaudeConfig;
pub use error::{ClaudeError, Result};
pub use message::{Message, MessageBuilder, Role};
pub use model::Model;
pub use request::{ChatRequest, ChatRequestBuilder};
pub use response::{ChatResponse, StreamResponse};

// 共通の定数
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// ライブラリのバージョン
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// プリリュード - よく使う型を一括インポート
pub mod prelude {
    pub use crate::{
        client::{ClaudeClient, ClaudeClientBuilder},
        error::{ClaudeError, Result},
        message::{Message, MessageBuilder, Role},
        model::Model,
        request::ChatRequestBuilder,
        response::ChatResponse,
    };

    #[cfg(feature = "streaming")]
    pub use crate::streaming::{StreamEvent, StreamResponse};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }
}
