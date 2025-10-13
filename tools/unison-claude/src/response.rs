//! レスポンスモジュール
//!
//! Claude APIからのレスポンスを処理

use crate::{
    error::{ClaudeError, Result},
    message::Role,
    model::Model,
};
use futures::{Stream, StreamExt};
use pin_project::pin_project;
use serde::{Deserialize, Serialize};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// チャットレスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// レスポンスID
    pub id: String,

    /// 使用されたモデル
    pub model: String,

    /// 生成されたコンテンツ
    pub content: String,

    /// メッセージの役割（常にAssistant）
    pub role: Role,

    /// 停止理由
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,

    /// 使用統計
    pub usage: Usage,
}

impl ChatResponse {
    /// レスポンスの総トークン数を取得
    pub fn total_tokens(&self) -> u32 {
        self.usage.input_tokens + self.usage.output_tokens
    }

    /// レスポンスが完了しているかどうか
    pub fn is_complete(&self) -> bool {
        self.stop_reason.is_some()
    }

    /// レスポンスが最大長で停止したかどうか
    pub fn stopped_at_max_tokens(&self) -> bool {
        self.stop_reason.as_deref() == Some("max_tokens")
    }

    /// コンテンツの長さ（文字数）を取得
    pub fn content_length(&self) -> usize {
        self.content.len()
    }

    /// 推定コストを計算（概算値）
    pub fn estimated_cost(&self, model: Model) -> EstimatedCost {
        EstimatedCost::calculate(model, self.usage.input_tokens, self.usage.output_tokens)
    }
}

/// 使用統計
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Usage {
    /// 入力トークン数
    pub input_tokens: u32,

    /// 出力トークン数
    pub output_tokens: u32,
}

impl Usage {
    /// 新しい使用統計を作成
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    /// 総トークン数を取得
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    /// 別の使用統計と合計
    pub fn add(&self, other: &Usage) -> Self {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
        }
    }
}

impl Default for Usage {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

/// ストリーミングレスポンス
#[pin_project]
pub struct StreamResponse {
    #[pin]
    inner: ReceiverStream<Result<StreamEvent>>,
    accumulated_content: String,
    usage: Usage,
    id: Option<String>,
    model: Option<String>,
}

impl StreamResponse {
    /// 新しいストリーミングレスポンスを作成（内部用）
    pub(crate) fn new(receiver: mpsc::Receiver<Result<StreamEvent>>) -> Self {
        Self {
            inner: ReceiverStream::new(receiver),
            accumulated_content: String::new(),
            usage: Usage::default(),
            id: None,
            model: None,
        }
    }

    /// 現在までの累積コンテンツを取得
    pub fn accumulated_content(&self) -> &str {
        &self.accumulated_content
    }

    /// 現在までの使用統計を取得
    pub fn current_usage(&self) -> Usage {
        self.usage
    }

    /// ストリームを完全に消費して最終レスポンスを取得
    pub async fn to_complete_response(mut self) -> Result<ChatResponse> {
        let mut content = String::new();
        let mut stop_reason = None;
        let mut id = None;
        let mut model = None;
        let mut usage = Usage::default();

        while let Some(event) = self.next().await {
            match event? {
                StreamEvent::ContentDelta { text } => {
                    content.push_str(&text);
                }
                StreamEvent::MessageStart { id: msg_id, model: msg_model, .. } => {
                    id = Some(msg_id);
                    model = Some(msg_model);
                }
                StreamEvent::MessageStop { reason, usage: final_usage } => {
                    stop_reason = Some(reason);
                    if let Some(final_usage) = final_usage {
                        usage = final_usage;
                    }
                }
                _ => {}
            }
        }

        Ok(ChatResponse {
            id: id.ok_or_else(|| ClaudeError::Streaming("Missing message ID".to_string()))?,
            model: model.ok_or_else(|| ClaudeError::Streaming("Missing model".to_string()))?,
            content,
            role: Role::Assistant,
            stop_reason,
            usage,
        })
    }
}

impl Stream for StreamResponse {
    type Item = Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        match this.inner.poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                // イベントを処理して内部状態を更新
                match &event {
                    StreamEvent::ContentDelta { text } => {
                        this.accumulated_content.push_str(text);
                    }
                    StreamEvent::MessageStart { id, model, usage } => {
                        *this.id = Some(id.clone());
                        *this.model = Some(model.clone());
                        if let Some(usage) = usage {
                            *this.usage = *usage;
                        }
                    }
                    StreamEvent::MessageStop { usage, .. } => {
                        if let Some(usage) = usage {
                            *this.usage = *usage;
                        }
                    }
                    _ => {}
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// ストリーミングイベント
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// メッセージ開始
    MessageStart {
        /// メッセージID
        id: String,
        /// モデル
        model: String,
        /// 初期使用統計
        usage: Option<Usage>,
    },

    /// コンテンツのデルタ（差分）
    ContentDelta {
        /// テキストの差分
        text: String,
    },

    /// コンテンツブロック開始
    ContentBlockStart {
        /// ブロックのインデックス
        index: usize,
        /// ブロックのタイプ
        content_type: String,
    },

    /// コンテンツブロック停止
    ContentBlockStop {
        /// ブロックのインデックス
        index: usize,
    },

    /// メッセージ停止
    MessageStop {
        /// 停止理由
        reason: String,
        /// 最終使用統計
        usage: Option<Usage>,
    },

    /// エラーイベント
    Error {
        /// エラーメッセージ
        message: String,
        /// エラーコード
        code: Option<String>,
    },

    /// Ping（キープアライブ）
    Ping,
}

/// 推定コスト
#[derive(Debug, Clone, Copy)]
pub struct EstimatedCost {
    /// 入力コスト（USD）
    pub input_cost: f64,
    /// 出力コスト（USD）
    pub output_cost: f64,
    /// 総コスト（USD）
    pub total_cost: f64,
}

impl EstimatedCost {
    /// コストを計算
    pub fn calculate(model: Model, input_tokens: u32, output_tokens: u32) -> Self {
        let (input_rate, output_rate) = Self::get_rates(model);

        let input_cost = (input_tokens as f64 / 1_000_000.0) * input_rate;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_rate;
        let total_cost = input_cost + output_cost;

        Self {
            input_cost,
            output_cost,
            total_cost,
        }
    }

    /// モデルのレートを取得（USD per 1M tokens）
    fn get_rates(model: Model) -> (f64, f64) {
        match model {
            Model::Claude3Opus => (15.0, 75.0),
            Model::Claude35Sonnet => (3.0, 15.0),
            Model::Claude3Sonnet => (3.0, 15.0),
            Model::Claude3Haiku => (0.25, 1.25),
            Model::Claude21 => (8.0, 24.0),
            Model::Claude20 => (8.0, 24.0),
            Model::ClaudeInstant12 => (0.8, 2.4),
        }
    }

    /// フォーマットされたコスト文字列を取得
    pub fn format(&self) -> String {
        format!(
            "Input: ${:.6}, Output: ${:.6}, Total: ${:.6}",
            self.input_cost, self.output_cost, self.total_cost
        )
    }
}

/// バッチレスポンス（複数のレスポンスをまとめて処理）
#[derive(Debug, Clone)]
pub struct BatchResponse {
    /// 個々のレスポンス
    pub responses: Vec<ChatResponse>,
    /// 総使用統計
    pub total_usage: Usage,
    /// エラーがあった場合のインデックス
    pub errors: Vec<(usize, ClaudeError)>,
}

impl BatchResponse {
    /// 新しいバッチレスポンスを作成
    pub fn new() -> Self {
        Self {
            responses: Vec::new(),
            total_usage: Usage::default(),
            errors: Vec::new(),
        }
    }

    /// レスポンスを追加
    pub fn add_response(&mut self, response: ChatResponse) {
        self.total_usage = self.total_usage.add(&response.usage);
        self.responses.push(response);
    }

    /// エラーを追加
    pub fn add_error(&mut self, index: usize, error: ClaudeError) {
        self.errors.push((index, error));
    }

    /// 成功したレスポンスの数
    pub fn success_count(&self) -> usize {
        self.responses.len()
    }

    /// エラーの数
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    /// 全体の成功率
    pub fn success_rate(&self) -> f64 {
        let total = self.success_count() + self.error_count();
        if total == 0 {
            0.0
        } else {
            self.success_count() as f64 / total as f64
        }
    }
}

impl Default for BatchResponse {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_arithmetic() {
        let usage1 = Usage::new(100, 200);
        let usage2 = Usage::new(50, 100);
        let total = usage1.add(&usage2);

        assert_eq!(total.input_tokens, 150);
        assert_eq!(total.output_tokens, 300);
        assert_eq!(total.total_tokens(), 450);
    }

    #[test]
    fn test_estimated_cost() {
        let cost = EstimatedCost::calculate(Model::Claude3Haiku, 1000, 2000);

        // Claude 3 Haiku: $0.25/1M input, $1.25/1M output
        let expected_input = 1000.0 / 1_000_000.0 * 0.25;
        let expected_output = 2000.0 / 1_000_000.0 * 1.25;

        assert!((cost.input_cost - expected_input).abs() < 0.000001);
        assert!((cost.output_cost - expected_output).abs() < 0.000001);
        assert!((cost.total_cost - (expected_input + expected_output)).abs() < 0.000001);
    }

    #[test]
    fn test_chat_response_helpers() {
        let response = ChatResponse {
            id: "msg-123".to_string(),
            model: "claude-3-opus-20240229".to_string(),
            content: "Hello, world!".to_string(),
            role: Role::Assistant,
            stop_reason: Some("end_turn".to_string()),
            usage: Usage::new(10, 20),
        };

        assert_eq!(response.total_tokens(), 30);
        assert!(response.is_complete());
        assert!(!response.stopped_at_max_tokens());
        assert_eq!(response.content_length(), 13);
    }

    #[test]
    fn test_batch_response() {
        let mut batch = BatchResponse::new();

        let response1 = ChatResponse {
            id: "msg-1".to_string(),
            model: "claude-3-opus-20240229".to_string(),
            content: "Response 1".to_string(),
            role: Role::Assistant,
            stop_reason: Some("end_turn".to_string()),
            usage: Usage::new(10, 20),
        };

        let response2 = ChatResponse {
            id: "msg-2".to_string(),
            model: "claude-3-opus-20240229".to_string(),
            content: "Response 2".to_string(),
            role: Role::Assistant,
            stop_reason: Some("end_turn".to_string()),
            usage: Usage::new(15, 25),
        };

        batch.add_response(response1);
        batch.add_response(response2);
        batch.add_error(2, ClaudeError::Timeout(std::time::Duration::from_secs(30)));

        assert_eq!(batch.success_count(), 2);
        assert_eq!(batch.error_count(), 1);
        assert_eq!(batch.total_usage.input_tokens, 25);
        assert_eq!(batch.total_usage.output_tokens, 45);
        assert!((batch.success_rate() - 0.6667).abs() < 0.001);
    }
}
