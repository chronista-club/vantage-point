//! 偽の ACP agent（メモリの中の JSON-RPC の相手）— 実 CLI 無しで [`super::AcpAgentHost`] と pump を確かめる。
//!
//! 共有 crate の `tests/fake_agent.rs` と同じ作り（SDK の `Channel::duplex()` の片側で、生の
//! JSON-RPC を返す）。違いは、turn の中身をテストが 1 手ずつ進められること（[`Step`]）—
//! 「turn の最中に履歴を配り直す」場面で、どこまで流れた時点かをテストが決める。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{Channel, RawJsonRpcMessage, RawJsonRpcParams, TransportFrame};
use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// turn の中でテストが進める 1 手。
pub(crate) enum Step {
    /// `session/update` を 1 件流す。
    Update(Value),
    /// 待たせている `session/prompt` のうち、いちばん古い 1 本に応答する（turn の終わり。
    /// steer で複数が待っている時は、engine が前の prompt から返すのと同じ並び）。
    EndTurn,
    /// 接続を閉じる（engine が終了した）。
    Close,
    /// 待たせている `session/set_config_option` に応答する（[`Script::hold_config`]）。
    ReleaseConfig,
    /// agent から `elicitation/create`（構造化質問）を送る。`sessionId` は偽の agent が埋める。
    /// client の応答は log に `elicitation/response` として残る。
    Elicit(Value),
}

/// 偽の agent が送る `elicitation/create` の JSON-RPC id。
const ELICIT_ID: &str = "elicit-1";

/// 接続の台本。
#[derive(Clone, Default)]
pub(crate) struct Script {
    /// `session/load` の応答の前に流す update（過去の会話）。
    pub replay: Vec<Value>,
    /// `session/new` / `session/load` / `session/set_config_option` が返す configOptions。
    pub config: Vec<Value>,
    /// `session/load` を失敗させる。
    pub load_error: bool,
    /// `session/set_config_option` の応答を、テストの [`Step::ReleaseConfig`] まで待たせる
    /// （設定の当て直しに時間がかかる engine）。
    pub hold_config: bool,
    /// model の切替で返す別の設定面（Grok の候補依存を再現）。
    pub config_after_model: Option<Vec<Value>>,
    /// 指定軸の切替を拒否する。
    pub reject_config: Option<String>,
    /// 成功応答でも現在値を変えない engine を再現する。
    pub ignore_config: bool,
}

/// 偽の agent が受けた要求と通知（method, params）。
pub(crate) type Log = Arc<Mutex<Vec<(String, Value)>>>;

/// テストが握る側。
pub(crate) struct FakeAgent {
    pub steps: mpsc::UnboundedSender<Step>,
    pub log: Log,
}

impl FakeAgent {
    /// 偽の agent と、host に渡す connector を作る。
    pub(crate) fn new(script: Script) -> (Self, FakeConnector) {
        let (steps, rx) = mpsc::unbounded_channel();
        let log = Log::default();
        (
            Self {
                steps,
                log: Arc::clone(&log),
            },
            FakeConnector {
                script,
                steps: Arc::new(tokio::sync::Mutex::new(rx)),
                log,
            },
        )
    }

    pub(crate) fn update(&self, update: Value) {
        self.steps.send(Step::Update(update)).expect("偽の agent");
    }

    pub(crate) fn end_turn(&self) {
        self.steps.send(Step::EndTurn).expect("偽の agent");
    }

    pub(crate) fn close(&self) {
        self.steps.send(Step::Close).expect("偽の agent");
    }

    pub(crate) fn release_config(&self) {
        self.steps.send(Step::ReleaseConfig).expect("偽の agent");
    }

    pub(crate) fn elicit(&self, form: Value) {
        self.steps.send(Step::Elicit(form)).expect("偽の agent");
    }

    /// `method` の要求か通知が届くまで待つ（turn が始まる前に update を流さないため）。
    pub(crate) async fn wait_for(&self, method: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !self.methods().iter().any(|m| m == method) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "偽の agent に {method} が届かない。届いたもの: {:?}",
                self.methods()
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// 受けた method の並び。
    pub(crate) fn methods(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }
}

pub(crate) fn agent_text(text: &str) -> Value {
    json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})
}

pub(crate) fn user_text(text: &str) -> Value {
    json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": text}})
}

/// host に渡す接続の作り方（立て直しのたびに新しい偽の agent をつなぐ）。
pub(crate) struct FakeConnector {
    script: Script,
    steps: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Step>>>,
    log: Log,
}

impl agent_chat_acp::Connector for FakeConnector {
    type Transport = Channel;

    fn connect(&mut self) -> Result<Channel, String> {
        let (client, agent) = Channel::duplex();
        tokio::spawn(run(
            agent,
            self.script.clone(),
            Arc::clone(&self.steps),
            Arc::clone(&self.log),
        ));
        Ok(client)
    }

    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
}

fn params(params: Option<&RawJsonRpcParams>) -> Value {
    params
        .cloned()
        .map_or(Value::Null, RawJsonRpcParams::into_value)
}

fn send(peer: &Channel, message: RawJsonRpcMessage) {
    let _ = peer.tx.unbounded_send(TransportFrame::Single(message));
}

fn send_update(peer: &Channel, session: &str, update: &Value) {
    let message = RawJsonRpcMessage::notification(
        "session/update".to_owned(),
        json!({"sessionId": session, "update": update}),
    )
    .expect("update");
    send(peer, message);
}

async fn run(
    mut peer: Channel,
    mut script: Script,
    steps: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Step>>>,
    log: Log,
) {
    // 接続が生きている間、この接続が手を受ける（立て直しの後は次の接続が受ける）。
    let mut steps = steps.lock().await;
    let mut session = "new-1".to_string();
    let mut pending_prompts = VecDeque::new();
    let mut pending_config = None;
    loop {
        let frame = tokio::select! {
            frame = peer.rx.next() => match frame {
                Some(frame) => frame,
                None => return,
            },
            Some(step) = steps.recv() => {
                match step {
                    Step::Close => return,
                    Step::ReleaseConfig => {
                        if let Some(id) = pending_config.take() {
                            send(
                                &peer,
                                RawJsonRpcMessage::response(
                                    id,
                                    Ok(json!({"configOptions": script.config})),
                                ),
                            );
                        }
                    }
                    Step::Update(update) => send_update(&peer, &session, &update),
                    Step::Elicit(mut form) => {
                        form["sessionId"] = json!(session);
                        let request = RawJsonRpcMessage::request(
                            "elicitation/create".to_owned(),
                            form,
                            ELICIT_ID.to_string(),
                        )
                        .expect("elicitation");
                        send(&peer, request);
                    }
                    Step::EndTurn => {
                        if let Some(id) = pending_prompts.pop_front() {
                            send(
                                &peer,
                                RawJsonRpcMessage::response(id, Ok(json!({"stopReason": "end_turn"}))),
                            );
                        }
                    }
                }
                continue;
            }
        };
        let TransportFrame::Single(message) = frame else {
            continue;
        };
        match message {
            RawJsonRpcMessage::Request(request) => {
                let p = params(request.params.as_ref());
                log.lock()
                    .unwrap()
                    .push((request.method.to_string(), p.clone()));
                let result = match &*request.method {
                    "initialize" => Ok(
                        json!({"protocolVersion": 1, "agentCapabilities": {"loadSession": true}}),
                    ),
                    "session/new" => {
                        session = "new-1".to_string();
                        Ok(json!({"sessionId": session, "configOptions": script.config}))
                    }
                    "session/load" if script.load_error => {
                        Err(agent_client_protocol::Error::invalid_params())
                    }
                    "session/load" => {
                        session = p["sessionId"].as_str().unwrap_or_default().to_string();
                        for update in &script.replay {
                            send_update(&peer, &session, update);
                        }
                        Ok(json!({"configOptions": script.config}))
                    }
                    "session/set_config_option"
                        if script.reject_config.as_deref() == p["configId"].as_str() =>
                    {
                        Err(agent_client_protocol::Error::invalid_params())
                    }
                    "session/set_config_option" => {
                        if p["configId"] == "model"
                            && let Some(options) = &script.config_after_model
                        {
                            script.config = options.clone();
                        }
                        for option in &mut script.config {
                            if option["id"] == p["configId"] && !script.ignore_config {
                                option["currentValue"] = p["value"].clone();
                            }
                        }
                        if script.hold_config {
                            pending_config = Some(request.id.clone());
                            continue;
                        }
                        Ok(json!({"configOptions": script.config}))
                    }
                    "session/prompt" => {
                        // 応答はテストの `EndTurn` まで待たせる。
                        pending_prompts.push_back(request.id.clone());
                        continue;
                    }
                    _ => Err(agent_client_protocol::Error::method_not_found()),
                };
                send(&peer, RawJsonRpcMessage::response(request.id, result));
            }
            RawJsonRpcMessage::Notification(notification) => {
                log.lock().unwrap().push((
                    notification.method.to_string(),
                    params(notification.params.as_ref()),
                ));
                if &*notification.method == "session/cancel" {
                    for id in pending_prompts.drain(..) {
                        send(
                            &peer,
                            RawJsonRpcMessage::response(id, Ok(json!({"stopReason": "cancelled"}))),
                        );
                    }
                }
            }
            RawJsonRpcMessage::Response(response) => {
                // 偽の agent が client へ送る要求は今は elicitation だけだが、名札は id で決める
                // （別の要求を足した時に、テストが他の応答を拾わないように）。
                let value = serde_json::to_value(&response).unwrap_or_default();
                if value["id"] == ELICIT_ID {
                    log.lock()
                        .unwrap()
                        .push(("elicitation/response".to_string(), value));
                }
            }
        }
    }
}
