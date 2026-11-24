//! lane の **conversation session**（gui mode の chat — 構造化イベント購読 + prompt 投入）。
//!
//! terminal session と同型だが demand-driven（ChatPane の初回 submit で lazy spawn）。
//! `run()` が `HashMap<String, LaneConversation>` で持ち、`cmd_tx` 経由で submit / respond を送る。
//! 購読後の demand は初回も再接続も毎回撃ち、collapse 時は明示 unsubscribe（doc 60 §4）。
//! app/mod.rs から移設（棚卸し 項目 6 / 6-1 #10、2026-09-08）。

use crate::event_proxy::EventLoopProxy;

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
        client_user_message_id: String,
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

/// 停止（`conversation_interrupt`）が repo に届かなかった時に、その session の chat へ出す event
/// （ConversationEvent の wire 形）。「応答中」を下ろし、届かなかった理由を見せる。
///
/// webview は応答中の間（Claude と Codex 以外の engine）、次の発話を送信待ちに溜めて turn の
/// 終わりまで送らない。停止が届かずに応答中が残ると、停止も次の発話も効かなくなる（2026-11-24）。
/// `error` は turn の終わりの kind なので、停止の印も下ろす（「停止しました」は出さない — 停止は
/// 届いていない）。購読している時は、engine が実は動いていれば続きの event が streaming を立て直す
/// （購読していない時は、張り直すまで event は来ない）。
pub(crate) fn interrupt_undelivered_event(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "error",
        "message": format!("停止を engine に届けられませんでした（{reason}）"),
    })
}

/// `conversation_interrupt` の応答から、届かなかった理由を読む（届いたなら None）。
///
/// 失敗は 2 つの形で届く: daemon の Error frame（#1286 以降、request の Err）と、それより前の
/// daemon の成功の frame の `{"error": …}`（[`crate::daemon::control::rpc_error`]）。GUI だけ
/// 先に更新した時（`app:swap` で daemon を立て直さなかった等）も、どちらも失敗として読む。
pub(crate) fn interrupt_failure<E: std::fmt::Display>(
    response: Result<serde_json::Value, E>,
) -> Option<String> {
    match response {
        Ok(value) => crate::daemon::control::rpc_error(&value),
        Err(e) => Some(e.to_string()),
    }
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
                    Some(ConversationCmd::Submit { prompt, session, images, reply, client_user_message_id }) => {
                        if reply.is_closed() { continue; }
                        // session: None は JSON null になり、repo 側 payload_session_key が
                        // focused に解決する（旧 UI / 旧 SP との後方互換）。
                        let result = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_submit",
                                &serde_json::json!({
                                    "lane": lane_key, "prompt": prompt, "session": session,
                                    "images": images,
                                    "client_user_message_id": client_user_message_id,
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
                        // engine が居ない時は repo が EngineExited を流して Ok を返す。失敗が返るのは
                        // 届かなかった時（request の最中の切断 / 存在しない lane・session / repo が
                        // 居ない）。黙って捨てると「応答中」が残るので、理由を添えて turn を閉じる。
                        // 再接続を待っている間の停止は、この loop が cmd_rx を読まないので溜まり、
                        // 再接続の後に送られる（再接続の replay が真の状態を戻す）。
                        let response = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_interrupt",
                                &serde_json::json!({ "lane": lane_key, "session": session }),
                            )
                            .await;
                        if let Some(reason) = interrupt_failure(response) {
                            tracing::warn!("conversation_interrupt 失敗: lane={lane_key}: {reason}");
                            if proxy
                                .send_event(AppEvent::ConversationEvent {
                                    lane: lane_key.to_string(),
                                    // submit の結果と同じ既定（webview は session を必ず付けて送る）。
                                    session: session.unwrap_or(1),
                                    event: interrupt_undelivered_event(&reason),
                                })
                                .is_err()
                            {
                                return Ok(SubscriptionOutcome::AppClosing);
                            }
                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 停止が repo に届かない時に chat へ出す event は、webview が「turn の終わり」として畳む kind で、
    /// ConversationEvent の wire 形（`kind` + `message`）。turn の終わりの kind でないと、
    /// 「応答中」も送信待ちの発話も残ったままになる（2026-11-24）。
    #[test]
    fn interrupt_fallback_event_closes_the_turn_in_the_webview() {
        // webview の判定の正本（`session-now-bridge.ts` の TURN_CLOSING_KINDS）と突き合わせる。
        let closing = include_str!("../../webview/session-now-bridge.ts")
            .lines()
            .find(|line| line.contains("export const TURN_CLOSING_KINDS"))
            .expect("TURN_CLOSING_KINDS の定義");

        let event = interrupt_undelivered_event("connection lost");
        assert_eq!(event["kind"], "error", "届かなかった理由を見せる");
        assert!(
            event["message"]
                .as_str()
                .is_some_and(|m| m.contains("connection lost")),
            "{event}"
        );
        assert!(
            closing.contains(r#""error""#),
            "error は turn を閉じる kind ではない: {closing}"
        );
    }

    /// 停止の失敗は 2 つの形で届く: 新しい daemon（#1286 以降）は Error frame（request の Err）、
    /// それより前の daemon は成功の frame の `{"error": …}`。GUI だけ先に更新した時も
    /// 「応答中」が残らないよう、どちらも失敗として読む。
    #[test]
    fn interrupt_failure_reads_both_daemon_error_forms() {
        // Error frame の Err の Display の実物の形（`app/on_sidebar.rs` の lanes/create の例から、呼び手が
        // 足した `daemon-control.lanes/create: ` を除いた部分）。
        const ERROR_FRAME: &str = r#"Protocol error: Request error: {"error":"conversation_interrupt: session が存在しません（addr=vp/lane/x, session=9）"}"#;
        assert_eq!(
            interrupt_failure(Err::<serde_json::Value, _>(ERROR_FRAME)).as_deref(),
            Some(ERROR_FRAME)
        );
        assert_eq!(
            interrupt_failure(Ok::<_, String>(
                serde_json::json!({ "error": "conversation_interrupt: Lane not found" })
            ))
            .as_deref(),
            Some("conversation_interrupt: Lane not found")
        );
        assert_eq!(
            interrupt_failure(Ok::<_, String>(
                serde_json::json!({ "status": "ok", "lane": "vp/lane/x" })
            )),
            None,
            "届いた停止は失敗にしない"
        );
    }
}
