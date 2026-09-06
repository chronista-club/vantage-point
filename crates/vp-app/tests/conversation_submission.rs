use serde_json::json;
use unison::network::NetworkError;
use vp_app::conversation_submission::submit_result;

#[test]
fn reports_rejection_without_losing_request_identity() {
    let event = submit_result(
        "request-7",
        Err(NetworkError::Protocol(
            "Request error: conversation_submit: engine missing".into(),
        )),
    );
    assert_eq!(event["kind"], "submit_result");
    assert_eq!(event["request_id"], "request-7");
    assert!(event["error"].as_str().unwrap().contains("engine missing"));
    assert!(!event["error"].as_str().unwrap().contains("受付結果"));
}

#[test]
fn transport_failure_is_uncertain_and_success_is_not_an_error() {
    let event = submit_result("request-8", Err(NetworkError::Timeout));
    assert!(event["error"].as_str().unwrap().contains("受付結果"));
    assert_eq!(
        submit_result("request-9", Ok(json!({"status":"ok"})))["error"],
        json!(null)
    );
}

#[tokio::test]
async fn a_dropped_session_returns_feedback_instead_of_hanging() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    drop(sender);
    let event = vp_app::conversation_submission::await_submit_result("closed", receiver).await;
    assert_eq!(event["request_id"], "closed");
    assert!(event["error"].is_string());
}

#[tokio::test]
async fn expired_queued_submission_cannot_be_delivered_after_reconnect() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let event = vp_app::conversation_submission::await_submit_result("expired", receiver).await;
    assert!(
        sender.is_closed(),
        "session loop must skip this command on reconnect"
    );
    assert_eq!(event["request_id"], "expired");
    assert!(event["error"].as_str().unwrap().contains("受付結果"));
}
