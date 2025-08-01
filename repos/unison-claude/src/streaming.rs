//! ストリーミングモジュール
//!
//! Claude APIのストリーミングレスポンスを処理

use crate::{
    error::{ClaudeError, Result},
    request::ChatRequest,
    response::{StreamEvent, Usage},
    ClaudeClient,
};
use futures::{future::BoxFuture, FutureExt, StreamExt};
use reqwest::Response;
use serde_json::Value;
use std::{
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_stream::wrappers::LinesStream;
use tracing::{debug, error, trace, warn};

/// ストリーミングハンドラー
pub struct StreamHandler {
    client: ClaudeClient,
    request: ChatRequest,
    sender: mpsc::Sender<Result<StreamEvent>>,
    receiver: Option<mpsc::Receiver<Result<StreamEvent>>>,
}

impl StreamHandler {
    /// 新しいストリーミングハンドラーを作成
    pub fn new(client: ClaudeClient, request: ChatRequest) -> Self {
        let (sender, receiver) = mpsc::channel(100);
        Self {
            client,
            request,
            sender,
            receiver: Some(receiver),
        }
    }

    /// レシーバーを取得（一度のみ）
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<Result<StreamEvent>>> {
        self.receiver.take()
    }

    /// ストリーミングを開始
    pub async fn start_streaming(self) -> Result<()> {
        let client = self.client;
        let request = self.request;
        let sender = self.sender;

        // バックグラウンドでストリーミング処理を実行
        tokio::spawn(async move {
            if let Err(e) = Self::stream_handler(client, request, sender.clone()).await {
                error!("Stream handler error: {}", e);
                let _ = sender.send(Err(e)).await;
            }
        });

        Ok(())
    }

    /// 実際のストリーミング処理
    async fn stream_handler(
        client: ClaudeClient,
        mut request: ChatRequest,
        sender: mpsc::Sender<Result<StreamEvent>>,
    ) -> Result<()> {
        // ストリーミングを有効化
        request.stream = Some(true);

        // HTTPリクエストを準備
        let http_client = &client.http_client;
        let url = format!("{}/v1/messages", client.config.base_url);

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-api-key",
            reqwest::header::HeaderValue::from_str(&client.config.api_key.as_ref().unwrap())
                .map_err(|_| ClaudeError::Configuration("Invalid API key".to_string()))?,
        );
        headers.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_str(&client.config.api_version)
                .map_err(|_| ClaudeError::Configuration("Invalid API version".to_string()))?,
        );
        headers.insert(
            "content-type",
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            "accept",
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );

        // リクエストを送信
        let response = http_client
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .map_err(|e| ClaudeError::Network(e.to_string()))?;

        // ステータスコードをチェック
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ClaudeError::http(status.as_u16(), error_body));
        }

        // SSEストリームを処理
        Self::process_sse_stream(response, sender).await
    }

    /// SSEストリームを処理
    async fn process_sse_stream(
        response: Response,
        sender: mpsc::Sender<Result<StreamEvent>>,
    ) -> Result<()> {
        let stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut event_type: Option<String> = None;
        let mut event_data = String::new();

        let mut stream = stream.map(|result| {
            result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        });

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ClaudeError::Network(e.to_string()))?;
            buffer.extend_from_slice(&chunk);

            // 改行で分割してイベントを処理
            while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                let line = &buffer[..newline_pos];
                buffer.drain(..=newline_pos);

                let line_str = String::from_utf8_lossy(line);
                trace!("SSE line: {}", line_str);

                if line_str.starts_with("event:") {
                    event_type = Some(line_str[6..].trim().to_string());
                } else if line_str.starts_with("data:") {
                    event_data.push_str(&line_str[5..].trim());
                    event_data.push('\n');
                } else if line_str.is_empty() && event_type.is_some() {
                    // イベントの終了
                    if let Some(event) = Self::parse_event(&event_type.take().unwrap(), &event_data)? {
                        if let Err(e) = sender.send(Ok(event)).await {
                            debug!("Receiver dropped: {}", e);
                            break;
                        }
                    }
                    event_data.clear();
                }
            }
        }

        Ok(())
    }

    /// イベントを解析
    fn parse_event(event_type: &str, data: &str) -> Result<Option<StreamEvent>> {
        let data = data.trim();

        if data.is_empty() {
            return Ok(None);
        }

        match event_type {
            "message_start" => {
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| ClaudeError::Deserialization(e.to_string()))?;

                let message = &value["message"];
                let id = message["id"].as_str().unwrap_or_default().to_string();
                let model = message["model"].as_str().unwrap_or_default().to_string();

                let usage = message["usage"].as_object().map(|u| {
                    Usage::new(
                        u["input_tokens"].as_u64().unwrap_or(0) as u32,
                        u["output_tokens"].as_u64().unwrap_or(0) as u32,
                    )
                });

                Ok(Some(StreamEvent::MessageStart { id, model, usage }))
            }
            "content_block_start" => {
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| ClaudeError::Deserialization(e.to_string()))?;

                let index = value["index"].as_u64().unwrap_or(0) as usize;
                let content_type = value["content_block"]["type"]
                    .as_str()
                    .unwrap_or("text")
                    .to_string();

                Ok(Some(StreamEvent::ContentBlockStart { index, content_type }))
            }
            "content_block_delta" => {
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| ClaudeError::Deserialization(e.to_string()))?;

                if let Some(text) = value["delta"]["text"].as_str() {
                    Ok(Some(StreamEvent::ContentDelta {
                        text: text.to_string(),
                    }))
                } else {
                    Ok(None)
                }
            }
            "content_block_stop" => {
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| ClaudeError::Deserialization(e.to_string()))?;

                let index = value["index"].as_u64().unwrap_or(0) as usize;
                Ok(Some(StreamEvent::ContentBlockStop { index }))
            }
            "message_stop" => {
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| ClaudeError::Deserialization(e.to_string()))?;

                let reason = value["stop_reason"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();

                let usage = value["usage"].as_object().map(|u| {
                    Usage::new(
                        u["input_tokens"].as_u64().unwrap_or(0) as u32,
                        u["output_tokens"].as_u64().unwrap_or(0) as u32,
                    )
                });

                Ok(Some(StreamEvent::MessageStop { reason, usage }))
            }
            "error" => {
                let value: Value = serde_json::from_str(data)
                    .map_err(|e| ClaudeError::Deserialization(e.to_string()))?;

                let message = value["error"]["message"]
                    .as_str()
                    .unwrap_or("Unknown error")
                    .to_string();
                let code = value["error"]["type"].as_str().map(|s| s.to_string());

                Ok(Some(StreamEvent::Error { message, code }))
            }
            "ping" => Ok(Some(StreamEvent::Ping)),
            _ => {
                warn!("Unknown event type: {}", event_type);
                Ok(None)
            }
        }
    }
}

/// ストリーミング設定
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// バッファサイズ
    pub buffer_size: usize,
    /// タイムアウト
    pub timeout: Duration,
    /// 再接続の最大試行回数
    pub max_reconnects: u32,
    /// 再接続の遅延
    pub reconnect_delay: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            buffer_size: 100,
            timeout: Duration::from_secs(300), // 5分
            max_reconnects: 3,
            reconnect_delay: Duration::from_secs(1),
        }
    }
}

/// ストリーミングのヘルパー関数
pub struct StreamHelpers;

impl StreamHelpers {
    /// ストリームから完全なテキストを収集
    pub async fn collect_text(
        mut stream: impl futures::Stream<Item = Result<StreamEvent>> + Unpin,
    ) -> Result<String> {
        let mut text = String::new();

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::ContentDelta { text: delta } => {
                    text.push_str(&delta);
                }
                _ => {}
            }
        }

        Ok(text)
    }

    /// ストリームをタイムアウト付きで処理
    pub async fn with_timeout<F, Fut>(
        stream: impl futures::Stream<Item = Result<StreamEvent>> + Unpin,
        timeout_duration: Duration,
        handler: F,
    ) -> Result<()>
    where
        F: Fn(StreamEvent) -> Fut,
        Fut: futures::Future<Output = Result<()>>,
    {
        let mut stream = stream;

        while let Ok(Some(event)) = timeout(timeout_duration, stream.next()).await {
            handler(event?).await?;
        }

        Ok(())
    }

    /// ストリームを再試行付きで処理
    pub async fn with_retry<F>(
        create_stream: F,
        config: &StreamConfig,
    ) -> Result<crate::response::StreamResponse>
    where
        F: Fn() -> BoxFuture<'static, Result<crate::response::StreamResponse>>,
    {
        let mut attempts = 0;

        loop {
            match create_stream().await {
                Ok(stream) => return Ok(stream),
                Err(e) if attempts < config.max_reconnects && e.is_retryable() => {
                    attempts += 1;
                    warn!(
                        "Stream failed (attempt {}/{}), retrying in {:?}: {}",
                        attempts, config.max_reconnects, config.reconnect_delay, e
                    );
                    sleep(config.reconnect_delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_config_default() {
        let config = StreamConfig::default();
        assert_eq!(config.buffer_size, 100);
        assert_eq!(config.timeout, Duration::from_secs(300));
        assert_eq!(config.max_reconnects, 3);
        assert_eq!(config.reconnect_delay, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_collect_text() {
        let events = vec![
            Ok(StreamEvent::MessageStart {
                id: "msg-1".to_string(),
                model: "claude-3-opus".to_string(),
                usage: None,
            }),
            Ok(StreamEvent::ContentDelta {
                text: "Hello, ".to_string(),
            }),
            Ok(StreamEvent::ContentDelta {
                text: "world!".to_string(),
            }),
            Ok(StreamEvent::MessageStop {
                reason: "end_turn".to_string(),
                usage: Some(Usage::new(10, 20)),
            }),
        ];

        let stream = futures::stream::iter(events);
        let text = StreamHelpers::collect_text(stream).await.unwrap();
        assert_eq!(text, "Hello, world!");
    }
}
