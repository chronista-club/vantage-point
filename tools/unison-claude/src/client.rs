//! Claudeクライアントの実装
//!
//! Anthropic APIとの通信を管理するメインクライアント

use crate::{
    config::ClaudeConfig,
    error::{ClaudeError, Result},
    message::{Message, Role},
    model::Model,
    rate_limit::RateLimiter,
    request::{ChatRequest, ChatRequestBuilder},
    response::{ChatResponse, StreamResponse},
    streaming::StreamHandler,
    DEFAULT_BASE_URL, DEFAULT_API_VERSION, DEFAULT_MAX_RETRIES, DEFAULT_TIMEOUT_SECS,
};
use anthropic_ai::{AnthropicClient, AnthropicError};
use futures::StreamExt;
use parking_lot::RwLock;
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Claude APIクライアント
///
/// Anthropic Claude APIとの通信を管理するメインクライアント
#[derive(Clone)]
pub struct ClaudeClient {
    /// 内部のAnthropic APIクライアント
    inner: Arc<AnthropicClient>,
    /// 設定
    config: Arc<ClaudeConfig>,
    /// レート制限
    rate_limiter: Arc<RateLimiter>,
    /// HTTPクライアント
    http_client: Arc<reqwest::Client>,
    /// 現在のモデル
    current_model: Arc<RwLock<Model>>,
}

impl ClaudeClient {
    /// APIキーを使って新しいクライアントを作成
    ///
    /// # Arguments
    ///
    /// * `api_key` - Anthropic APIキー
    ///
    /// # Example
    ///
    /// ```no_run
    /// use claude_integration::ClaudeClient;
    ///
    /// let client = ClaudeClient::new("sk-ant-...").unwrap();
    /// ```
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        ClaudeClientBuilder::new()
            .api_key(api_key)
            .build()
    }

    /// ビルダーを作成
    pub fn builder() -> ClaudeClientBuilder {
        ClaudeClientBuilder::new()
    }

    /// チャットリクエストビルダーを開始
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use claude_integration::{ClaudeClient, Role};
    /// # async fn example(client: ClaudeClient) -> Result<(), Box<dyn std::error::Error>> {
    /// let response = client
    ///     .chat()
    ///     .message(Role::User, "Hello!")
    ///     .send()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn chat(&self) -> ChatRequestBuilder {
        ChatRequestBuilder::new(self.clone())
    }

    /// 現在のモデルを取得
    pub fn current_model(&self) -> Model {
        *self.current_model.read()
    }

    /// モデルを設定
    pub fn set_model(&self, model: Model) {
        *self.current_model.write() = model;
    }

    /// 生のチャットリクエストを送信（内部API）
    pub(crate) async fn send_chat_request(&self, request: ChatRequest) -> Result<ChatResponse> {
        // レート制限チェック
        self.rate_limiter.acquire().await?;

        let mut retries = 0;
        let max_retries = self.config.max_retries;

        loop {
            match self.send_request_internal(&request).await {
                Ok(response) => return Ok(response),
                Err(e) if retries < max_retries && e.is_retryable() => {
                    retries += 1;
                    let delay = self.calculate_retry_delay(retries);
                    warn!(
                        "Request failed (attempt {}/{}), retrying in {:?}: {}",
                        retries, max_retries, delay, e
                    );
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// ストリーミングチャットリクエストを送信
    pub(crate) async fn send_chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<StreamResponse> {
        // レート制限チェック
        self.rate_limiter.acquire().await?;

        let stream_handler = StreamHandler::new(self.clone(), request);
        Ok(StreamResponse::new(stream_handler))
    }

    /// 内部リクエスト送信
    async fn send_request_internal(&self, request: &ChatRequest) -> Result<ChatResponse> {
        debug!("Sending chat request: {:?}", request);

        // Anthropic SDKを使用してリクエストを送信
        let anthropic_request = self.convert_to_anthropic_request(request)?;

        match self.inner.messages().create(anthropic_request).await {
            Ok(response) => {
                debug!("Received response: {:?}", response);
                self.convert_from_anthropic_response(response)
            }
            Err(e) => {
                error!("API request failed: {}", e);
                Err(self.convert_anthropic_error(e))
            }
        }
    }

    /// リクエストをAnthropic SDK形式に変換
    fn convert_to_anthropic_request(
        &self,
        request: &ChatRequest,
    ) -> Result<anthropic_ai::messages::CreateMessageRequest> {
        use anthropic_ai::messages::{CreateMessageRequest, MessageContent};

        let mut anthropic_messages = Vec::new();

        for msg in &request.messages {
            let content = MessageContent::Text(msg.content.clone());
            let role = match msg.role {
                Role::User => anthropic_ai::messages::Role::User,
                Role::Assistant => anthropic_ai::messages::Role::Assistant,
                Role::System => {
                    // Anthropicはシステムメッセージを別途扱う
                    continue;
                }
            };

            anthropic_messages.push(anthropic_ai::messages::Message {
                role,
                content: vec![content],
            });
        }

        // システムメッセージを抽出
        let system_message = request
            .messages
            .iter()
            .find(|m| matches!(m.role, Role::System))
            .map(|m| m.content.clone());

        Ok(CreateMessageRequest {
            model: request.model.to_string(),
            messages: anthropic_messages,
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
            top_p: request.top_p,
            top_k: request.top_k,
            stream: Some(false),
            system: system_message,
            ..Default::default()
        })
    }

    /// Anthropic SDKのレスポンスを変換
    fn convert_from_anthropic_response(
        &self,
        response: anthropic_ai::messages::MessagesResponse,
    ) -> Result<ChatResponse> {
        let content = response
            .content
            .into_iter()
            .filter_map(|c| match c {
                anthropic_ai::messages::ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ChatResponse {
            id: response.id,
            model: response.model,
            content,
            role: Role::Assistant,
            stop_reason: response.stop_reason,
            usage: crate::response::Usage {
                input_tokens: response.usage.input_tokens as u32,
                output_tokens: response.usage.output_tokens as u32,
            },
        })
    }

    /// Anthropicエラーを変換
    fn convert_anthropic_error(&self, error: AnthropicError) -> ClaudeError {
        match error {
            AnthropicError::InvalidRequest(msg) => ClaudeError::InvalidRequest(msg),
            AnthropicError::Authentication(msg) => ClaudeError::Authentication(msg),
            AnthropicError::RateLimit { retry_after } => {
                ClaudeError::RateLimit { retry_after }
            }
            AnthropicError::ServerError(msg) => ClaudeError::ServerError(msg),
            AnthropicError::NetworkError(e) => ClaudeError::Network(e.to_string()),
            _ => ClaudeError::Unknown(error.to_string()),
        }
    }

    /// リトライ遅延を計算
    fn calculate_retry_delay(&self, attempt: u32) -> Duration {
        let base_delay = Duration::from_millis(100);
        let max_delay = Duration::from_secs(10);
        let exponential_delay = base_delay * 2u32.pow(attempt - 1);
        exponential_delay.min(max_delay)
    }

    /// ヘルスチェック
    pub async fn health_check(&self) -> Result<bool> {
        // 簡単なテストメッセージを送信
        match self
            .chat()
            .message(Role::User, "Hi")
            .max_tokens(10)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(ClaudeError::Authentication(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Claudeクライアントビルダー
///
/// クライアントの設定を段階的に構築
pub struct ClaudeClientBuilder {
    api_key: Option<String>,
    base_url: String,
    api_version: String,
    model: Model,
    max_retries: u32,
    timeout: Duration,
    rate_limit_requests: Option<u32>,
    rate_limit_window: Option<Duration>,
    http_client: Option<reqwest::Client>,
}

impl ClaudeClientBuilder {
    /// 新しいビルダーを作成
    pub fn new() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            api_version: DEFAULT_API_VERSION.to_string(),
            model: Model::default(),
            max_retries: DEFAULT_MAX_RETRIES,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            rate_limit_requests: None,
            rate_limit_window: None,
            http_client: None,
        }
    }

    /// APIキーを設定
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// ベースURLを設定
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// APIバージョンを設定
    pub fn api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }

    /// デフォルトモデルを設定
    pub fn model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    /// 最大リトライ回数を設定
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// タイムアウトを設定
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// レート制限を設定
    pub fn rate_limit(mut self, requests: u32, window: Duration) -> Self {
        self.rate_limit_requests = Some(requests);
        self.rate_limit_window = Some(window);
        self
    }

    /// カスタムHTTPクライアントを設定
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// 設定ファイルから読み込み
    pub fn from_config(config: ClaudeConfig) -> Self {
        Self {
            api_key: config.api_key.clone(),
            base_url: config.base_url.clone(),
            api_version: config.api_version.clone(),
            model: config.default_model,
            max_retries: config.max_retries,
            timeout: config.timeout,
            rate_limit_requests: config.rate_limit_requests,
            rate_limit_window: config.rate_limit_window,
            http_client: None,
        }
    }

    /// クライアントを構築
    pub fn build(self) -> Result<ClaudeClient> {
        let api_key = self
            .api_key
            .ok_or_else(|| ClaudeError::Configuration("API key is required".to_string()))?;

        // HTTPクライアントを作成または使用
        let http_client = self.http_client.unwrap_or_else(|| {
            reqwest::Client::builder()
                .timeout(self.timeout)
                .default_headers({
                    let mut headers = reqwest::header::HeaderMap::new();
                    headers.insert(
                        "anthropic-version",
                        reqwest::header::HeaderValue::from_str(&self.api_version).unwrap(),
                    );
                    headers
                })
                .build()
                .expect("Failed to build HTTP client")
        });

        // Anthropicクライアントを作成
        let anthropic_client = AnthropicClient::new(api_key.clone())
            .map_err(|e| ClaudeError::Configuration(e.to_string()))?;

        // レート制限を設定
        let rate_limiter = if let (Some(requests), Some(window)) =
            (self.rate_limit_requests, self.rate_limit_window)
        {
            Arc::new(RateLimiter::new(requests, window))
        } else {
            Arc::new(RateLimiter::default())
        };

        // 設定を作成
        let config = ClaudeConfig {
            api_key: Some(api_key),
            base_url: self.base_url,
            api_version: self.api_version,
            default_model: self.model,
            max_retries: self.max_retries,
            timeout: self.timeout,
            rate_limit_requests: self.rate_limit_requests,
            rate_limit_window: self.rate_limit_window,
        };

        info!("Claude client initialized with model: {}", self.model);

        Ok(ClaudeClient {
            inner: Arc::new(anthropic_client),
            config: Arc::new(config),
            rate_limiter,
            http_client: Arc::new(http_client),
            current_model: Arc::new(RwLock::new(self.model)),
        })
    }
}

impl Default for ClaudeClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder() {
        let builder = ClaudeClientBuilder::new()
            .api_key("test-key")
            .model(Model::Claude3Sonnet)
            .max_retries(5)
            .timeout(Duration::from_secs(60));

        assert!(builder.api_key.is_some());
    }

    #[test]
    fn test_client_creation_without_api_key() {
        let result = ClaudeClientBuilder::new().build();
        assert!(result.is_err());
    }
}
