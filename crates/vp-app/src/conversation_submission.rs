//! GUI-local submit acknowledgement. No prompt or attachment enters this event.
use serde_json::{Value, json};
use tokio::sync::oneshot;
use unison::network::NetworkError;

pub type SubmitReply = Result<Value, NetworkError>;

pub fn submit_result(request_id: &str, result: SubmitReply) -> Value {
    let error = result.err().map(|error| match &error {
        // Unison 1.9 represents a remote Error frame with this prefix. Other
        // Protocol errors (e.g. a closed channel) do not prove remote rejection.
        NetworkError::Protocol(message) if message.starts_with("Request error: ") => {
            format!(
                "送信に失敗しました: {}",
                &message["Request error: ".len()..]
            )
        }
        _ => format!(
            "受付結果を確認できませんでした。会話の応答を確認してから再送してください: {error}"
        ),
    });
    json!({ "kind": "submit_result", "request_id": request_id, "error": error })
}

/// Includes time queued while disconnected. Dropping the receiver on timeout
/// lets the session loop discard an expired command before it reaches an engine.
pub async fn await_submit_result(request_id: &str, reply: oneshot::Receiver<SubmitReply>) -> Value {
    let result = match tokio::time::timeout(std::time::Duration::from_secs(30), reply).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(NetworkError::NotConnected),
        Err(_) => Err(NetworkError::Timeout),
    };
    submit_result(request_id, result)
}
