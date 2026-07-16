//! プロセス発見モジュール
//!
//! TheWorld API（port 32000）を単一の真実源として稼働中 Process を発見する。
//! SP は QUIC "registry" チャネルで自己登録し、切断時に即時除去される。
//!
//! ## データフロー
//!
//! ```text
//! SP 起動 → QUIC "registry" チャネルで TheWorld に自己登録
//! 問い合わせ → TheWorld HTTP API (port 32000) → 返却
//! SP 停止/切断 → TheWorld が即時除去
//! ```

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::cli::{PORT_RANGE_END, PORT_RANGE_START, world_port};
use crate::config::Config;

/// 発見された Process の情報
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessInfo {
    /// ポート番号
    pub port: u16,
    /// プロセス ID
    pub pid: u32,
    /// プロジェクトディレクトリ（正規化済み）
    pub project_dir: String,
    /// Terminal チャネル認証トークン
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
}

/// TheWorld API のレスポンス
#[derive(Debug, serde::Deserialize)]
struct WorldProcessesResponse {
    processes: Vec<WorldProcessEntry>,
}

/// TheWorld が返す Process エントリ
#[derive(Debug, serde::Deserialize)]
struct WorldProcessEntry {
    port: u16,
    pid: u32,
    project_path: String,
}

/// Health API のレスポンス
#[derive(Debug, serde::Deserialize)]
struct HealthResponse {
    pid: u32,
    project_dir: String,
    #[serde(default)]
    terminal_token: Option<String>,
}

/// HTTP クライアントを生成（短タイムアウト）
fn build_client(timeout_ms: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 全稼働中 Process を取得
///
/// TheWorld API (port 32000) に問い合わせ。
/// SP は QUIC registry チャネルで自己登録するため、TheWorld が単一の真実源。
pub async fn list() -> Vec<ProcessInfo> {
    query_world().await.unwrap_or_default()
}

/// プロジェクトディレクトリから Process を検索
pub async fn find_by_project(project_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(project_dir));
    list()
        .await
        .into_iter()
        .find(|p| p.project_dir == canonical)
}

/// 現在のワーキングディレクトリから Process を検索
///
/// cwd と一致するか、cwd が project_dir のサブディレクトリならマッチ。
/// 複数マッチした場合は最も具体的な（パスが長い）ものを返す。
pub async fn find_for_cwd() -> Option<ProcessInfo> {
    let cwd = std::env::current_dir().ok()?;
    let cwd_str = Config::normalize_path(&cwd);

    let processes = list().await;

    processes
        .into_iter()
        .filter(|p| cwd_str == p.project_dir || cwd_str.starts_with(&format!("{}/", p.project_dir)))
        .max_by_key(|p| p.project_dir.len())
}

/// 空きポートを検索（バインドテストのみ、ファイル不使用）
pub fn find_available_port() -> Option<u16> {
    (PORT_RANGE_START..=PORT_RANGE_END).find(|&port| is_port_available(port))
}

/// ポートが利用可能かバインドして確認 (wildcard で test、 dual-stack)
///
/// 旧実装: `[::1]` (loopback specific) で test していたが、 既存 SP が `[::]` (wildcard)
/// で bound してる場合、 specific bind は OS の dual-stack 仕様で **success してしまう**
/// (= false positive、 「available」 判定 → actual SP bind で EADDRINUSE) という bug 発生
/// (bikeboy 2026-04-29 観測)。
///
/// 修正: SP server と同じ wildcard (`[::]`) で test bind 試行、
/// + TCP connect 経由で「listening 中の何か」 検出 (wildcard bind 不可なケース対応)。
fn is_port_available(port: u16) -> bool {
    use std::net::{Ipv6Addr, SocketAddrV6, TcpListener};
    // 1. wildcard bind を試行 (= SP が実際に bind するのと同じ場所)
    let v6_wild = TcpListener::bind(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0)).is_ok();
    let v4_wild = TcpListener::bind(("0.0.0.0", port)).is_ok();
    if !v6_wild || !v4_wild {
        return false;
    }
    // 2. listening している process が無いか念のため connect 経由で再確認
    std::net::TcpStream::connect_timeout(
        &format!("[::1]:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(50),
    )
    .is_err()
}

/// TheWorld API に問い合わせ
async fn query_world() -> Option<Vec<ProcessInfo>> {
    let client = build_client(1000);
    let url = format!("http://[::1]:{}/api/world/processes", world_port());

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let body = resp.json::<WorldProcessesResponse>().await.ok()?;

    Some(
        body.processes
            .into_iter()
            .map(|p| ProcessInfo {
                port: p.port,
                pid: p.pid,
                project_dir: p.project_path,
                terminal_token: None, // TheWorld は token を持たない — 必要なら health API で取得
            })
            .collect(),
    )
}

/// Terminal トークンを生成（UUID v4）
pub fn generate_terminal_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── World uplink（SP → TheWorld :32000、1 connection × N streams）───────────────
//
// F1a (doc 27 §3.4.4): registry / canvas-ingest / control の SP outbound 接続を、
// 旧構成の「各々別 QUIC connection + 別 reconnect supervisor」(= N conn × 各 1 stream、
// QUIC multiplex を使えていない負債) から **1 共有 connection に集約**する。
//
// 構成: 1 共有 `ProtocolClient` を connect → 3 channel を open (= 3 stream) → channel ごとの
// 並行性を保つため canvas/control は独立 driver task に分離 (registry は低頻度なので supervisor
// 同居)。connection drop は `subscribe_connection_events` で 1 本の supervisor が検知し、epoch
// token で driver を一括 teardown → 再接続 → 全 channel 再 open。
//
// driver を別 task に保つ理由: 1 select ループに畳むと canvas の terminal 出力 push (高頻度) の
// `send_event().await` が control の reverse-request (keystroke = terminal_write) を待たせ
// keystroke latency が悪化する。connection は 1 本だが stream ごとの独立性は維持する。

/// SP → TheWorld の uplink を 1 connection に集約して維持する。
///
/// 切断時は exp backoff で自動再接続。`shutdown` cancel まで loop。TheWorld 側の registry
/// channel handler が切断を検知 → running_processes から即時除去。
///
/// 旧 `spawn_registry_keepalive` / `spawn_canvas_keepalive` / `spawn_control_keepalive` を
/// 統合 (F1a)。依存は全て `AppState` から引く (project_dir / port / terminal_token /
/// topic_router / lane_pool / system_event_tx)。
pub(crate) fn spawn_world_uplink(
    state: std::sync::Arc<crate::process::state::AppState>,
    shutdown: CancellationToken,
) {
    let project_dir = state.project_dir.clone();
    let project_name = resolve_project_name(&project_dir);
    let port = state.port;
    let pid = std::process::id();

    // Phase 5-D: exponential backoff for reconnect resilience。
    //  TheWorld 復活時に **全 SP が同時に殺到して thundering herd** になるのを避ける。
    //  1s → 2s → 4s → 8s → 16s → 32s → 60s (cap)、 接続成功で 1s に reset。
    const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut backoff = INITIAL_BACKOFF;

    // SystemEvent 購読は epoch を跨いで保持する (再接続中に落ちた diff は、再接続時の
    // register が full lane snapshot を送るので resync される)。
    let mut event_rx = state.system_event_tx.subscribe();

    tokio::spawn(async move {
        loop {
            if shutdown.is_cancelled() {
                return;
            }

            // agent_card は接続ごとに build (lane_pool の最新 snapshot を反映)。
            let agent_card = build_agent_card(&state, &project_name, port, pid).await;

            match connect_world_uplink(&project_dir, &agent_card).await {
                Ok(uplink) => {
                    tracing::info!(
                        "World uplink: 1 conn × 3 channel 確立 (project={}, port={})",
                        project_name,
                        port
                    );
                    backoff = INITIAL_BACKOFF;

                    let WorldUplink {
                        client,
                        registry,
                        canvas_ingest,
                        control,
                    } = uplink;

                    // epoch: この接続の driver task を一括 teardown するための token。
                    // driver は自分の channel error 時に epoch.cancel() して supervisor に
                    // 再接続を促す (= supervisor は epoch.cancelled() を監視するだけで良い)。
                    let epoch = CancellationToken::new();
                    let h_canvas = tokio::spawn(run_canvas_driver(
                        canvas_ingest,
                        state.topic_router.clone(),
                        project_dir.clone(),
                        epoch.clone(),
                    ));
                    let h_control = tokio::spawn(run_control_driver(
                        control,
                        state.clone(),
                        project_dir.clone(),
                        epoch.clone(),
                    ));

                    // registry (heartbeat 15s + lane diff push、 低頻度 outbound) は supervisor に
                    // 同居。 conn drop 検知 / driver 終了監視 (epoch) / shutdown も同 loop で扱う。
                    let mut conn_events = client.subscribe_connection_events();
                    let mut interval = tokio::time::interval(Duration::from_secs(15));
                    interval.tick().await; // 最初の tick をスキップ

                    let should_reconnect = loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                if registry
                                    .request::<serde_json::Value, serde_json::Value>("heartbeat", &serde_json::json!({}))
                                    .await
                                    .is_err()
                                {
                                    tracing::warn!("World uplink: registry heartbeat 失敗 → 再接続");
                                    break true;
                                }
                            }
                            event_result = event_rx.recv() => {
                                use crate::process::lanes_state::{Diff, SystemEvent};
                                match event_result {
                                    Ok(SystemEvent::Lane(mut diff)) => {
                                        let method = match &diff {
                                            Diff::Add { .. } => "lanes/add",
                                            Diff::Remove { .. } => "lanes/remove",
                                            Diff::Update { .. } => "lanes/update",
                                        };
                                        // doc 37: diff payload は World lane_registry をそのまま
                                        // replace するため、ここでも engine_session_id を enrich
                                        // する（供給 3 点の 1 つ。LanePool 生 LaneInfo は None）。
                                        if let Diff::Add { payload } | Diff::Update { payload } =
                                            &mut diff
                                        {
                                            payload.refresh_engine_session_id();
                                        }
                                        if registry
                                            .request::<serde_json::Value, serde_json::Value>(method, &serde_json::to_value(&diff).unwrap_or_default())
                                            .await
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                "World uplink: SystemEvent push 失敗 ({}) → 再接続 (snapshot resync)",
                                                method
                                            );
                                            break true;
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!(
                                            "World uplink: SystemEvent lagged {} events、 reconnect で snapshot 再 sync",
                                            n
                                        );
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        tracing::info!("World uplink: SystemEvent channel closed、 uplink 終了");
                                        break false;
                                    }
                                }
                            }
                            conn_ev = conn_events.recv() => {
                                use unison::network::ClientConnectionEvent;
                                if let Ok(ClientConnectionEvent::Disconnected { reason }) = conn_ev {
                                    tracing::warn!("World uplink: QUIC 切断検知 ({}) → 即再接続", reason);
                                    break true;
                                }
                            }
                            _ = epoch.cancelled() => {
                                // canvas / control driver が channel error で epoch を畳んだ。
                                tracing::warn!("World uplink: driver 終了検知 → 再接続");
                                break true;
                            }
                            _ = shutdown.cancelled() => {
                                // グレースフル unregister (registry channel はまだ生きている)。
                                let _ = registry
                                    .request::<serde_json::Value, serde_json::Value>("unregister", &serde_json::json!({}))
                                    .await;
                                tracing::info!("World uplink: registry 登録解除 (shutdown)");
                                break false;
                            }
                        }
                    };

                    // driver を畳んで完全終了を待ってから client drop (connection close)。
                    // epoch.cancelled() で抜けた場合も idempotent。 driver は cleanup (canvas の
                    // topic unsubscribe 等) を済ませて return するので await で待てる
                    // (JoinHandle を select で消費していないため二度 poll の panic は起きない)。
                    epoch.cancel();
                    let _ = h_canvas.await;
                    let _ = h_control.await;
                    drop(client);

                    if !should_reconnect {
                        return;
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        "World uplink: 接続失敗 ({}), {}秒後にリトライ (exp backoff)",
                        e,
                        backoff.as_secs()
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown.cancelled() => return,
                    }
                    backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
                }
            }
        }
    });
}

/// project_dir からプロジェクト名を解決 (config 優先、 ディレクトリ名 fallback)。
fn resolve_project_name(project_dir: &str) -> String {
    let dir_name = std::path::Path::new(project_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // config からプロジェクト名を解決（ディレクトリ名がデフォルト）
    if let Ok(config) = Config::load() {
        let normalized = Config::normalize_path(std::path::Path::new(project_dir));
        config
            .projects
            .iter()
            .find(|p| Config::normalize_path(std::path::Path::new(&p.path)) == normalized)
            .map(|p| p.name.clone())
            .unwrap_or(dir_name)
    } else {
        dir_name
    }
}

/// registry register / heartbeat 用の agent_card を最新 lane snapshot で build する。
///
/// Phase 1d: `lanes` field に SP 起動時点の `LanePool::list()` を含める (initial snapshot)。
/// tmux_session は conductor lane の実 session を反映 (無ければ None、 SP は固定の自前 session を
/// 持たないため旧 `{project}-vp` 固定値は廃止)。
async fn build_agent_card(
    state: &crate::process::state::AppState,
    project_name: &str,
    port: u16,
    pid: u32,
) -> serde_json::Value {
    let mut lanes = state.lane_pool.read().await.list();
    // doc 37: register payload の lanes は World lane_registry の初期値になる（= vp-app が受ける
    // lanes の源流）。LanePool 生の LaneInfo は engine_session_id を持たないため、供給点で enrich
    // する（供給 3 点の 1 つ — `LaneInfo::refresh_engine_session_id` の ⚠️ 参照）。
    for lane in lanes.iter_mut() {
        lane.refresh_engine_session_id();
    }
    // tmux decoupling PR2: 旧 "tmux_session" field は退役 (lane identity は lanes[].address)。
    serde_json::json!({
        "project_name": project_name,
        "port": port,
        "project_dir": state.project_dir,
        "pid": pid,
        "terminal_token": state.terminal_token,
        "lanes": lanes,
    })
}

/// 1 共有 connection 上の SP outbound 3 channel を束ねる。
///
/// `_client` (= ProtocolClient) が drop されると QUIC connection も切れるため、 channel と
/// 一緒に保持する。 registry / canvas_ingest / control は同一 connection 上の別 stream。
struct WorldUplink {
    /// QUIC connection の所有権 (drop で connection close)。
    client: unison::ProtocolClient,
    /// registry channel (heartbeat / lane diff push / unregister)。
    registry: unison::UnisonChannel,
    /// canvas-ingest channel (paisley-park / terminal topic を World に push)。
    canvas_ingest: unison::UnisonChannel,
    /// control channel (World の reverse-routing request を受ける)。
    control: unison::UnisonChannel,
}

/// TheWorld に 1 connection で接続し、 registry / canvas-ingest / control の 3 channel を
/// open + handshake する (= 1 conn × 3 streams)。 どれか 1 つでも失敗したら接続全体を
/// やり直す (all-or-nothing per epoch)。
async fn connect_world_uplink(
    project_dir: &str,
    agent_card: &serde_json::Value,
) -> Result<WorldUplink, String> {
    // VP-184: Builder API (dev default を明示、 PR-3 で mesh keypair に差し替え)。
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client 作成失敗: {}", e))?;
    let client = unison::ProtocolClient::new(transport);

    let addr = format!("[::1]:{}", world_port());
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("TheWorld 接続失敗: {}", e))?;

    // registry: 自己登録 (register request)。 拒否されたら接続を捨てて retry。
    let registry = client
        .open_channel("registry")
        .await
        .map_err(|e| format!("registry チャネルオープン失敗: {}", e))?;
    let resp = registry
        .request::<serde_json::Value, serde_json::Value>("register", agent_card)
        .await
        .map_err(|e| format!("register リクエスト失敗: {}", e))?;
    if resp.get("error").is_some() {
        return Err(format!("register 拒否: {}", resp));
    }

    // canvas-ingest: paisley-park / terminal topic を push する outbound channel。
    // handshake で project_path を渡す (World 側で path_key に正規化される)。
    let canvas_ingest = client
        .open_channel("canvas-ingest")
        .await
        .map_err(|e| format!("canvas-ingest チャネルオープン失敗: {}", e))?;
    canvas_ingest
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": project_dir }),
        )
        .await
        .map_err(|e| format!("canvas-ingest subscribe handshake 失敗: {}", e))?;

    // control: World の reverse-routing request を受ける inbound-driven channel。
    // World は本接続を `control_channels[path_key]` に保持し、 process-proxy 経由の外部
    // request を逆用 forward する (SP portless 化の本丸)。
    let control = client
        .open_channel("control")
        .await
        .map_err(|e| format!("control チャネルオープン失敗: {}", e))?;
    control
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": project_dir }),
        )
        .await
        .map_err(|e| format!("control subscribe handshake 失敗: {}", e))?;

    Ok(WorldUplink {
        client,
        registry,
        canvas_ingest,
        control,
    })
}

/// canvas-ingest driver: project の TopicRouter を購読し、 paisley-park / terminal topic の
/// ProcessMessage を canvas-ingest channel に push する。
///
/// 接続ごとに subscribe し直すことで retained を再 seed する (World 再起動を越えた canvas
/// state の再構築)。 channel error 時 / epoch cancel 時に subscription を畳んで return し、
/// channel error の場合は epoch を畳んで supervisor に再接続を促す。
async fn run_canvas_driver(
    channel: unison::UnisonChannel,
    topic_router: std::sync::Arc<crate::process::topic_router::TopicRouter>,
    project_dir: String,
    epoch: CancellationToken,
) {
    let (sub_id, mut rx) = topic_router.subscribe("process/paisley-park/#").await;
    // S2 (doc 27 §4.1): terminal data も同 ingest 経路で push (lane PTY 出力 → World canvas
    // router → surface)。 pump 不在なら本 subscription には何も流れない (= 無駄 push ゼロ)。
    // LanesSnapshot 等の dead data 混入を避けるため `process/#` 全広げはしない。
    let (term_sub_id, mut rx_term) = topic_router.subscribe("process/terminal/data/#").await;
    // doc 33 C2: Echoes Act II の構造化イベントも同 ingest 経路で World に push
    // (EchoesAgentHost → echoes_pump → 本 subscription → World canvas router → vp-app)。
    let (echoes_sub_id, mut rx_echoes) = topic_router.subscribe("process/echoes/data/#").await;

    loop {
        tokio::select! {
            recvd = rx.recv() => {
                match recvd {
                    Some((_topic, msg)) => {
                        let json = serde_json::to_value(&msg).unwrap_or_default();
                        if channel.send_event("pane", &json).await.is_err() {
                            tracing::warn!("World uplink/canvas: send 失敗 → 再接続 (project={})", project_dir);
                            break;
                        }
                    }
                    None => break, // topic_router subscription 終了 (通常起きない)
                }
            }
            recvd_term = rx_term.recv() => {
                match recvd_term {
                    Some((_topic, msg)) => {
                        let json = serde_json::to_value(&msg).unwrap_or_default();
                        if channel.send_event("pane", &json).await.is_err() {
                            tracing::warn!("World uplink/canvas: terminal send 失敗 → 再接続 (project={})", project_dir);
                            break;
                        }
                    }
                    None => break,
                }
            }
            recvd_echoes = rx_echoes.recv() => {
                match recvd_echoes {
                    Some((_topic, msg)) => {
                        let json = serde_json::to_value(&msg).unwrap_or_default();
                        if channel.send_event("pane", &json).await.is_err() {
                            tracing::warn!("World uplink/canvas: echoes send 失敗 → 再接続 (project={})", project_dir);
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = epoch.cancelled() => break,
        }
    }

    // 再接続前に subscription を畳む (次 epoch で貼り直す)。
    topic_router.unsubscribe(sub_id).await;
    topic_router.unsubscribe(term_sub_id).await;
    topic_router.unsubscribe(echoes_sub_id).await;
    // 自分の channel が死んだケースでも reconnect を誘発する (epoch.cancelled() で抜けた
    // 場合は既に cancelled なので no-op)。
    epoch.cancel();
}

/// control driver: World の reverse-routing request を recv し、 SP "process" channel と同一の
/// `dispatch_process_method` で処理して応答する。
///
/// reverse request の dispatch は現状この recv ループ内で逐次実行する (= 1 つの slow op が
/// 後続を待たせる。 並行化は follow-up)。 channel error 時 / epoch cancel 時に return し、
/// channel error の場合は epoch を畳んで supervisor に再接続を促す。
async fn run_control_driver(
    channel: unison::UnisonChannel,
    state: std::sync::Arc<crate::process::state::AppState>,
    project_dir: String,
    epoch: CancellationToken,
) {
    loop {
        tokio::select! {
            recvd = channel.recv() => {
                match recvd {
                    Ok(msg) => {
                        // World からの reverse request のみ処理 (それ以外は無視)。
                        if msg.msg_type != unison::network::MessageType::Request {
                            continue;
                        }
                        let id = msg.id;
                        let method = msg.method.clone();
                        let payload = msg.payload_as_value().unwrap_or_default();
                        // SP "process" channel と同一 dispatch で処理する。
                        let result = crate::process::unison_server::dispatch_process_method(
                            &state, &method, payload,
                        )
                        .await;
                        let response = match &result {
                            Ok(v) => v.clone(),
                            Err(e) => serde_json::json!({ "error": e }),
                        };
                        if channel.send_response(id, &method, &response).await.is_err() {
                            tracing::warn!("World uplink/control: send_response 失敗 → 再接続 (project={})", project_dir);
                            break;
                        }
                    }
                    Err(_) => break, // 切断 → 再接続
                }
            }
            _ = epoch.cancelled() => break,
        }
    }
    epoch.cancel();
}

// ─── 同期ラッパー（CLI コマンドから使用）───────────────────
//
// resolve.rs / start.rs 等の同期関数から呼ぶための同期版。
// 内部で小さな tokio ランタイムを作成する。

/// 同期版: 全稼働中 Process を取得
pub fn list_blocking() -> Vec<ProcessInfo> {
    make_runtime().block_on(list())
}

/// 同期版: プロジェクトディレクトリから Process を検索
pub fn find_by_project_blocking(project_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(project_dir));
    let processes = list_blocking();
    processes.into_iter().find(|p| p.project_dir == canonical)
}

/// 同期版: 現在のワーキングディレクトリから Process を検索
pub fn find_for_cwd_blocking() -> Option<ProcessInfo> {
    make_runtime().block_on(find_for_cwd())
}

/// 短命のランタイムを作成（同期ラッパー用）
fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}
