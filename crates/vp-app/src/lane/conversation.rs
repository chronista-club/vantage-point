//! lane の **conversation session**（gui mode の chat — 構造化イベント購読 + prompt 投入）。
//!
//! terminal session と同型だが demand-driven（ChatPane の初回 submit で lazy spawn）。
//! `run()` が `HashMap<String, LaneConversation>` で持ち、`cmd_tx` 経由で submit / respond を送る。
//! 購読後の demand は初回も再接続も毎回撃ち、collapse 時は明示 unsubscribe（doc 60 §4）。
//! app/mod.rs から移設（棚卸し 項目 6 / 6-1 #10、2026-09-08）。

use std::time::Duration;

use tao::event_loop::EventLoopProxy;

use crate::daemon::conn::{SharedDaemonConn, SubscriptionOutcome};
use crate::events::AppEvent;

// =============================================================================
// Conversation gui (doc 32): per-lane conversation session — 構造化イベント購読 + prompt 投入
// =============================================================================
//
// terminal session と同型だが **demand-driven**: lane reconcile には結合させず、
// ChatPane を開いた lane で初回 submit された時に lazy spawn する (repo 側 host の
// lazy モデルと一致)。subscribe → submit の順で走るため取りこぼしなし。

/// Conversation session への command (WebView → repo)。
#[derive(Debug)]
pub(crate) enum ConversationCmd {
    /// プロンプト投入。 canvas channel 上り request `conversation_submit` で repo に送る。
    /// session（doc 50 P2）: None = focused（repo 側 payload_session_key の後方互換）。
    Submit {
        reply: tokio::sync::oneshot::Sender<crate::conversation_submission::SubmitReply>,
        prompt: String,
        session: Option<u32>,
        /// 添付画像（chat 入力欄への貼り付け）。空 = text だけ。
        images: Vec<serde_json::Value>,
    },
    /// doc 35 PR1: PromptCard 回答。 canvas channel 上り request `conversation_respond` で repo に送る。
    Respond {
        request_id: String,
        answers: Option<serde_json::Value>,
        behavior: Option<String>,
        message: Option<String>,
        session: Option<u32>,
    },
    /// doc 35 §5 / PR2: 実行中 turn の中断。 canvas channel 上り request `conversation_interrupt` で repo へ。
    Interrupt { session: Option<u32> },
    /// doc 35 §2.5 / PR3: permission mode 切替。 canvas channel 上り request `conversation_set_permission_mode` で repo へ。
    SetPermissionMode { mode: String, session: Option<u32> },
}

/// 1 lane の conversation session handle (event loop が保持)。map から remove で cmd_tx drop → 停止。
pub(crate) struct LaneConversation {
    pub(crate) cmd_tx: tokio::sync::mpsc::UnboundedSender<ConversationCmd>,
}

/// lane の conversation を Daemon "canvas" channel に乗せる per-lane session を spawn。
///
/// `repo/conversation/data/{lane_key}/event` を subscribe → repo host が emit する ConversationEvent を
/// `AppEvent::ConversationEvent` で event loop に流し、 cmd (submit) は同 channel の上り request
/// `conversation_submit` で repo に forward する (terminal session の gui 対応)。
pub(crate) fn spawn_conversation_session(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
) -> LaneConversation {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    rt_handle.spawn(conversation_session_loop(
        proxy, conn, repo_path, lane_key, cmd_rx,
    ));
    LaneConversation { cmd_tx }
}

/// conversation session の購読 → 再購読を司る long-lived ループ (terminal_session_loop と同型)。
async fn conversation_session_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ConversationCmd>,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_conversation_session(&proxy, &repo_path, &lane_key, &client, &mut cmd_rx).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("conversation session error: lane={}: {}", lane_key, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の conversation session: connect → `open_channel("canvas")` → subscribe(conversation pattern) →
/// recv (ConversationEvent) / cmd (submit) の select ループ (run_terminal_session と同型)。
async fn run_conversation_session(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    lane_key: &str,
    client: &unison::ProtocolClient,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ConversationCmd>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    let channel = client
        .open_channel("gui")
        .await
        .map_err(|e| format!("open gui channel (conversation): {}", e))?;
    let topic = format!(
        "repo/conversation/data/{}/event",
        lane_key.replace('/', "~")
    );
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path, "pattern": topic }),
        )
        .await
        .map_err(|e| format!("conversation subscribe handshake: {}", e))?;
    tracing::info!(
        "conversation session connected: lane={} topic={}",
        lane_key,
        topic
    );

    // subscribe 直後に engine 復活 + replay の demand を**毎回**明示的に撃つ。
    //
    // 背景: gui engine は demand-driven。本来は購読 0→1 を daemon の TopicRouter demand hook が
    // 検知して repo に conversation_demand_start を reverse-route し ensure_chat_engine で復活させる経路が
    // あるが、この edge は 2 つのレースで取りこぼされる:
    // (a) full restart 直後の「Daemon 復帰 / repo 再登録 / surface 再購読 / router 生成」の多者間レース
    //     （refire_active_demands の救済も順序に脆い）→ submit まで engine 不在（⚠/💤 固着）
    // (b) **前任 GUI の残留購読**: pkill された旧 GUI の QUIC 購読が cleanup される前に新 GUI が
    //     subscribe すると 1→2 で edge が立たず、demand が発火しない = **chat が空で始まる**
    //     （transcript replay 不発、2026-07-24 実測 — swap 連打で毎回再現）。
    // ここで forward request として明示的に撃つと forward_to_sp_control が **request 時に repo を
    // lookup** するためレースに強い。冪等なので二重発火は無害: ensure_chat_engine は既起動なら
    // no-op / transcript replay は ReplayStart の clear-prefix で収束する（自動 hook と重なっても
    // 一瞬の再描画のみ）。旧実装は「初回は自動 hook に任せる」と reconnect 限定にしていたが、
    // (b) は初回 attach でこそ起きるため撃ち分けをやめた。
    if let Err(e) = channel
        .request::<serde_json::Value, serde_json::Value>(
            "conversation_demand_start",
            &serde_json::json!({ "lane": lane_key }),
        )
        .await
    {
        tracing::warn!(
            "conversation attach demand_start 失敗（次 submit の self-heal に委ねる, lane={}）: {}",
            lane_key,
            e
        );
    }

    loop {
        tokio::select! {
            recvd = channel.recv() => {
                let msg = match recvd {
                    Ok(m) => m,
                    Err(_) => return Ok(SubscriptionOutcome::Disconnected),
                };
                if msg.msg_type != MessageType::Event || msg.method != "pane" {
                    continue;
                }
                let payload = match msg.payload_as_value() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // RepoMessage::ConversationEvent { lane, session, event } の生 JSON。 event と
                // session を抜いて lane_key 付きで JS に渡す (lane は subscription で確定済)。
                // doc 38 Phase 2: session を落とさず通す（旧 sender / N=1 では default 1）。
                if let Some(event) = payload.get("event") {
                    let session = payload
                        .get("session")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1) as u32;
                    if proxy
                        .send_event(AppEvent::ConversationEvent {
                            lane: lane_key.to_string(),
                            event: event.clone(),
                            session,
                        })
                        .is_err()
                    {
                        return Ok(SubscriptionOutcome::AppClosing);
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ConversationCmd::Submit { prompt, session, images, reply }) => {
                        if reply.is_closed() { continue; }
                        // session: None は JSON null になり、repo 側 payload_session_key が
                        // focused に解決する（旧 UI / 旧 SP との後方互換）。
                        let result = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_submit",
                                &serde_json::json!({
                                    "lane": lane_key, "prompt": prompt, "session": session,
                                    "images": images,
                                }),
                            )
                            .await;
                        let _ = reply.send(result);
                    }
                    Some(ConversationCmd::Respond { request_id, answers, behavior, message, session }) => {
                        // allow/deny のどちらの形も同 request に載せる（repo 側が behavior で分岐）。
                        let mut req = serde_json::json!({
                            "lane": lane_key, "request_id": request_id, "session": session,
                        });
                        if let Some(a) = answers {
                            req["answers"] = a;
                        }
                        if let Some(b) = behavior {
                            req["behavior"] = serde_json::Value::String(b);
                        }
                        if let Some(m) = message {
                            req["message"] = serde_json::Value::String(m);
                        }
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>("conversation_respond", &req)
                            .await;
                    }
                    Some(ConversationCmd::Interrupt { session }) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_interrupt",
                                &serde_json::json!({ "lane": lane_key, "session": session }),
                            )
                            .await;
                    }
                    Some(ConversationCmd::SetPermissionMode { mode, session }) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_set_permission_mode",
                                &serde_json::json!({
                                    "lane": lane_key, "mode": mode, "session": session,
                                }),
                            )
                            .await;
                    }
                    // cmd_tx が drop された = 呼び手が session を畳んだ（repo collapse /
                    // lane 削除 / app 終了）。**明示的に購読を降りてから**抜ける。
                    //
                    // ⚠️ channel を drop するだけでは daemon の demand は下がらない。
                    // daemon が `router.unsubscribe` を呼ぶのは受信ループが切れた時だけで、
                    // ここで黙って return すると「GUI は見ていないのに demand は立ったまま」
                    // になり engine の idle teardown が永久に発火しない（2026-08-29 実測:
                    // 畳んだ 7 repo の engine が 11 分経っても残っていた）。
                    None => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "unsubscribe",
                                &serde_json::json!({}),
                            )
                            .await;
                        tracing::info!(
                            "conversation session disconnected: lane={lane_key} (unsubscribed)"
                        );
                        return Ok(SubscriptionOutcome::AppClosing);
                    }
                }
            }
        }
    }
}
