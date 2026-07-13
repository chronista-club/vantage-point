//! Unison QUIC サーバー
//!
//! MCP <-> Process 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP と同一ポート番号を使う。 HTTP は TCP・QUIC は UDP で OS レベルの
//! ポート名前空間が独立しているため衝突しない (`QUIC_PORT_OFFSET = 0`)。
//!
//! "process" チャネルですべての操作を統一:
//! - show / clear / toggle_pane / split_pane / close_pane
//! - watch_file / unwatch_file

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::state::AppState;
use crate::protocol::ProcessMessage;

/// QUIC ポートのオフセット（HTTP ポートからの差分）
/// TCP (HTTP) と UDP (QUIC) は OS レベルで独立 → 同一ポートで共存可能
pub const QUIC_PORT_OFFSET: u16 = 0;

/// recv_raw の最大フレームサイズ（64 KiB）
const MAX_RAW_FRAME_SIZE: usize = 64 * 1024;

/// UnwatchFile リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct UnwatchFileRequest {
    pane_id: String,
}

// =============================================================================
// Process チャネル ハンドラー
// =============================================================================

/// ProcessMessage を受け取って broadcast + Msgbox 配信する汎用ハンドラー
///
/// MCP → QUIC → ここ の経路では、MCP が ProcessMessage をそのままシリアライズして送る。
/// HTTP ハンドラ（health.rs の show_handler 等）と同じ ProcessMessage 形式を受ける。
///
/// 配信先:
/// 1. Hub broadcast → WebSocket → Canvas（既存）
/// 2. Msgbox "protocol" → PP Capability（VP-24）
fn handle_process_message(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let msg: ProcessMessage = serde_json::from_value(payload.clone())
        .map_err(|e| format!("Invalid ProcessMessage: {}", e))?;

    // 1. Hub broadcast → WebSocket → Canvas（既存経路）
    // TopicRouter が Hub ブリッジ経由で自動的に retained に保存するため、
    // 明示的なキャッシュは不要。Hub に broadcast するだけ。
    state.hub.broadcast(msg);

    // NOTE: Msgbox 経由の配信は ProtocolCapability 側の受信ループ実装後に追加（VP-24）
    // 現在は Hub broadcast のみで Canvas に配信。

    Ok(serde_json::json!({"status": "ok"}))
}

/// watch_file メソッドのハンドラー
async fn handle_watch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let config: crate::file_watcher::WatchConfig = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid watch_file payload: {}", e))?;

    let pane_id = config.pane_id.clone();

    state
        .file_watchers
        .lock()
        .await
        .start_watch(config, state.hub.clone())
        .map_err(|e| format!("watch_file 開始失敗: {}", e))?;

    Ok(serde_json::json!({"status": "ok", "pane_id": pane_id}))
}

/// unwatch_file メソッドのハンドラー
async fn handle_unwatch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: UnwatchFileRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid unwatch_file payload: {}", e))?;

    state.file_watchers.lock().await.stop_watch(&req.pane_id);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

// =============================================================================
// ProcessRunner ハンドラー
// =============================================================================

/// プロセス起動
async fn handle_process_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::process::process_runner::RunParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    let process_id = crate::process::process_runner::process_run(
        &state.process_registry,
        &params,
        &state.project_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// プロセス停止
async fn handle_process_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::process::process_runner::process_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// コード注入
async fn handle_process_inject(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::process::process_runner::InjectParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    crate::process::process_runner::process_inject(&state.process_registry, &params).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// プロセス一覧
async fn handle_process_list(state: &AppState) -> Result<serde_json::Value, String> {
    let processes = state.process_registry.lock().await.list();
    Ok(serde_json::json!({"status": "ok", "processes": processes}))
}

// L0 portless Group B-3: 旧 SP HTTP `/api/ruby/*` を process-proxy ask に移管。 HTTP handler と同じ
// `process_runner::ruby_*` core を呼ぶ薄い adapter (payload からフィールド抽出)。 ruby_list は
// `process_registry.list()` = `handle_process_list` と同一なので dispatch 側で再利用する。

/// ruby_eval: 短命 Ruby 実行
async fn handle_ruby_eval(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let r = crate::process::process_runner::ruby_eval(
        code,
        file,
        pane_id,
        &state.project_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({
        "status": "ok",
        "stdout": r.stdout,
        "stderr": r.stderr,
        "exit_code": r.exit_code,
        "elapsed_ms": r.elapsed_ms,
    }))
}

/// ruby_run: 長命 Ruby daemon 起動
async fn handle_ruby_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let name = payload.get("name").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let process_id = crate::process::process_runner::ruby_run(
        &state.process_registry,
        code,
        file,
        name,
        pane_id,
        &state.project_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// ruby_stop: Ruby daemon 停止
async fn handle_ruby_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::process::process_runner::ruby_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// Terminal チャネル制御メッセージハンドラー
// =============================================================================

// =============================================================================
// サーバー起動
// =============================================================================

/// SP "process" channel の method dispatch（reverse-routing と共有する単一の入口）。
///
/// SP の "process" Unison channel handler と、 World reverse-routing 経由 (SP control
/// keepalive) の **両方**がこの関数を呼ぶことで、 「MCP が SP 直結」「MCP → World → SP
/// reverse」どちらの経路でも同一の dispatch ロジック・同一の AppState 操作になる
/// (L0 SP-portless: SP listen port を World 単一 endpoint に寄せても挙動不変)。
/// S2 (doc 27 §4.1): terminal demand start ハンドラー。
///
/// World の TopicRouter demand hook が `process/terminal/data/{lane}/out` の購読者を
/// 検知し、 control reverse-route で本 method を撃つ。 SP は当該 Lane の PtySlot output
/// broadcast を購読する pump を spawn し、 per-lane terminal topic に route し始める
/// (= 購読者が居る間だけ pump を回す demand-driven production)。
async fn handle_terminal_demand_start(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand_start: lane 未指定".to_string());
    }
    if crate::process::lanes_state::LanePool::parse_address(&lane).is_none() {
        return Err(format!("terminal_demand_start: lane パース失敗: {}", lane));
    }

    // 当該 Lane の現行 PtySlot に pump を張る。 Lane 不在 / PtySlot 無 = pump 張れず
    // (demand 自体は受理 = Lane 起動後の再 demand 余地を残す)。
    if respawn_terminal_pump(state, &lane).await {
        Ok(serde_json::json!({"status": "started", "lane": lane}))
    } else {
        Ok(serde_json::json!({"status": "no_lane", "lane": lane}))
    }
}

/// 指定 Lane の **現時点の** PtySlot output に terminal pump を張り直す (idempotent)。
///
/// `attach_output` で今の PtySlot の replay snapshot + broadcast を原子的に取得して pump を
/// spawn (= 新 subscriber には直近画面が replay されてから live が続く、 replay-on-attach)。
/// 既存 pump handle があれば `abort()` して差し替える (二重 demand_start / restart 後の
/// 付け替えでも 1 本に収束)。 Lane に PtySlot が無ければ `false` (pump は張れない)。
///
/// demand hook (購読 0→1) の start 経路と、 restart_lane 後の pump 付け替え (BUG#1: restart で
/// slot を差し替えても World 側 subscriber は張りっぱなしで demand が再発火しない) が、
/// この単一経路を共有する。 `lane` は LaneAddress の Display 形。
pub(crate) async fn respawn_terminal_pump(state: &AppState, lane: &str) -> bool {
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return false;
    };
    let attached = state.lane_pool.read().await.attach_output(&addr);
    let Some((replay, rx)) = attached else {
        tracing::debug!("respawn_terminal_pump: Lane に PtySlot 無 (lane={})", lane);
        return false;
    };
    let handle = crate::process::terminal_pump::spawn_lane_terminal_pump(
        lane.to_string(),
        replay,
        rx,
        state.topic_router.clone(),
    );
    if let Some(old) = state
        .terminal_pumps
        .write()
        .await
        .insert(lane.to_string(), handle)
    {
        old.abort();
    }
    tracing::info!("terminal pump start (lane={})", lane);
    true
}

/// S2: terminal demand stop ハンドラー。 最後の購読者が消えたら pump を abort する。
async fn handle_terminal_demand_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand_stop: lane 未指定".to_string());
    }
    let removed = state.terminal_pumps.write().await.remove(&lane);
    match removed {
        Some(handle) => {
            handle.abort();
            tracing::info!("terminal pump stop (lane={})", lane);
            Ok(serde_json::json!({"status": "stopped", "lane": lane}))
        }
        None => Ok(serde_json::json!({"status": "not_running", "lane": lane})),
    }
}

/// Act II replay-on-attach: echoes demand start ハンドラー。
///
/// World の demand hook が `process/echoes/data/{lane}/event` の購読者 0→1 を検知し、 control
/// reverse-route で本 method を撃つ。 SP は当該 chat lane の **transcript を replay** して topic に
/// route する（`ReplayStart` + 過去会話の EchoesEvent 列）。
///
/// なぜ必要か: echoes topic は非 retained で、 会話履歴は vp-app の in-memory ring buffer に
/// しか無い。 app 再起動で ChatView が空になる（engine 側は `--resume` で会話を保持しているのに
/// 描く履歴が無い）。 唯一の履歴 SSOT である claude の transcript(jsonl) から起こし直す。
///
/// 冪等: 先頭の `ReplayStart` を見て GUI が会話表示をクリアするため、 reconnect / demand 再発火で
/// 二重化しない（terminal replay の clear-prefix と同型）。
///
/// **生成中に着地した場合**: claude は message を完了時にしか transcript へ flush しないので、
/// transcript だけでは生成中 message が欠ける（GUI は reset 済みなので、 復帰後の chunk が文の
/// 途中から新しいバブルを立ててしまう）。 そこで engine host の **in-flight tail** を transcript の
/// 後ろに継ぐ（`replay = transcript(commit 済み) ++ tail(未 commit)`、 `echoes::host` module doc）。
///
/// transcript を読んでいる最中に commit が挟まると tail と transcript が食い違う（欠落 or 二重化）。
/// commit 世代 `seq` を読み前後で検算し、 動いていたら読み直す。 収束しなければ tail を捨てて
/// commit 済み状態に収束させる（= 従来動作にフォールバック、 二重化より安全）。
///
/// chat mode でない lane / cc_session id 不明 / transcript 不在は「replay 無し」で graceful に返す
/// （console は live event を待つだけで壊れない）。
async fn handle_echoes_demand_start(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("echoes_demand_start: lane 未指定".to_string());
    }
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(&lane) else {
        return Err(format!("echoes_demand_start: lane パース失敗: {lane}"));
    };

    // chat lane 以外は replay しない（Act I の履歴は PtySlot の terminal replay が担う）。
    let is_chat = {
        let pool = state.lane_pool.read().await;
        pool.console_mode(&addr) == Some(crate::lane::console_mode::ConsoleMode::Chat)
    };
    if !is_chat {
        return Ok(serde_json::json!({"status": "not_chat", "lane": lane}));
    }

    let lane_label = crate::process::stand_spawner::lane_label(&addr).to_string();
    let Some(session_id) = crate::lane::cc_session::last(&addr.project, &lane_label) else {
        // 初回 (まだ会話が無い) lane。 空会話に収束させるため ReplayStart だけ送る。
        route_echoes(state, &lane, vec![crate::echoes::EchoesEvent::ReplayStart]).await;
        return Ok(serde_json::json!({"status": "no_session", "lane": lane}));
    };

    let (events, tail_len) = replay_with_in_flight(state, &addr, &session_id).await?;

    let count = events.len();
    route_echoes(state, &lane, events).await;
    tracing::info!(
        "echoes transcript replay: {count} events を配送 (lane={lane}, in-flight tail={tail_len})"
    );
    Ok(serde_json::json!({
        "status": "replayed", "lane": lane, "events": count, "in_flight": tail_len
    }))
}

/// transcript 読み + in-flight tail の結合を、 commit 世代 `seq` で検算しながら行う。
///
/// 戻り値は `(replay 列, 継いだ tail の長さ)`。 tail 長 0 は「生成中でない」か「収束せず捨てた」。
async fn replay_with_in_flight(
    state: &AppState,
    addr: &crate::process::lanes_state::LaneAddress,
    session_id: &str,
) -> Result<(Vec<crate::echoes::EchoesEvent>, usize), String> {
    /// commit が挟まったときの読み直し回数。 commit 間隔（数百 ms 〜 秒）に対し transcript 読みは
    /// 数 ms なので、 実運用では 1 回目で収束する。
    const MAX_ATTEMPTS: usize = 3;

    for _ in 0..MAX_ATTEMPTS {
        // 先に tail を取る。 「tail → transcript」の順なら、 間に commit が挟まっても
        // transcript 側が新しい = 情報の欠落は起きない（二重化は seq 検算で弾く）。
        let before = state.lane_pool.read().await.chat_in_flight(addr);

        // disk read + 翻訳は同期 I/O（数 MB / 数千行）。 tokio worker を塞がないよう隔離する。
        let sid = session_id.to_string();
        let mut events =
            tokio::task::spawn_blocking(move || crate::echoes::transcript::replay_events(&sid))
                .await
                .map_err(|e| format!("echoes_demand_start: transcript 変換 join 失敗: {e}"))?;

        let after_seq = state.lane_pool.read().await.chat_commit_seq(addr);
        let Some(in_flight) = before else {
            // engine 未起動（chat-idle / 再起動直後）= 継ぐ tail が無い。 transcript がすべて。
            return Ok((events, 0));
        };
        if after_seq != Some(in_flight.seq) {
            // 読んでいる間に message が commit された（or engine が入れ替わった）。
            // tail が古い可能性があるので読み直す。
            continue;
        }
        let tail_len = in_flight.tail.len();
        events.extend(in_flight.tail);
        return Ok((events, tail_len));
    }

    // 収束せず（生成が極端に速い / engine が入れ替わり続ける）。 tail を捨て、 commit 済み状態に
    // 収束させる。 欠けた生成中 message は次の attach cycle で復元される。
    tracing::warn!(
        "echoes replay: commit 世代が {MAX_ATTEMPTS} 回連続で動いたため in-flight tail を破棄 (lane={addr})"
    );
    let sid = session_id.to_string();
    let events =
        tokio::task::spawn_blocking(move || crate::echoes::transcript::replay_events(&sid))
            .await
            .map_err(|e| format!("echoes_demand_start: transcript 変換 join 失敗: {e}"))?;
    Ok((events, 0))
}

/// echoes demand stop ハンドラー。 replay は on-attach の一度きりなので停止対象の task は無い。
/// （live event の producer は `EchoesAgentHost` + `echoes_pump` で、 engine の生存に紐づく）
async fn handle_echoes_demand_stop(
    _state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(serde_json::json!({"status": "noop", "lane": lane}))
}

/// EchoesEvent 列を per-lane echoes topic に順に route する（echoes_pump と同じ経路）。
async fn route_echoes(state: &AppState, lane: &str, events: Vec<crate::echoes::EchoesEvent>) {
    for event in events {
        state
            .topic_router
            .route(crate::protocol::ProcessMessage::EchoesEvent {
                lane: lane.to_string(),
                event,
            })
            .await;
    }
}

/// S3 (doc 27 §4.1, 経路 B): terminal 入力。
///
/// surface (vp-app) → World canvas channel (upstream request) → SP control → 本 dispatch。
/// `data` は base64 (出力 pump の encoding と対称、 任意バイトを JSON で運ぶため)。 decode して
/// 当該 Lane の PtySlot に書き込む。
async fn handle_terminal_write(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_write: lane 未指定".to_string());
    }
    let data_b64 = payload.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("terminal_write: base64 decode 失敗: {}", e))?;
    vp_paths::term_trace("B:sp-recv", lane, &bytes);
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_write: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .write_to_lane(&addr, &bytes)
        .map_err(|e| format!("terminal_write 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// Act II (doc 33): echoes プロンプト投入。
///
/// surface (vp-app) → World canvas channel → SP control → 本 dispatch。
/// **mode=chat が前提**（法: 1 lane 高々 1 エンジン。tui のまま submit は Err で弾き、
/// 生きた TUI を暗黙に殺さない）。engine は LanePool が lazy spawn（初回のみ）し、
/// EchoesEvent は echoes_pump 経由で `process/echoes/data/{lane}/event` に流れる。
async fn handle_echoes_submit(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("echoes_submit: lane 未指定".to_string());
    }
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.is_empty() {
        return Err("echoes_submit: prompt 未指定".to_string());
    }
    ensure_and_submit_chat(state, "echoes_submit", lane, prompt).await?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 35 §4: 逆方向 can_use_tool（質問/承認）への GUI 回答を engine に書き戻す。
///
/// `{lane, request_id, answers?, behavior?, message?}` → `LanePool::respond_chat` →
/// `host.respond_permission`。PR1 は AskUserQuestion の回答（answers）のみ。
async fn handle_echoes_respond(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("echoes_respond: lane 未指定".to_string());
    }
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err("echoes_respond: request_id 未指定".to_string());
    }
    let decision = parse_permission_decision(&payload);
    let addr = crate::process::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("echoes_respond: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .respond_chat(&addr, request_id, decision)
        .await
        .map_err(|e| format!("echoes_respond 失敗: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// echoes_respond payload → PermissionDecision（doc 35 §2.3、純粋 calculation）。
///
/// - `answers` あり → Answer（AskUserQuestion 回答、PR1）
/// - `behavior == "deny"` → Deny（質問キャンセル / PR3 却下）
/// - それ以外（`behavior == "allow"` 等）→ Allow（PR3 の tool 承認）
fn parse_permission_decision(payload: &serde_json::Value) -> crate::echoes::PermissionDecision {
    use crate::echoes::PermissionDecision;
    if let Some(answers) = payload.get("answers") {
        return PermissionDecision::Answer(answers.clone());
    }
    match payload.get("behavior").and_then(|v| v.as_str()) {
        Some("deny") => {
            let message = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("ユーザーが却下しました")
                .to_string();
            PermissionDecision::Deny { message }
        }
        _ => PermissionDecision::Allow,
    }
}

/// channel E（doc 34 §3）: wire delivery / delegation reconcile からの engine 直接注入。
///
/// `{lane, text}` — Tui の `lane_nudge`（PtySlot 直書き）の Chat 対応物。nudge 文言を 1 ターン
/// として submit する。turn 実行中でも engine 側が queue するため任意時点で呼べる
/// （doc 34 Step 0 spike ①実測）。
async fn handle_echoes_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("echoes_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Err("echoes_nudge: text 未指定".to_string());
    }
    ensure_and_submit_chat(state, "echoes_nudge", lane, text).await?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// ensure（mode ガード + lazy spawn）→ submit（+ engine 死亡時 1 回の self-heal retry）の共通核。
///
/// `echoes_submit`（GUI 入力）と `echoes_nudge`（channel E）が共用する。`ctx` はエラー文言の
/// 前置き（呼び出し元 method 名 — 嘘ログ防止のため呼び元を正しく名乗る）。
async fn ensure_and_submit_chat(
    state: &AppState,
    ctx: &str,
    lane: &str,
    prompt: &str,
) -> Result<(), String> {
    let addr = crate::process::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("{ctx}: lane パース失敗: {lane}"))?;

    // ensure（mode ガード + lazy spawn は LanePool = 法の番人が行う）。
    state
        .lane_pool
        .write()
        .await
        .ensure_chat_engine(&addr, &state.topic_router)
        .map_err(|e| format!("{ctx}: {e}"))?;

    // submit（read lock — 他 lane の操作をブロックしない）。
    let submit_result = state
        .lane_pool
        .read()
        .await
        .submit_chat(&addr, prompt)
        .await;
    if let Err(e) = submit_result {
        // self-heal: engine が死んでいた場合は落として 1 回だけ張り直す。
        tracing::warn!("{ctx} 失敗 → engine 再起動して retry: {e}");
        {
            let mut pool = state.lane_pool.write().await;
            pool.drop_chat_engine(&addr);
            pool.ensure_chat_engine(&addr, &state.topic_router)
                .map_err(|e| format!("{ctx}: engine 再起動失敗: {e}"))?;
        }
        state
            .lane_pool
            .read()
            .await
            .submit_chat(&addr, prompt)
            .await
            .map_err(|e| format!("{ctx} 失敗（retry 後）: {e}"))?;
    }
    Ok(())
}

/// Act II (doc 33): Console のエンジンモード切替。
///
/// `{lane, mode: "tui"|"chat"}`。遷移の実体（旧エンジン stop → 永続 → 新エンジン spawn）は
/// `LanePool::set_console_mode`。切替後の同一会話継続は cc_session `--resume` が担う。
async fn handle_console_set_mode(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("console_set_mode: lane 未指定".to_string());
    }
    let mode_str = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    let mode = crate::lane::console_mode::ConsoleMode::parse(mode_str)
        .ok_or_else(|| format!("console_set_mode: mode 不正: {mode_str:?}（tui|chat）"))?;
    let addr = crate::process::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("console_set_mode: lane パース失敗: {lane}"))?;

    {
        let mut pool = state.lane_pool.write().await;
        pool.set_console_mode(&addr, mode)
            .map_err(|e| format!("console_set_mode 失敗: {e}"))?;
        // doc 33 §9: chat へは engine を eager spawn（切替時に resume を開始 → session_init を
        // 早く出す = 引き継ぎ progress を切替時に集約）。失敗しても mode 切替自体は成功扱いにし、
        // engine は次の submit で self-heal 再試行される（切替 UX を engine エラーで巻き戻さない）。
        if mode == crate::lane::console_mode::ConsoleMode::Chat
            && let Err(e) = pool.ensure_chat_engine(&addr, &state.topic_router)
        {
            tracing::warn!(
                "console_set_mode: eager chat engine spawn 失敗（submit で再試行）: {e}"
            );
        }
    }
    // doc 33: Tui 方向は set_console_mode 内の restart_lane が新しい PtySlot を立てる。
    // その新 slot の PTY 出力を terminal topic に route するため pump を張り直す（chat 分岐の
    // eager engine spawn と対称。restart_lane_orchestrated と同じ付け替え配線）。
    //
    // これが要るのは「vp-app が II→I を跨いで購読を維持している」lane（= 起動時 tui）だけ。
    // demand が 1 のままなので購読 0→1 の hook（handle_terminal_demand_start）が発火せず、
    // 誰も新 PtySlot に pump を張らない。逆に購読を持たない lane（= 起動時 chat）では vp-app の
    // subscribe が demand 0→1 を撃ってこの pump が張られる。terminal topic は非 retained なので、
    // どちらの経路でも購読者不在の間に流れた PTY 出力は復元されない。
    // 上の write lock は block を抜けて drop 済（respawn_terminal_pump は内部で read lock を取る）。
    if mode == crate::lane::console_mode::ConsoleMode::Tui
        && !respawn_terminal_pump(state, lane).await
    {
        tracing::warn!(
            "console_set_mode(tui): PtySlot 不在で terminal pump 張り直し不発（lane={lane}）"
        );
    }
    Ok(serde_json::json!({"status": "ok", "lane": lane, "mode": mode.as_str()}))
}

/// Act II モデル切替: chat engine の `--model` を lane 単位で切替える。
///
/// `{lane, model: string|null}`。null / 省略 = 記録を消して claude default に戻す。
/// spec「セッション進行中でも切り替えられる」の実体はここ — 稼働中 engine を drop して
/// `ensure_chat_engine` で即再 spawn すると、cc_session の `--resume` + 新 `--model` で
/// **会話コンテキストを保ったままモデルだけ替わる**（CC の `/model` の VP 版）。
/// engine 不在（tui 中 / chat-idle）は記録のみ = 次 spawn から適用。
/// ⚠️ 進行中の turn は engine drop で切れる（UI 側は streaming 中 picker を disable して抑止）。
async fn handle_console_set_model(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("console_set_model: lane 未指定".to_string());
    }
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(ref m) = model
        && !crate::lane::engine_model::is_valid_model(m)
    {
        return Err(format!("console_set_model: model 名が不正: {m:?}"));
    }
    let addr = crate::process::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("console_set_model: lane パース失敗: {lane}"))?;

    {
        let mut pool = state.lane_pool.write().await;
        let info = pool
            .get(&addr)
            .ok_or_else(|| format!("console_set_model: Lane not found: {lane}"))?;
        if info.stand != "echoes" {
            return Err(format!(
                "console_set_model は stand=echoes の lane のみ（lane={lane}, stand={}）",
                info.stand
            ));
        }
        let lane_label = crate::process::stand_spawner::lane_label(&addr).to_string();
        match &model {
            Some(m) => crate::lane::engine_model::record(&addr.project, &lane_label, m),
            None => crate::lane::engine_model::clear(&addr.project, &lane_label),
        }
        .map_err(|e| format!("console_set_model: model 永続失敗: {e}"))?;
        // 稼働中 engine の入替（drop → resume 付き eager 再 spawn）。spawn 失敗しても
        // 記録は成功済みなので mode 切替と同様に成功扱い — 次 submit で self-heal される。
        if pool.drop_chat_engine(&addr)
            && let Err(e) = pool.ensure_chat_engine(&addr, &state.topic_router)
        {
            tracing::warn!("console_set_model: engine 再 spawn 失敗（submit で再試行）: {e}");
        }
    }
    tracing::info!("console_set_model: lane={lane} model={model:?}");
    Ok(serde_json::json!({"status": "ok", "lane": lane, "model": model}))
}

/// tmux decoupling PR1: lane nudge。 論理 lane address 宛に literal text + Enter を PtySlot へ書く。
///
/// 旧制御面 (`tmux send-keys -t <session>`) の SP-proxy 置換。 World daemon (delivery/reconcile
/// loop の re-nudge) / CLI (`vp flow handoff`) / MCP (`flow_handoff`) が control channel 経由で
/// この method を ask する。 SP-local な `AppState::nudge_lane` は同じ `deliver_nudge` sink を
/// in-process で呼ぶ (text→Enter の submit 意味論は `deliver_nudge` に集約)。
async fn handle_lane_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_nudge: lane パース失敗: {}", lane));
    };
    crate::process::lanes_state::deliver_nudge(&state.lane_pool, &addr, text)
        .await
        .map_err(|e| format!("lane_nudge 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// tmux decoupling: lane console capture。 lane の Term grid（TermAttach）を text で返す。
///
/// 旧 `tmux capture-pane`（`handle_tmux_capture`）の native 代替 — conductor が performer の
/// console を読む dev-flow 用途。 CLI `vp lane capture` / 将来の MCP がこの method を ask する。
async fn handle_lane_capture(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_capture: lane 未指定".to_string());
    }
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_capture: lane パース失敗: {}", lane));
    };
    let content = state
        .lane_pool
        .read()
        .await
        .capture_lane(&addr)
        .ok_or_else(|| format!("lane_capture: lane 不在 or console 未配線: {}", lane))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "content": content}))
}

/// S3: terminal resize。 PtySlot (+ TermAttach grid) を cols×rows に同期する。
async fn handle_terminal_resize(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_resize: lane 未指定".to_string());
    }
    // bounds check: 0 / 極大値で PTY に不正 dims を渡さない (u64→u16 silent wrap も防ぐ)。
    // 旧 daemon "terminal" channel の resize 経路と同じ範囲 (1..=1000)。
    let cols = payload.get("cols").and_then(|v| v.as_u64()).unwrap_or(80);
    let rows = payload.get("rows").and_then(|v| v.as_u64()).unwrap_or(24);
    if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
        return Err(format!(
            "terminal_resize: 不正な dims (cols={cols} rows={rows})"
        ));
    }
    let (cols, rows) = (cols as u16, rows as u16);
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_resize: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .resize_lane(&addr, cols, rows)
        .map_err(|e| format!("terminal_resize 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "cols": cols, "rows": rows}))
}

/// F6 (doc 27 §3.4.5/§6): PP Canvas state save。 旧 SP HTTP `POST /api/pp/state` を
/// process-proxy ask に移管（surface→SP 直結 HTTP を撤去、 World 経由の ask に統一）。
/// logic は旧 `pp_state_save_handler` から移設（HTTP route は削除）。
async fn handle_pp_state_save(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("pp_state_save: vpdb 未初期化".to_string());
    };
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("pp_state_save: pane_id 必須")?
        .to_string();
    let content_type = payload
        .get("content_type")
        .and_then(|v| v.as_str())
        .unwrap_or("markdown")
        .to_string();
    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // lane: null/空/"conductor" は None(=lane IS NULL) に正規化（load 側と key 一致）。
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "conductor")
        .map(|s| s.to_string());
    let stack = payload.get("stack").filter(|v| !v.is_null()).cloned();
    let ui_state = payload.get("ui_state").filter(|v| !v.is_null()).cloned();
    vpdb.upsert_pp_state(
        &state.project_dir,
        lane.as_deref(),
        &pane_id,
        &content_type,
        &content,
        title.as_deref(),
        stack.as_ref(),
        ui_state.as_ref(),
    )
    .await
    .map_err(|e| format!("pp_state upsert 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "saved"}))
}

/// F6: PP Canvas state load。 旧 SP HTTP `GET /api/pp/state` を process-proxy ask に移管。
async fn handle_pp_state_load(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("pp_state_load: vpdb 未初期化".to_string());
    };
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("paisley-park")
        .to_string();
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "conductor")
        .map(|s| s.to_string());
    match vpdb
        .load_pp_state(&state.project_dir, lane.as_deref(), &pane_id)
        .await
    {
        Ok(Some(rec)) => Ok(serde_json::json!({"status": "ok", "record": rec})),
        Ok(None) => Ok(serde_json::json!({"status": "empty"})),
        Err(e) => Err(format!("pp_state load 失敗: {}", e)),
    }
}

/// F6② (doc 27 §3.4.5/§6): Lane delete。 旧 SP HTTP `DELETE /api/lanes` を process-proxy ask に
/// 移管（surface→SP 直結 HTTP を撤去、 World 経由の ask に統一）。 logic は旧 `delete_handler`
/// から移設し、 core の `delete_lane_orchestrated` を再利用（HTTP route + handler は削除）。
async fn handle_lane_delete(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_delete: address 必須")?;
    // cleanup default = true (旧 DeleteLaneQuery default_cleanup と一致、 dir も rm する)。
    let cleanup = payload
        .get("cleanup")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let addr = crate::process::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_delete: invalid lane address: {}", address))?;
    match super::routes::lanes::delete_lane_orchestrated(state, addr, cleanup).await {
        Ok(info) => Ok(serde_json::json!({
            "deleted": info.address,
            "pid": info.pid,
            "cleanup": info.cleanup_status,
        })),
        Err(e) => Err(e.to_string()),
    }
}

/// F6③ (doc 27 §3.4.5/§6): Lane restart。 旧 SP HTTP `POST /api/lanes/restart` を process-proxy
/// ask に移管。 core の `restart_lane_orchestrated` (VP-131 透過 retry loop) を呼ぶ薄い adapter。
async fn handle_lane_restart(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_restart: address 必須")?;
    // fresh default=false (旧 RestartLaneQuery の #[serde(default)] と一致)。
    let fresh = payload
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let addr = crate::process::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_restart: invalid lane address: {}", address))?;
    super::routes::lanes::restart_lane_orchestrated(state, addr, fresh).await
}

/// lanes portless (doc 27 §3.4.5): Lane create。 旧 SP HTTP `POST /api/lanes` を process-proxy ask に
/// 移管。 core の `create_performer_orchestrated` (lane clone + PtySlot spawn) を呼ぶ薄い adapter。
/// payload は `CreateLaneReq` 互換 JSON (kind/name/stand?/cwd?/branch?/base?)。 成功は LaneInfo JSON、
/// 失敗は core が返す String error (旧 HTTP の CONFLICT="already exists" 等を保持)。
async fn handle_lane_create(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: super::routes::lanes::CreateLaneReq = serde_json::from_value(payload)
        .map_err(|e| format!("lane_create: invalid payload: {}", e))?;
    let info = super::routes::lanes::create_performer_orchestrated(state, req).await?;
    serde_json::to_value(&info).map_err(|e| format!("lane_create: LaneInfo serialize 失敗: {}", e))
}

/// lanes portless (doc 27 §3.4.5): Lane list。 旧 SP HTTP `GET /api/lanes` を process-proxy ask に
/// 移管。 core の `build_lanes_snapshot` を呼び `{lanes:[...]}` で wrap (旧 HTTP `LanesResponse` 互換)。
async fn handle_lanes_list(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = super::routes::lanes::build_lanes_snapshot(state).await;
    Ok(serde_json::json!({ "lanes": lanes }))
}

/// F6④ (doc 27 §3.4.5/§6): Stand 一覧。 旧 SP HTTP `GET /api/stands` を process-proxy ask に移管。
/// tmux decoupling PR2: built-in 静的テーブル (旧 mise task scan + TTL cache は廃止)。
async fn handle_stands_list() -> Result<serde_json::Value, String> {
    let stands = super::routes::stands::list_stands();
    Ok(serde_json::json!({ "stands": stands }))
}

pub(crate) async fn dispatch_process_method(
    state: &Arc<AppState>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        // switch_lane も generic broadcast 経路に乗せる（B1: 遠隔 active Lane 制御）。
        // hub → topic `process/paisley-park/event/switch-lane`（一時コマンド=非
        // retained）→ canvas channel → vp-app が受信して active Lane を切り替える。
        "show" | "clear" | "toggle_pane" | "split_pane" | "close_pane" | "switch_lane" => {
            handle_process_message(state, payload)
        }
        "watch_file" => handle_watch_file(state, payload).await,
        "unwatch_file" => handle_unwatch_file(state, payload).await,
        // S2: demand-driven terminal pump (World demand hook → control reverse-route)
        "terminal_demand_start" => handle_terminal_demand_start(state, payload).await,
        "terminal_demand_stop" => handle_terminal_demand_stop(state, payload).await,
        // Act II replay-on-attach: chat lane の transcript を attach 時に replay
        "echoes_demand_start" => handle_echoes_demand_start(state, payload).await,
        "echoes_demand_stop" => handle_echoes_demand_stop(state, payload).await,
        // S3: terminal 入力/resize (surface → canvas channel upstream → control reverse-route)
        "terminal_write" => handle_terminal_write(state, payload).await,
        "echoes_submit" => handle_echoes_submit(state, payload).await,
        // doc 35 §4: 逆方向 can_use_tool（質問/承認）への GUI 回答を engine へ書き戻す。
        "echoes_respond" => handle_echoes_respond(state, payload).await,
        // channel E (doc 34): wire/delegation nudge の chat-engine 注入 (lane_nudge の Chat 対応物)
        "echoes_nudge" => handle_echoes_nudge(state, payload).await,
        "console_set_mode" => handle_console_set_mode(state, payload).await,
        "console_set_model" => handle_console_set_model(state, payload).await,
        // tmux decoupling PR1: 制御面 nudge の SP-proxy 入口 (旧 tmux send-keys の置換)
        "lane_nudge" => handle_lane_nudge(state, payload).await,
        // tmux decoupling PR2: lane console capture (旧 tmux capture-pane の native 代替)
        "lane_capture" => handle_lane_capture(state, payload).await,
        "terminal_resize" => handle_terminal_resize(state, payload).await,
        // F6: PP Canvas state (旧 SP HTTP /api/pp/state を process-proxy ask に移管)
        "pp_state_save" => handle_pp_state_save(state, payload).await,
        "pp_state_load" => handle_pp_state_load(state, payload).await,
        // lanes portless: Lane create/list (旧 SP HTTP POST/GET /api/lanes を process-proxy ask に移管)
        "lane_create" => handle_lane_create(state, payload).await,
        "lanes_list" => handle_lanes_list(state).await,
        // F6②: Lane delete (旧 SP HTTP DELETE /api/lanes を process-proxy ask に移管)
        "lane_delete" => handle_lane_delete(state, payload).await,
        // F6③: Lane restart (旧 SP HTTP POST /api/lanes/restart を process-proxy ask に移管)
        "lane_restart" => handle_lane_restart(state, payload).await,
        // F6④: Stand 一覧 (旧 SP HTTP GET /api/stands を process-proxy ask に移管)
        "stands_list" => handle_stands_list().await,
        // L0 finale: SP graceful shutdown を QUIC で (旧 SP HTTP POST /api/shutdown を置換、
        // World stop_process / restart_process 用)。 shutdown_token.cancel() で graceful 停止
        // (DB close 等)。 SP が即 QUIC server を畳むため応答が返らない事もあるが best-effort。
        "shutdown" => {
            tracing::info!("Shutdown requested via QUIC dispatch");
            state.shutdown_token.cancel();
            Ok(serde_json::json!({"status": "shutting_down"}))
        }
        // tmux decoupling PR2: 旧 "tmux_*" dispatch (split/list/close/capture/agent_meta/
        // send_keys/resolve_pane) は退役。 後継は lane 語彙の "lane_nudge" / "lane_capture"。
        // ProcessRunner
        "process_run" => handle_process_run(state, payload).await,
        "process_stop" => handle_process_stop(state, payload).await,
        "process_inject" => handle_process_inject(state, payload).await,
        "process_list" => handle_process_list(state).await,
        // L0 portless Group B-3: Ruby VM (旧 SP HTTP /api/ruby/* を process-proxy ask に移管)。
        // ruby_list は process_registry.list() = process_list と同一なので handle_process_list 再利用。
        "ruby_eval" => handle_ruby_eval(state, payload).await,
        "ruby_run" => handle_ruby_run(state, payload).await,
        "ruby_stop" => handle_ruby_stop(state, payload).await,
        "ruby_list" => handle_process_list(state).await,
        // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
        "wire_send" => handle_wire_send(state, payload).await,
        "wire_recv" => handle_wire_recv(state, payload).await,
        "wire_thread" => handle_wire_thread(state, payload).await,
        // flow_progress 用 read-only 未読 count (cursor 不触り)
        "wire_unread_count" => handle_wire_unread_count(state, payload).await,
        // flow_progress 5-state FSM derive 用 read-only 最新 wmsg
        "wire_latest_msg" => handle_wire_latest_msg(state, payload).await,
        // flow_progress AwaitingUser 判定用 read-only 未 ack needs_user
        "wire_needs_user_pending" => handle_wire_needs_user_pending(state, payload).await,
        "wire_ack" => handle_wire_ack(state, payload).await,
        // Agent 委譲 (doc 28 §4): delegate=B を wake / complete=A を wake /
        // respond=NeedsInput(Reborn) に A が回答して B を再 wake (Active へ loop)。
        "delegate" => super::delegation::handle_delegate(state, payload).await,
        "complete" => super::delegation::handle_complete(state, payload).await,
        "respond" => super::delegation::handle_respond(state, payload).await,
        _ => Err(format!("不明なメソッド: process.{}", method)),
    }
}

// =============================================================================
// wiremsg ハンドラー (R2-a: TheWorld 中央 store への proxy 層)
//
// store 直結のロジックは routes/wire.rs (TheWorld 側) に移設済。 SP の責務は
// 「アドレス正規化 (N1) → TheWorld へ HTTP relay」 のみ。 QUIC dispatch と
// HTTP wrapper (routes/health.rs) は本 proxy 群を呼ぶため signature 不変。
// =============================================================================

/// agent address を canonical (qualified) 形に正規化する (wiremsg N1、 refactor R1 PR-B)
///
/// bare `"agent"` を qualified (`agent@<project>`) に正規化する。
///
/// 現行 MCP (`SelfLane::from_address`) は conductor も canonical `agent@<project>` を
/// 自前で送るため、本関数は実質 **冪等な素通し + 後方互換 (旧 client / bare 送信者) 用の
/// 防御層**。bare を残す理由: 旧 bare 送信が来ても store 識別子を qualified 一本に揃え、
/// cross-process 返信 (`agent@<project>` 宛 forward) が bare query と完全一致せず届かない
/// バグ (B2、 レビュー mem_1CbuxQuNRwHBiZgBVUWVfN) を防ぐため。
/// bare 以外 (qualified / canvas@... / gold_experience@... 等) はそのまま返す。
///
/// ⚠️ 正規化先 `self_project` は「繋いだ SP の project」なので、bare のままだと誤 SP 接続で
/// identity が化ける (= 旧 conductor バグの根)。だから identity の SSOT は MCP 側 canonical
/// 送出に移した。本関数は qualified を受けたら何もしない (= SP 非依存) のが正常運用。
fn normalize_agent_addr(addr: &str, self_project: &str) -> String {
    if addr == "agent" {
        format!("agent@{}", self_project)
    } else {
        addr.to_string()
    }
}

/// wiremsg を送信する (R2-a: TheWorld 中央 store への proxy)
///
/// payload: `{ from, to: [..], body, reply_to? }`
///
/// SP の責務はアドレス正規化 (N1: bare `"agent"` → `"agent@<self_project>"`) のみ。
/// 保存・notify・local_seq 採番・body coerce は全て TheWorld 側
/// ([`crate::process::routes::wire`])。 cross-process forward は中央化で概念ごと消滅。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_send: 'from' required".to_string())?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| normalize_agent_addr(s, &state.project_name))
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = serde_json::json!({
        "from": from,
        "to": to,
    });
    if let Some(body) = payload.get("body") {
        forwarded["body"] = body.clone();
    }
    if let Some(reply_to) = payload.get("reply_to") {
        forwarded["reply_to"] = reply_to.clone();
    }
    super::world_wire::call("/api/wire/send", forwarded).await
}

/// wiremsg を受信する (R2-a: TheWorld 中央 store への proxy、 long-poll は TheWorld 側)
///
/// payload: `{ agent, timeout? }` — timeout の clamp (default 5s / max 30s) も TheWorld 側。
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?;
    let timeout = payload.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);
    super::world_wire::call(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout }),
    )
    .await
}

/// wiremsg の ancestor-chain (系譜) を取得する (R2-a: TheWorld proxy、 read-only)
///
/// payload: `{ message_id }` — agent 文脈不要のため正規化なしで relay。
pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = state; // thread は project 文脈 (正規化) 不要。 signature は他 handler と統一
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?;
    super::world_wire::call(
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

/// wiremsg の agent 関与最新 message を取得する (R2-a: TheWorld proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の 5-state FSM derive で使う。
pub(crate) async fn handle_wire_latest_msg(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_latest_msg: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/latest-msg",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の agent 発 未 ack needs_user を取得する (TheWorld proxy、 read-only)
///
/// payload: `{ agent }` → `{ status, message }`。 `flow_progress` の `AwaitingUser` 判定で使う。
pub(crate) async fn handle_wire_needs_user_pending(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_needs_user_pending: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/needs-user-pending",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の per-agent 未読 count を取得する (R2-a: TheWorld proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の集約 view / `wire_inbox` MCP tool で使う。
pub(crate) async fn handle_wire_unread_count(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_unread_count: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg を ack する (R2-a 新設、 決定 D3: cursor 非破壊の ack 台帳への proxy)
///
/// payload: `{ message_id, agent }` → `{ status, acked }`
pub(crate) async fn handle_wire_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::normalize_agent_addr;

    #[test]
    fn normalize_bare_agent_to_qualified() {
        assert_eq!(normalize_agent_addr("agent", "vp"), "agent@vp");
    }

    #[test]
    fn normalize_keeps_qualified_and_other_addrs() {
        assert_eq!(normalize_agent_addr("agent@vp", "vp"), "agent@vp");
        assert_eq!(normalize_agent_addr("agent@other", "vp"), "agent@other");
        assert_eq!(normalize_agent_addr("agent@vp/sub", "vp"), "agent@vp/sub");
        assert_eq!(normalize_agent_addr("canvas@vp", "vp"), "canvas@vp");
    }

    /// テスト用の spawn 可能な shell。 `$SHELL` があればそれを、 無ければ OS 既定
    /// (Unix: `/bin/sh`、 Windows: `cmd.exe`) を使う。 Windows には `/bin/sh` が無いので
    /// OS 分岐が必須 (pty_slot の `default_test_shell` と同方針)。
    fn default_test_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
    }

    // =========================================================================
    // S2 (doc 27 §4.1): demand-driven terminal pump の SP 側 e2e
    // =========================================================================

    /// 実 PtySlot を lane_pool に仕込み、 demand_start → pump 起動 → PTY 出力が
    /// per-lane terminal topic に届く → demand_stop → pump 除去、 を 1 本で検証する
    /// (World 側 demand hook の reverse-route 先 = SP dispatch の責務範囲)。
    #[tokio::test]
    async fn terminal_demand_start_routes_pty_output_then_stop() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::LaneAddress;
        use crate::process::state::build_test_app_state;
        use crate::protocol::ProcessMessage;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::conductor("vp");
        let lane = addr.to_string(); // "vp/conductor"

        // 実 PtySlot を attach (subscribe_output が Some を返す前提を作る)。
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), slot, rx);
        }

        // surface 相当: SP topic_router に per-lane terminal topic を購読。
        let topic = format!("process/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start → pump 起動。
        let started = dispatch_process_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(started["status"], "started");
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump が登録される"
        );

        // shell プロンプト等の PTY 出力が terminal topic に流れてくる。
        let (rtopic, msg) = tokio::time::timeout(Duration::from_secs(5), srx.recv())
            .await
            .expect("PTY 出力が terminal topic に届かない (timeout)")
            .expect("topic channel closed");
        assert_eq!(rtopic, topic);
        assert!(matches!(msg, ProcessMessage::LaneTerminalOutput { .. }));

        // demand_stop → pump abort + map 除去。
        let stopped = dispatch_process_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_stop");
        assert_eq!(stopped["status"], "stopped");
        assert!(
            !state.terminal_pumps.read().await.contains_key(&lane),
            "pump が除去される"
        );
    }

    /// PtySlot を持たない Lane への demand_start は graceful に no_lane を返し pump を張らない。
    #[tokio::test]
    async fn terminal_demand_start_without_lane_is_graceful() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": "vp/conductor" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "no_lane");
        assert!(state.terminal_pumps.read().await.is_empty());
    }

    /// S3: terminal_write の base64 入力が実 PTY に届き (echo 出力で確認)、 terminal_resize が
    /// status ok を返す。 surface→World→SP control の終端 = SP dispatch の責務範囲を検証する。
    #[tokio::test]
    async fn terminal_write_reaches_pty_and_resize_ok() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::LaneAddress;
        use crate::process::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::conductor("vp");
        let lane = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), slot, rx);
        }

        // PTY 出力を write 前に購読 (echo を取りこぼさない)。
        let mut out = state
            .lane_pool
            .read()
            .await
            .subscribe_output(&addr)
            .expect("subscribe_output");

        // シェル初期化待ち。
        tokio::time::sleep(Duration::from_millis(500)).await;

        // terminal_write: "echo VP_S3_OK" を PtySlot に届ける。 行確定の改行は OS 依存
        // (Unix shell は LF、 cmd.exe(ConPTY) は Enter=CR、 pty_slot の write test と同方針)。
        let echo_cmd: &[u8] = if cfg!(windows) {
            b"echo VP_S3_OK\r"
        } else {
            b"echo VP_S3_OK\n"
        };
        let data = base64::engine::general_purpose::STANDARD.encode(echo_cmd);
        let res = dispatch_process_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": lane, "data": data }),
        )
        .await
        .expect("terminal_write");
        assert_eq!(res["status"], "ok");

        // 出力に "VP_S3_OK" が現れる (= 入力が実 PTY に届いた)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), out.recv()).await {
                Ok(Ok(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    // ConPTY は DSR (`\x1b[6n` = カーソル位置問い合わせ) の応答を端末側から
                    // 受け取るまで描画を進めない。 本番は xterm.js が応答するが、 test では
                    // 端末役として terminal_write 経由で応答する (pty_slot の write test と同型)。
                    if text.contains("\u{1b}[6n") {
                        let dsr = base64::engine::general_purpose::STANDARD.encode(b"\x1b[1;1R");
                        let _ = dispatch_process_method(
                            &state,
                            "terminal_write",
                            serde_json::json!({ "lane": lane, "data": dsr }),
                        )
                        .await;
                    }
                    if text.contains("VP_S3_OK") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(found, "terminal_write の入力が PTY 出力に反映されない");

        // terminal_resize: status ok + cols/rows echo。
        let res = dispatch_process_method(
            &state,
            "terminal_resize",
            serde_json::json!({ "lane": lane, "cols": 120, "rows": 40 }),
        )
        .await
        .expect("terminal_resize");
        assert_eq!(res["status"], "ok");
        assert_eq!(res["cols"], 120);
        assert_eq!(res["rows"], 40);
    }

    /// replay-on-attach を **performer lane** で end-to-end 検証する。
    ///
    /// 「vp-app 再起動 → 新 xterm が後発 subscribe → 前回画面が replay で戻る」を再現:
    /// PTY 出力を先に発生させ (= replay buffer に溜める)、 その **後で** topic を新規購読し、
    /// demand_start (= respawn_terminal_pump → attach_output) を撃つ。 購読が出力より後でも
    /// replay snapshot 経由でマーカーが届けば、 performer でも画面復元が効くことの証明になる。
    /// performer は conductor と別 topic key (`vp~performer~<name>`) に載るため、 conductor
    /// テストとは別に経路を固める価値がある。
    #[tokio::test]
    async fn replay_on_attach_restores_screen_for_performer_lane() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::LaneAddress;
        use crate::process::state::build_test_app_state;
        use crate::protocol::ProcessMessage;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        // performer lane (conductor とは別 topic key になる)
        let addr = LaneAddress::performer("vp", "feat-replay");
        let lane = addr.to_string();
        assert_eq!(lane, "vp/performer/feat-replay");

        // 実 PtySlot を performer address で登録
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), slot, rx);
        }

        // マーカーを PTY に出力させる (echo)。 この出力は「過去」= replay buffer に溜まる。
        // 改行は OS 依存 (S3 test と同方針)。 ConPTY の DSR gating は reader task が起動時に
        // 自己応答する (pty_slot の Windows 分岐) ため、 ここでは端末役の応答は不要。
        let marker = "VP_PERFORMER_REPLAY_MARKER";
        let echo_cmd: Vec<u8> = if cfg!(windows) {
            format!("echo {marker}\r").into_bytes()
        } else {
            format!("echo {marker}\n").into_bytes()
        };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, &echo_cmd).expect("write to PTY");
        }

        // マーカーが replay buffer に確実に入るまで待つ (PtySlot が echo を読み終える猶予)。
        tokio::time::sleep(Duration::from_millis(800)).await;

        // ここで初めて topic を新規購読する (= 再起動後の新 xterm。 出力より後発)。
        let topic = format!("process/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start → respawn_terminal_pump → attach_output → replay 先頭配送。
        let res = dispatch_process_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(
            res["status"], "started",
            "performer lane に pump が張れるはず"
        );

        // 後発購読でも replay 経由でマーカーが届く (= 画面復元)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = String::new();
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((got_topic, ProcessMessage::LaneTerminalOutput { lane: l, data }))) => {
                    assert_eq!(got_topic, topic);
                    assert_eq!(l, lane, "message は full lane address を載せる");
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                    if seen.contains(marker) {
                        found = true;
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            found,
            "performer lane の後発 attach で replay されず画面が復元しない (seen={seen:?})"
        );
    }

    /// PtySlot を持たない Lane への terminal_write は Err (lane 不在を上位に伝える)。
    #[tokio::test]
    async fn terminal_write_unknown_lane_errs() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;
        use base64::Engine;

        let state = build_test_app_state(None).await;
        let data = base64::engine::general_purpose::STANDARD.encode(b"x");
        let res = dispatch_process_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": "vp/conductor", "data": data }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への write は Err");
    }

    /// tmux decoupling PR1-2: lane_nudge dispatch の error 経路 3 種
    /// (lane 未指定 / parse 失敗 / lane 不在 = PtySlot 無)。 happy path は実機検証済 (design §13.6)。
    #[tokio::test]
    async fn lane_nudge_dispatch_error_paths() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res =
            dispatch_process_method(&state, "lane_nudge", serde_json::json!({ "text": "x" })).await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_process_method(
            &state,
            "lane_nudge",
            serde_json::json!({ "lane": "%3", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // lane 不在 (PtySlot 無)
        let res = dispatch_process_method(
            &state,
            "lane_nudge",
            serde_json::json!({ "lane": "vp/conductor", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への nudge は Err: {res:?}");
    }

    /// channel E (doc 34): echoes_nudge dispatch の error 経路 4 種
    /// (lane 未指定 / text 未指定 / parse 失敗 / lane 不在)。happy path は実 engine 要のため
    /// echoes_host_roundtrip (ignored) と実機 dogfood で検証。
    #[tokio::test]
    async fn echoes_nudge_dispatch_error_paths() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res =
            dispatch_process_method(&state, "echoes_nudge", serde_json::json!({ "text": "x" }))
                .await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // text 未指定
        let res = dispatch_process_method(
            &state,
            "echoes_nudge",
            serde_json::json!({ "lane": "vp/conductor" }),
        )
        .await;
        assert!(res.is_err(), "text 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_process_method(
            &state,
            "echoes_nudge",
            serde_json::json!({ "lane": "%3", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // lane 不在 (ensure_chat_engine が Lane not found)
        let res = dispatch_process_method(
            &state,
            "echoes_nudge",
            serde_json::json!({ "lane": "vp/conductor", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "不在 lane への nudge は Err: {res:?}");
    }

    /// tmux decoupling PR2: lane_capture dispatch の error 経路 3 種。
    #[tokio::test]
    async fn lane_capture_dispatch_error_paths() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(&state, "lane_capture", serde_json::json!({})).await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        let res = dispatch_process_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": "some-label" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        let res = dispatch_process_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": "vp/conductor" }),
        )
        .await;
        assert!(
            res.is_err(),
            "console 未配線 lane の capture は Err: {res:?}"
        );
    }

    /// F6②: lane_delete dispatch e2e — performer lane を pool に作り、 lane_delete で除去できる。
    /// 二度目の delete は LaneNotFound で Err (= idempotent re-call の契約)。 Err message が
    /// "Lane not found" を含むことも固定する (MCP/CLI の idempotent 判定がこの文字列に依存)。
    #[tokio::test]
    async fn lane_delete_removes_performer_and_idempotent() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneKind, LaneState};
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::performer("vp", "chore");
        let address = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            let mut pool = state.lane_pool.write().await;
            // delete は lanes map (LaneInfo) を remove するので LaneInfo + PtySlot 両方を登録する。
            pool.insert(LaneInfo {
                console_mode: Default::default(),
                id: Default::default(),
                address: addr.clone(),
                kind: LaneKind::Performer,
                name: Some("chore".to_string()),
                state: LaneState::Running,
                stand: "echoes".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                performer_status: None,
                cc_session_id: None,
                flow_state: None,
            });
            pool.insert_pty_slot(addr.clone(), slot, rx);
        }

        // cleanup=false: test lane に実 workspace dir はないので Phase 2b (fs rm) をスキップ。
        let res = dispatch_process_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect("lane_delete");
        assert_eq!(res["deleted"], address);

        // pool から PtySlot が消えている (subscribe_output が None)。
        assert!(
            state
                .lane_pool
                .read()
                .await
                .subscribe_output(&addr)
                .is_none(),
            "lane_delete 後も PtySlot が pool に残っている"
        );

        // 二度目の delete は LaneNotFound で Err (idempotent re-call の契約)。
        let err = dispatch_process_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect_err("既に消えた lane の delete は Err (LaneNotFound)");
        assert!(
            err.contains("Lane not found"),
            "Err message に LaneNotFound が含まれる (MCP/CLI の idempotent 判定が依存): {err}"
        );
    }

    /// F6②: Conductor lane は lane_delete で拒否される (architecture rule: project lifetime 紐付き)。
    #[tokio::test]
    async fn lane_delete_rejects_conductor() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delete_lane_orchestrated は LanePool の有無に関係なく kind=Conductor を最初に弾く。
        let err = dispatch_process_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": "vp/conductor" }),
        )
        .await
        .expect_err("Conductor の delete は Err");
        assert!(
            err.contains("Conductor"),
            "Conductor delete は ConductorCannotBeDeleted: {err}"
        );
    }

    /// F6③: lane_restart dispatch — 存在しない lane の restart は透過 retry (3 attempts) 後 Err。
    /// dispatch 配線 + restart_lane_orchestrated 到達を確認 (respawn 成功 path は実機検証で担保)。
    #[tokio::test]
    async fn lane_restart_unknown_lane_errs() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(
            &state,
            "lane_restart",
            serde_json::json!({ "address": "vp/performer/ghost" }),
        )
        .await;
        assert!(res.is_err(), "存在しない lane の restart は Err");
    }

    /// F6④: stands_list dispatch — process-proxy ask が `{stands:[...]}` 形で返る。
    /// list_stands_cached は mise 不在 (CI) でも空 Vec に graceful degrade するので、 配線 +
    /// wire shape (stands array 常在) を CI でも固定できる (実 stand 内容は stands.rs の
    /// mise-gated test が担保)。
    #[tokio::test]
    async fn stands_list_returns_stands_array() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(&state, "stands_list", serde_json::json!({}))
            .await
            .expect("stands_list dispatch");
        assert!(
            res.get("stands").map(|s| s.is_array()).unwrap_or(false),
            "stands_list は {{stands:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lanes_list` dispatch arm が `{lanes:[...]}` 形で返る (build_lanes_snapshot 経由)。
    #[tokio::test]
    async fn lanes_list_returns_lanes_array() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(&state, "lanes_list", serde_json::json!({}))
            .await
            .expect("lanes_list dispatch");
        assert!(
            res.get("lanes").map(|s| s.is_array()).unwrap_or(false),
            "lanes_list は {{lanes:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lane_create` dispatch arm が validation error (kind != performer) を
    /// unison error frame (= Err) として返す (core の create_performer_orchestrated に到達している証)。
    #[tokio::test]
    async fn lane_create_rejects_non_performer() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let err = dispatch_process_method(
            &state,
            "lane_create",
            serde_json::json!({ "kind": "worker", "name": "x" }),
        )
        .await
        .expect_err("kind='worker' は Err");
        assert!(
            err.contains("kind must be 'performer'"),
            "error は kind 制約を含む: {err}"
        );
    }

    // =========================================================================
    // Agent 委譲 (doc 28 §4) の SP dispatch — early validation のみ。
    // 状態遷移ロジックは World 中央 store に移管したため (doc 28 §6)、その単体 test は
    // `capability::delegation_store` が担う。SP handler は必須 field 検証後に World へ proxy
    // する (world_wire::call) ので、ここでは World 不要な早期 Err 経路だけを固定する。
    // =========================================================================

    /// delegate/complete/respond の必須 field 欠落 / 不正 outcome は World 到達前に Err。
    #[tokio::test]
    async fn delegation_dispatch_validates_before_proxy() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delegate: doer 欠落 → Err (proxy 前)。
        assert!(
            dispatch_process_method(
                &state,
                "delegate",
                serde_json::json!({ "task": "x", "requester": "agent@vp" }),
            )
            .await
            .is_err(),
            "delegate doer 欠落は Err"
        );
        // complete: id 欠落 → Err。
        assert!(
            dispatch_process_method(
                &state,
                "complete",
                serde_json::json!({ "outcome": { "kind": "done", "result": "x" } }),
            )
            .await
            .is_err(),
            "complete id 欠落は Err"
        );
        // complete: outcome の kind が未知 → from_value で Err (proxy 前)。
        assert!(
            dispatch_process_method(
                &state,
                "complete",
                serde_json::json!({ "id": "dlg-x", "outcome": { "kind": "weird" } }),
            )
            .await
            .is_err(),
            "complete 不正 outcome は Err"
        );
        // respond: answer 欠落 → Err。
        assert!(
            dispatch_process_method(&state, "respond", serde_json::json!({ "id": "dlg-x" }))
                .await
                .is_err(),
            "respond answer 欠落は Err"
        );
    }

    /// C1 test 用の chat-mode conductor LaneInfo を pool に登録する（claude 不要）。
    async fn insert_test_lane(
        state: &crate::process::state::AppState,
        project: &str,
        mode: crate::lane::console_mode::ConsoleMode,
    ) -> crate::process::lanes_state::LaneAddress {
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneKind, LaneState};
        let addr = LaneAddress::conductor(project);
        state.lane_pool.write().await.insert(LaneInfo {
            console_mode: mode,
            id: Default::default(),
            address: addr.clone(),
            kind: LaneKind::Conductor,
            name: None,
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            performer_status: None,
            cc_session_id: None,
            flow_state: None,
        });
        addr
    }

    /// echoes_submit の lane / prompt 欠落は graceful Err（claude 不要）。
    #[tokio::test]
    async fn echoes_submit_missing_fields_is_graceful() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        assert!(
            dispatch_process_method(
                &state,
                "echoes_submit",
                serde_json::json!({ "prompt": "hi" })
            )
            .await
            .is_err(),
            "lane 欠落は Err"
        );
        assert!(
            dispatch_process_method(
                &state,
                "echoes_submit",
                serde_json::json!({ "lane": "vp/conductor" })
            )
            .await
            .is_err(),
            "prompt 欠落は Err"
        );
        // pool 未登録 lane への submit も graceful Err（engine spawn は起きない）。
        assert!(
            dispatch_process_method(
                &state,
                "echoes_submit",
                serde_json::json!({ "lane": "vp/conductor", "prompt": "hi" })
            )
            .await
            .is_err(),
            "未登録 lane は Err"
        );
    }

    /// doc 33 の法: mode=tui の lane への echoes_submit は Err（暗黙切替しない）。
    /// claude 不要 — mode ガードは engine spawn 前に弾く。
    #[tokio::test]
    async fn echoes_submit_rejected_in_tui_mode() {
        use super::dispatch_process_method;
        use crate::lane::console_mode::ConsoleMode;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        insert_test_lane(&state, "vptest-c1-tui", ConsoleMode::Tui).await;
        let err = dispatch_process_method(
            &state,
            "echoes_submit",
            serde_json::json!({ "lane": "vptest-c1-tui/conductor", "prompt": "hi" }),
        )
        .await
        .expect_err("tui mode は Err");
        assert!(
            err.contains("console_set_mode") || err.contains("mode"),
            "切替を促すメッセージ: {err}"
        );
    }

    /// console_set_mode の入力検証（claude 不要。engine-less lane の tui→chat 遷移も確認）。
    #[tokio::test]
    async fn console_set_mode_validates_and_transitions() {
        use super::dispatch_process_method;
        use crate::lane::console_mode::ConsoleMode;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // mode 不正 / lane 不正
        assert!(
            dispatch_process_method(
                &state,
                "console_set_mode",
                serde_json::json!({ "lane": "vptest-c1-sm/conductor", "mode": "gui" })
            )
            .await
            .is_err(),
            "mode 不正は Err"
        );
        // engine-less の tui lane → chat へ遷移（PTY 不在でも成立、registry が更新される）
        let addr = insert_test_lane(&state, "vptest-c1-sm", ConsoleMode::Tui).await;
        let res = dispatch_process_method(
            &state,
            "console_set_mode",
            serde_json::json!({ "lane": "vptest-c1-sm/conductor", "mode": "chat" }),
        )
        .await
        .expect("tui→chat ok");
        assert_eq!(res["mode"], "chat");
        assert_eq!(
            state.lane_pool.read().await.console_mode(&addr),
            Some(ConsoleMode::Chat)
        );
        // 同一 mode への再切替は no-op Ok
        dispatch_process_method(
            &state,
            "console_set_mode",
            serde_json::json!({ "lane": "vptest-c1-sm/conductor", "mode": "chat" }),
        )
        .await
        .expect("chat→chat no-op ok");
    }

    /// 実機統合: mode=chat の lane への echoes_submit が engine を lazy spawn し、EchoesEvent が
    /// `process/echoes/data/{lane}/event` topic に届く SP 終端 round-trip を検証する。
    /// `cargo test -p vantage-point --ignored echoes_submit_roundtrip`（要 claude CLI）。
    #[tokio::test]
    #[ignore = "requires claude CLI + subscription"]
    async fn echoes_submit_roundtrip() {
        use super::dispatch_process_method;
        use crate::echoes::EchoesEvent;
        use crate::lane::console_mode::ConsoleMode;
        use crate::process::state::build_test_app_state;
        use crate::protocol::ProcessMessage;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        // doc 33: submit には mode=chat の lane が pool に要る。
        // project 名はテスト固有にする — 実在 project だと cc_session::last が本物の
        // session id を返し、temp cwd との不整合で resume が失敗する。
        insert_test_lane(&state, "vptest-c1-rt", ConsoleMode::Chat).await;
        // echoes data は非 retained なので submit 前に subscribe。
        let (_id, mut srx) = state
            .topic_router
            .subscribe("process/echoes/data/vptest-c1-rt~conductor/event")
            .await;

        dispatch_process_method(
            &state,
            "echoes_submit",
            serde_json::json!({ "lane": "vptest-c1-rt/conductor", "prompt": "Reply with exactly: PONG" }),
        )
        .await
        .expect("echoes_submit ok");

        let mut got_init = false;
        let mut text = String::new();
        let mut got_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(90), srx.recv()).await {
                Ok(Some((_topic, ProcessMessage::EchoesEvent { event, .. }))) => match event {
                    EchoesEvent::SessionInit { .. } => got_init = true,
                    EchoesEvent::MessageChunk { text: t } => text.push_str(&t),
                    EchoesEvent::TurnCompleted { .. } => {
                        got_done = true;
                        break;
                    }
                    EchoesEvent::Error { message } => panic!("engine error: {message}"),
                    _ => {}
                },
                _ => break,
            }
        }

        assert!(got_init, "SessionInit が topic に届く");
        assert!(got_done, "TurnCompleted が topic に届く");
        assert!(text.to_uppercase().contains("PONG"), "本文 PONG: {text:?}");
    }
}
