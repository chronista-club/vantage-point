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

use crate::cli::{PORT_RANGE_END, PORT_RANGE_START, WORLD_PORT};
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
    let url = format!("http://[::1]:{}/api/world/processes", WORLD_PORT);

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

/// 特定ポートの Process から terminal_token を取得
pub async fn fetch_terminal_token(port: u16) -> Option<String> {
    let client = build_client(1000);
    let url = format!("http://[::1]:{}/api/health", port);

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let health = resp.json::<HealthResponse>().await.ok()?;
    health.terminal_token
}

/// Terminal トークンを生成（UUID v4）
pub fn generate_terminal_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── QUIC Registry 登録（TheWorld 永続接続）───────────────

/// TheWorld に QUIC "registry" チャネルで接続し、自己登録 + heartbeat を維持する
///
/// 切断時は自動的に再接続を試みる。shutdown_token がキャンセルされるまでループ。
/// TheWorld 側の registry チャネルハンドラが切断を検知 → running_processes から即時除去。
///
/// Phase 1d: agent_card に `lanes` field を含める。 SP startup 時点の `LanePool::list()` を
/// JSON 化して push (initial snapshot)。 Lane lifecycle 変更 (Performer create/destroy) の diff
/// push は Phase 2 の Step E で実装、 現在は initial snapshot のみで Conductor を反映。
pub fn spawn_registry_keepalive(
    port: u16,
    project_dir: &str,
    pid: u32,
    terminal_token: &str,
    lane_pool: std::sync::Arc<tokio::sync::RwLock<crate::process::lanes_state::LanePool>>,
    system_event_tx: tokio::sync::broadcast::Sender<crate::process::lanes_state::SystemEvent>,
    shutdown: CancellationToken,
) {
    let project_name = std::path::Path::new(project_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // config からプロジェクト名を解決（ディレクトリ名がデフォルト）
    let project_name = if let Ok(config) = Config::load() {
        let normalized = Config::normalize_path(std::path::Path::new(project_dir));
        config
            .projects
            .iter()
            .find(|p| Config::normalize_path(std::path::Path::new(&p.path)) == normalized)
            .map(|p| p.name.clone())
            .unwrap_or(project_name)
    } else {
        project_name
    };

    // tmux_session は conductor lane の実 session（lane scheme `vp-{project}-conductor-{stand}`）を
    // agent_card build 時に lane_pool から反映する（spawn 内、 下記）。 SP は固定の自前 session を
    // 持たないため、 旧 `{project}-vp` 固定値は廃止（fix-tmux-session-naming）。

    // Phase 1d: agent_card は tokio::spawn 内で build (lane_pool の最新を読むため async 必要)。
    // outer の sync context で build する project_name は move、 lane_pool は Arc clone で渡す。
    let project_name_for_async = project_name.clone();
    let project_dir_owned = project_dir.to_string();
    let terminal_token_owned = terminal_token.to_string();
    let lane_pool_for_async = lane_pool.clone();

    // 前提: 旧 agent_card の build を tokio::spawn 内に移すため、 ここでは build しない。
    // (旧コードでは sync で build した JSON Value を closure に move していた)

    // Phase 5-D: exponential backoff for reconnect resilience。
    //  TheWorld 復活時に **全 SP が同時に殺到して thundering herd** になるのを避ける。
    //  1s → 2s → 4s → 8s → 16s → 32s → 60s (cap) の順、 接続成功で 1s に reset。
    //  Stand-side initiated reconnect (Mako 設計方針 2026-04-28)。
    const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut backoff = INITIAL_BACKOFF;

    tokio::spawn(async move {
        // Phase 1d: agent_card を spawn 内で build (lane_pool の最新 snapshot を反映)。
        // 旧 sync 構築から async 構築に変更、 reconnect 時は同 JSON を再使用 (Phase 1 MVP)。
        // Phase 2 の Step E で reconnect 時に lane_pool 再 read + diff push を検討。
        let lanes = lane_pool_for_async.read().await.list();
        // conductor lane の実 tmux session を agent_card に反映（無ければ None）。
        let tmux_session = lanes
            .iter()
            .find(|l| l.kind == crate::process::lanes_state::LaneKind::Conductor)
            .and_then(|l| l.tmux.first())
            .map(|t| t.session.clone());
        let agent_card = serde_json::json!({
            "project_name": project_name_for_async,
            "port": port,
            "project_dir": project_dir_owned,
            "pid": pid,
            "terminal_token": terminal_token_owned,
            "tmux_session": tmux_session,
            "lanes": lanes,
        });

        // Phase 2 (Step E): SystemEvent broadcast subscriber (central system bus)。
        // SP の caller (lane_spawn_actor / routes/* 等) が `system_event_tx.send(SystemEvent::*)`
        // で publish、 本 keepalive task が QUIC registry channel で TheWorld に push する経路。
        // reconnect で QUIC 入替時も event_rx は同じ Sender に接続されたまま (lag は警告のみ)。
        let mut event_rx = system_event_tx.subscribe();

        loop {
            // VP-187 round 1 review: shutdown と QUIC 切断 event が同時に発火した場合、
            // 内側 select! が conn_ev arm を選ぶと外側 loop を 1 周回して余分な
            // connect_and_register を試みる。 外側 loop 先頭で shutdown を確認して
            // 余分な再接続試行 (= log ノイズ) を防ぐ。
            if shutdown.is_cancelled() {
                return;
            }
            // TheWorld に QUIC 接続
            match connect_and_register(&agent_card).await {
                Ok(conn) => {
                    tracing::info!(
                        "Registry: QUIC 登録成功 (project={}, port={})",
                        project_name_for_async,
                        port
                    );
                    // 成功で backoff reset (次の disconnect 時に 1s からやり直し)
                    backoff = INITIAL_BACKOFF;

                    // Heartbeat ループ（15秒間隔）
                    // conn（ProtocolClient + UnisonChannel）はこのスコープで保持
                    let mut interval = tokio::time::interval(Duration::from_secs(15));
                    interval.tick().await; // 最初の tick をスキップ

                    // VP-187: connection event hook。 QUIC connection drop を即座に検知し、
                    // 15 秒周期の heartbeat 失敗を待たずに再接続へ抜ける。 heartbeat は
                    // keepalive (= SP → TheWorld registry の生存通知) として維持。
                    let mut conn_events = conn._client.subscribe_connection_events();

                    loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                if conn.channel
                                    .request::<serde_json::Value, serde_json::Value>("heartbeat", &serde_json::json!({}))
                                    .await
                                    .is_err()
                                {
                                    tracing::warn!(
                                        "Registry: heartbeat 失敗 → 再接続"
                                    );
                                    break; // 外側ループで再接続
                                }
                            }
                            event_result = event_rx.recv() => {
                                // Phase 2 (Step E): SystemEvent を QUIC channel で TheWorld に push
                                use crate::process::lanes_state::{Diff, SystemEvent};
                                match event_result {
                                    Ok(SystemEvent::Lane(diff)) => {
                                        let method = match &diff {
                                            Diff::Add { .. } => "lanes/add",
                                            Diff::Remove { .. } => "lanes/remove",
                                            Diff::Update { .. } => "lanes/update",
                                        };
                                        if conn
                                            .channel
                                            .request::<serde_json::Value, serde_json::Value>(method, &serde_json::to_value(&diff).unwrap_or_default())
                                            .await
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                "Registry: SystemEvent push 失敗 ({}) → 再接続 (snapshot で resync)",
                                                method
                                            );
                                            break;
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!(
                                            "Registry: SystemEvent lagged {} events、 reconnect で snapshot 再 sync",
                                            n
                                        );
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        tracing::info!("Registry: SystemEvent channel closed、 keepalive 終了");
                                        return;
                                    }
                                }
                            }
                            conn_ev = conn_events.recv() => {
                                // VP-187: QUIC connection lifecycle event。 Disconnected を
                                // 受けたら即座に内側 loop を抜けて外側 loop で再接続する。
                                // Connected (= 接続済) / Lagged / Closed は無視 — Closed は
                                // client drop 時のみで、 その場合 heartbeat も失敗するため
                                // 外側 loop の再接続に自然合流する。
                                use unison::network::ClientConnectionEvent;
                                if let Ok(ClientConnectionEvent::Disconnected { reason }) = conn_ev {
                                    tracing::warn!(
                                        "Registry: QUIC 切断検知 ({}) → 即再接続",
                                        reason
                                    );
                                    break;
                                }
                            }
                            _ = shutdown.cancelled() => {
                                // グレースフル unregister
                                let _ = conn.channel
                                    .request::<serde_json::Value, serde_json::Value>("unregister", &serde_json::json!({}))
                                    .await;
                                tracing::info!("Registry: QUIC 登録解除 (shutdown)");
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        "Registry: TheWorld 接続失敗 ({}), {}秒後にリトライ (exp backoff)",
                        e,
                        backoff.as_secs()
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown.cancelled() => return,
                    }
                    // Exponential: 次回は倍、 ただし MAX_BACKOFF cap
                    backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
                }
            }
        }
    });
}

/// QUIC 接続の所有権を保持する構造体
///
/// `ProtocolClient` が drop されると QUIC 接続も切れるため、
/// チャネルと一緒に保持する必要がある。
struct RegistryConnection {
    /// QUIC 接続の所有権（drop されないように保持）
    _client: unison::ProtocolClient,
    /// registry チャネル（heartbeat / unregister に使用）
    channel: unison::UnisonChannel,
}

/// TheWorld の "registry" チャネルに接続し、register リクエストを送信する
async fn connect_and_register(
    agent_card: &serde_json::Value,
) -> Result<RegistryConnection, String> {
    // VP-184: Builder API 移行 (dev default を明示、 PR-3 で mesh keypair に差し替え)。
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client 作成失敗: {}", e))?;
    let client = unison::ProtocolClient::new(transport);

    let addr = format!("[::1]:{}", WORLD_PORT);
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("TheWorld 接続失敗: {}", e))?;

    let channel = client
        .open_channel("registry")
        .await
        .map_err(|e| format!("registry チャネルオープン失敗: {}", e))?;

    // register リクエスト送信
    let resp = channel
        .request::<serde_json::Value, serde_json::Value>("register", agent_card)
        .await
        .map_err(|e| format!("register リクエスト失敗: {}", e))?;

    if resp.get("error").is_some() {
        return Err(format!("register 拒否: {}", resp));
    }

    Ok(RegistryConnection {
        _client: client,
        channel,
    })
}

/// L0 SP-portless (canvas slice): SP → World の canvas push keepalive。
///
/// SP 起動時に spawn し、 World の "canvas-ingest" channel に paisley-park topic の ProcessMessage
/// を push する。 World は受けた message を project の TopicRouter に route し、 vp-app 向け "canvas"
/// channel に再配信する (= SP "canvas" channel 直結の World 集約版)。 registry keepalive と同型の
/// backoff 再接続を持ち、 接続のたび `topic_router.subscribe` し直すことで retained を再 seed する
/// (World 再起動を越えた canvas state の再構築)。
pub fn spawn_canvas_keepalive(
    project_dir: &str,
    topic_router: std::sync::Arc<crate::process::topic_router::TopicRouter>,
    shutdown: CancellationToken,
) {
    let project_dir = project_dir.to_string();

    const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut backoff = INITIAL_BACKOFF;

    tokio::spawn(async move {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            match connect_canvas_ingest(&project_dir).await {
                Ok(conn) => {
                    tracing::info!("Canvas push: World 接続成立 (project={})", project_dir);
                    backoff = INITIAL_BACKOFF;

                    // 接続ごとに topic_router を購読 (retained 初期配信 + live delta)。
                    // 再接続時は前 subscription を unsubscribe してから貼り直す。
                    let (sub_id, mut rx) = topic_router.subscribe("process/paisley-park/#").await;
                    // S2 (doc 27 §4.1): terminal data も同 ingest 経路で push する
                    // (lane PTY 出力 → World canvas router → surface)。 pump 自体は World の
                    // demand hook が start/stop するので、 demand 不在なら本 subscription には
                    // 何も流れない (= 無駄 push ゼロ)。 LanesSnapshot 等の dead data 混入を避ける
                    // ため `process/#` 全広げはせず terminal/data に限定する。
                    let (term_sub_id, mut rx_term) =
                        topic_router.subscribe("process/terminal/data/#").await;
                    let mut conn_events = conn._client.subscribe_connection_events();

                    loop {
                        tokio::select! {
                            recvd = rx.recv() => {
                                match recvd {
                                    Some((_topic, msg)) => {
                                        let json = serde_json::to_value(&msg).unwrap_or_default();
                                        if conn.channel
                                            .send_event("pane", &json)
                                            .await
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                "Canvas push: send 失敗 → 再接続 (project={})",
                                                project_dir
                                            );
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
                                        if conn.channel
                                            .send_event("pane", &json)
                                            .await
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                "Canvas push: terminal send 失敗 → 再接続 (project={})",
                                                project_dir
                                            );
                                            break;
                                        }
                                    }
                                    None => break,
                                }
                            }
                            conn_ev = conn_events.recv() => {
                                use unison::network::ClientConnectionEvent;
                                if let Ok(ClientConnectionEvent::Disconnected { reason }) = conn_ev {
                                    tracing::warn!(
                                        "Canvas push: QUIC 切断検知 ({}) → 即再接続",
                                        reason
                                    );
                                    break;
                                }
                            }
                            _ = shutdown.cancelled() => {
                                topic_router.unsubscribe(sub_id).await;
                                topic_router.unsubscribe(term_sub_id).await;
                                return;
                            }
                        }
                    }

                    // 再接続前に subscription を畳む (次接続で貼り直す)。
                    topic_router.unsubscribe(sub_id).await;
                    topic_router.unsubscribe(term_sub_id).await;
                }
                Err(e) => {
                    tracing::debug!(
                        "Canvas push: World 接続失敗 ({}), {}秒後にリトライ (exp backoff)",
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

/// World の "canvas-ingest" channel に接続し、 subscribe handshake を済ませる。
async fn connect_canvas_ingest(project_dir: &str) -> Result<RegistryConnection, String> {
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client 作成失敗: {}", e))?;
    let client = unison::ProtocolClient::new(transport);

    let addr = format!("[::1]:{}", WORLD_PORT);
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("TheWorld 接続失敗: {}", e))?;

    let channel = client
        .open_channel("canvas-ingest")
        .await
        .map_err(|e| format!("canvas-ingest チャネルオープン失敗: {}", e))?;

    // handshake: project_path を渡す (World 側で path_key に正規化される)。
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": project_dir }),
        )
        .await
        .map_err(|e| format!("canvas-ingest subscribe handshake 失敗: {}", e))?;

    Ok(RegistryConnection {
        _client: client,
        channel,
    })
}

/// L0 SP-portless (control slice): SP → World の reverse-routing keepalive。
///
/// SP 起動時に spawn し、 World の "control" channel を開いて handshake する。 World は本接続を
/// `control_channels[path_key]` に保持し、 "process-proxy" channel 経由で来た外部 client (MCP/CLI)
/// の request を**この接続を逆用して SP に forward** する。 SP 側は本 keepalive の recv ループで
/// その reverse request を受け、 `dispatch_process_method` (= SP "process" channel と同一 dispatch)
/// で処理して応答する。 これにより MCP/CLI は SP listen port ではなく World :32000 に繋いで
/// process 操作 (show/clear/tmux/process/wire) を実行できる (SP portless 化の本丸)。
///
/// registry / canvas keepalive と同型の backoff 再接続を持つ。 reverse request の dispatch は
/// 現状この recv ループ内で逐次実行する (= 1 つの slow op が後続を待たせる。 並行化は follow-up)。
pub(crate) fn spawn_control_keepalive(
    project_dir: &str,
    state: std::sync::Arc<crate::process::state::AppState>,
    shutdown: CancellationToken,
) {
    let project_dir = project_dir.to_string();

    const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut backoff = INITIAL_BACKOFF;

    tokio::spawn(async move {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            match connect_control(&project_dir).await {
                Ok(conn) => {
                    tracing::info!("Control: World 接続成立 (project={})", project_dir);
                    backoff = INITIAL_BACKOFF;
                    let mut conn_events = conn._client.subscribe_connection_events();

                    loop {
                        tokio::select! {
                            recvd = conn.channel.recv() => {
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
                                        if conn.channel
                                            .send_response(id, &method, &response)
                                            .await
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                "Control: send_response 失敗 → 再接続 (project={})",
                                                project_dir
                                            );
                                            break;
                                        }
                                    }
                                    Err(_) => break, // 切断 → 再接続
                                }
                            }
                            conn_ev = conn_events.recv() => {
                                use unison::network::ClientConnectionEvent;
                                if let Ok(ClientConnectionEvent::Disconnected { reason }) = conn_ev {
                                    tracing::warn!(
                                        "Control: QUIC 切断検知 ({}) → 即再接続",
                                        reason
                                    );
                                    break;
                                }
                            }
                            _ = shutdown.cancelled() => return,
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        "Control: World 接続失敗 ({}), {}秒後にリトライ (exp backoff)",
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

/// World の "control" channel に接続し、 subscribe handshake を済ませる。
async fn connect_control(project_dir: &str) -> Result<RegistryConnection, String> {
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client 作成失敗: {}", e))?;
    let client = unison::ProtocolClient::new(transport);

    let addr = format!("[::1]:{}", WORLD_PORT);
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("TheWorld 接続失敗: {}", e))?;

    let channel = client
        .open_channel("control")
        .await
        .map_err(|e| format!("control チャネルオープン失敗: {}", e))?;

    // handshake: project_path を渡す (World 側で path_key に正規化されて control_channels に登録)。
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": project_dir }),
        )
        .await
        .map_err(|e| format!("control subscribe handshake 失敗: {}", e))?;

    Ok(RegistryConnection {
        _client: client,
        channel,
    })
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

/// 同期版: terminal_token を取得
pub fn fetch_terminal_token_blocking(port: u16) -> Option<String> {
    make_runtime().block_on(fetch_terminal_token(port))
}

/// 短命のランタイムを作成（同期ラッパー用）
fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}
