//! HTTP server with WebSocket support
//!
//! Process サーバーのエントリーポイント。`run()` と `run_world()` でサーバーを起動する。
//! ルートハンドラーは `routes/` モジュールに分離されている。

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::routing::{get, post};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use super::capabilities::{CapabilityConfig, ProcessCapabilities};
use super::hub::Hub;
use super::pty::PtyManager;
use super::routes::{
    health, lanes, permission, project_feed, prompt, update, world, ws, ws_terminal,
};
use super::session::SessionManager;
use super::state::AppState;
use super::topic_router::TopicRouter;
use super::unison_server;
use crate::capability::{ProcessManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::protocol::DebugMode;

/// Run the Process server
pub async fn run(
    port: u16,
    auto_open_browser: bool,
    debug_mode: DebugMode,
    mut cap_config: CapabilityConfig,
) -> Result<()> {
    let project_dir = cap_config.project_dir.clone();

    // Whitesnake をポート別ディレクトリで早期初期化（Msgbox persistence で使用）
    let whitesnake = crate::capability::Whitesnake::file_backed_for_port(port);
    cap_config.whitesnake = Some(whitesnake.clone());

    // Msgbox Phase 3: cross-Process routing 用 RemoteRoutingClient を注入
    // - project_name は project_dir から解決
    // - local_port = この Process の port
    let project_name_for_remote = crate::resolve::project_name_from_path(
        &project_dir,
        &crate::config::Config::load().unwrap_or_default(),
    )
    .to_string();
    let remote_client = crate::capability::msgbox_remote::RemoteRoutingClient::new(
        crate::cli::WORLD_PORT,
        project_name_for_remote.clone(),
        port,
    );
    cap_config.remote_routing = Some(remote_client);

    // rustls 0.23+ は CryptoProvider の明示的な設定が必要
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // トレースログファイルを早期初期化
    crate::trace_log::init_log_file();

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Create session manager with state restoration
    let sessions = SessionManager::with_config(port, project_dir.clone());
    tracing::info!(
        "Session manager initialized with {} sessions",
        sessions.session_count()
    );

    // Initialize Capability system
    let capabilities = Arc::new(ProcessCapabilities::new(cap_config).await);

    // Initialize all capabilities
    if let Err(e) = capabilities.initialize().await {
        tracing::warn!("Failed to initialize capabilities: {}", e);
    }

    // Msgbox Phase 3 Step 2b: TheWorld registry に actor を一括 register
    // initialize() 後の addresses を登録対象とする
    {
        let addresses = capabilities.msgbox_router.addresses().await;
        let project_name = project_name_for_remote.clone();
        let world_port = crate::cli::WORLD_PORT;
        tokio::spawn(async move {
            // TheWorld 起動完了を少し待つ（ベストエフォート）
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let failed = crate::capability::msgbox_remote::register_actors_to_world(
                world_port,
                &project_name,
                port,
                &addresses,
            )
            .await;
            if failed.is_empty() {
                tracing::info!(
                    "Msgbox: {} 件の actor を TheWorld registry に登録 (project={}, port={})",
                    addresses.len(),
                    project_name,
                    port
                );
            } else {
                tracing::warn!(
                    "Msgbox: {} 件 register 失敗（TheWorld 未起動の可能性）: {:?}",
                    failed.len(),
                    failed
                );
            }
        });
    }

    // Shutdown 時の TheWorld unregister（cancellation token で発火）
    {
        let shutdown_for_unreg = shutdown_token.clone();
        tokio::spawn(async move {
            shutdown_for_unreg.cancelled().await;
            if let Err(e) = crate::capability::msgbox_remote::unregister_process_from_world(
                crate::cli::WORLD_PORT,
                port,
            )
            .await
            {
                tracing::warn!("Msgbox: TheWorld unregister failed: {}", e);
            } else {
                tracing::info!(
                    "Msgbox: TheWorld registry から port={} 配下を unregister",
                    port
                );
            }
        });
    }

    let hub = Hub::new();

    // Start event bridge: EventBus -> Hub（shutdown token で停止可能）
    let _event_bridge = capabilities.start_event_bridge(hub.sender(), shutdown_token.clone());
    tracing::info!("Capability event bridge started");

    // Terminal チャネル認証トークンを生成
    let terminal_token = crate::discovery::generate_terminal_token();

    // tmux / ccwire はvp sp コマンドで独立管理（server.rs では触らない）
    // TmuxActor は SP がペイン操作（tmux_split 等）に使うため、既存セッションがあれば起動
    let project_name = crate::resolve::project_name_from_path(
        &project_dir,
        &crate::config::Config::load().unwrap_or_default(),
    )
    .to_string();
    let tmux_session = crate::tmux::session_name(&project_name);

    let tmux_handle =
        if crate::tmux::is_tmux_available() && crate::tmux::session_exists(&tmux_session) {
            super::tmux_actor::spawn_for_session(&tmux_session)
        } else {
            None
        };
    let tmux_session_name = tmux_session.clone();

    // TopicRouter 初期化 + Hub → TopicRouter ブリッジ（shutdown token で停止可能）
    let topic_router = Arc::new(TopicRouter::new());
    {
        let router_clone = topic_router.clone();
        let mut hub_rx = hub.subscribe();
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("TopicRouter bridge: shutdown");
                        break;
                    }
                    result = hub_rx.recv() => {
                        match result {
                            Ok(msg) => router_clone.route(msg).await,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("TopicRouter lagged: {} messages dropped", n);
                            }
                        }
                    }
                }
            }
        });
    }

    // SurrealDB (embedded) に接続
    // single-writer の前提下で World (= この Process) がオープン中は他 Process は
    // 同じ DB を開けないため、SP 独立起動時はエラー時に DB なし継続する fallback は
    // 残しておく (dogfooding / 将来の "vp start -p N" 併用で有用)。
    let vpdb: Option<vp_db::SharedVpDb> = {
        let data_dir = vp_db::db_data_dir();
        match vp_db::VpDb::connect_embedded(&data_dir).await {
            Ok(db) => {
                if let Err(e) = db.define_schema().await {
                    tracing::warn!("SP: SurrealDB スキーマ定義失敗（DB なしで継続）: {}", e);
                    None
                } else {
                    tracing::info!("SP: SurrealDB 接続成功 (embedded)");
                    Some(std::sync::Arc::new(db))
                }
            }
            Err(e) => {
                tracing::warn!("SP: SurrealDB 未接続、DB なしで継続: {}", e);
                None
            }
        }
    };

    // MCP 用 Msgbox ハンドルを登録（VP-24）
    let mcp_msgbox = capabilities.msgbox_router.register("mcp").await;

    // Notification ブリッジ: Msgbox "notify" → DistributedNotification（VP-24）
    // Msgbox に送られた Notification メッセージを macOS DistributedNotification に変換
    // shutdown token で停止可能
    {
        let notify_handle = capabilities.msgbox_router.register("notify").await;
        let project_dir_clone = project_dir.clone();
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("Notification bridge: shutdown");
                        break;
                    }
                    msg = notify_handle.recv() => {
                        match msg {
                            Some(msg) if msg.kind == crate::capability::msgbox::MessageKind::Notification => {
                                let project = msg
                                    .payload
                                    .get("project")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| {
                                        project_dir_clone
                                            .rsplit('/')
                                            .find(|s| !s.is_empty())
                                            .unwrap_or("unknown")
                                    })
                                    .to_string();
                                let message = msg
                                    .payload
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("完了")
                                    .to_string();
                                // path: 通知元のターミナルパス（Lane 単位通知用）
                                let path = msg
                                    .payload
                                    .get("path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&project_dir_clone)
                                    .to_string();
                                crate::notify::post_cc_notification(&project, &message, &path);
                            }
                            Some(_) => {} // 非 Notification メッセージは無視
                            None => break, // チャネル閉鎖
                        }
                    }
                }
            }
        });
    }

    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(sessions)),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
        // Phase A4-2b: lane_pool init で同 var を後続参照するため clone
        project_dir: project_dir.clone(),
        pending_permissions: Arc::new(RwLock::new(HashMap::new())),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities,
        world: None,
        msgbox_registry: None, // SP モードでは TheWorld registry 不要
        update: None,
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: terminal_token.clone(),
        tmux: Arc::new(tokio::sync::Mutex::new(tmux_handle)),
        tmux_session_name,
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        mcp_msgbox: Some(mcp_msgbox),
        vpdb,
        // ポート別ディレクトリで分離（複数プロセスの namespace 衝突を防ぐ）
        // run() 冒頭で作成した Whitesnake を共有（Msgbox persistent と同一インスタンス）
        whitesnake: whitesnake.clone(),
        // Phase A4-2b: Lane scope の Stand pool — Lead Lane 1 つ pre-populate
        // memory rule: 多 scope architecture (App/Project/Lane/Pane)、HD/TH は Lane scope。
        // Worker Lane の動的 create は A4-4、Stand spawn 連動は A5 で実装。
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::with_lead(
            project_name_for_remote.clone(),
            project_dir.clone(),
        ))),
        // Phase A4-2b: Project scope の Stand pool (PP/GE/HP) — skeleton
        project_stands: Arc::new(RwLock::new(
            super::project_stands_state::ProjectStandsPool::new(),
        )),
    });

    // Phase review fix #2: LanePool::with_lead は内部で PtySlot::spawn (openpty + spawn_command)
    // で OS syscall ブロッキング → spawn_blocking で tokio worker thread を保護。
    // でも... AppState 既に構築済なので restructure したいけど不可。 代替:
    // with_lead 自体は sync だが state 構築段階で `tokio::task::block_in_place` も使えない。
    // 結果的に SP 起動時 1 回だけの呼び出しなので影響は軽微。 review 指摘は記録、 現実装維持。
    // (`create_handler` 側の spawn_blocking 化は完了済 = lanes.rs の方が頻繁に呼ばれる重要 path)

    // ペイン状態をディスクから復元（前回 Process 終了時の状態 → RetainedStore）
    state.restore_pane_contents().await;

    let app = Router::new()
        .route("/", get(health::index_handler))
        .route("/canvas", get(health::canvas_handler))
        .route("/vendor/{filename}", get(health::vendor_handler))
        .route("/wasm/{filename}", get(health::wasm_handler))
        .route("/ws", get(ws::ws_handler))
        // Canvas Project Feed 集約 WebSocket（全 Process のメッセージを Project Feed でラップして中継）
        // 注: URL `/ws/lanes` は外部互換のため維持。内部命名は `project_feed` (mem_1CaSsN7xj69aVQtLPQFJxQ 命名整理)
        .route("/ws/lanes", get(project_feed::project_feed_ws_handler))
        // Phase 2 (Architecture v4): vp-app から Lane の PtySlot に attach する WS endpoint。
        // `?lane=<address>` で既存 LanePool の PtySlot に subscribe + write 経路を貼る。
        // 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Lane = Session Process)
        .route("/ws/terminal", get(ws_terminal::ws_terminal_handler))
        // Phase A4-2b: Lane (Lead/Worker) lifecycle の REST endpoint
        // GET: list、 POST: Worker create (A6 minimum)
        .route(
            "/api/lanes",
            get(lanes::list_handler)
                .post(lanes::create_handler)
                .delete(lanes::delete_handler),
        )
        .route("/api/show", post(health::show_handler))
        .route(
            "/api/msgbox/remote_deliver",
            post(health::msgbox_remote_deliver_handler),
        )
        .route("/api/msgbox/debug", get(health::msgbox_debug_handler))
        .route("/api/msgbox/send", post(health::msgbox_send_handler))
        .route("/api/msgbox/recv", post(health::msgbox_recv_handler))
        .route("/api/diagnose", get(health::diagnose_handler))
        .route("/api/toggle-pane", post(health::toggle_pane_handler))
        .route("/api/split-pane", post(health::split_pane_handler))
        .route("/api/close-pane", post(health::close_pane_handler))
        .route("/api/watch-file", post(health::watch_file_handler))
        .route("/api/unwatch-file", post(health::unwatch_file_handler))
        // tmux ペイン操作（Native App の Cmd+D / Cmd+Shift+D から呼ばれる）
        .route("/api/tmux/split", post(health::tmux_split_handler))
        .route("/api/tmux/close", post(health::tmux_close_handler))
        .route("/api/tmux/capture", post(health::tmux_capture_handler))
        .route("/api/tmux/list", get(health::tmux_list_handler))
        .route("/api/tmux/send-keys", post(health::tmux_send_keys_handler))
        .route("/api/tmux/agent-meta", get(health::tmux_agent_meta_handler))
        .route(
            "/api/tmux/resolve-pane",
            get(health::tmux_resolve_pane_handler),
        )
        .route("/api/ruby/eval", post(health::ruby_eval_handler))
        .route("/api/ruby/run", post(health::ruby_run_handler))
        .route("/api/ruby/stop", post(health::ruby_stop_handler))
        .route("/api/ruby/list", get(health::ruby_list_handler))
        // ProcessRunner 汎用 API
        .route("/api/process/run", post(health::process_run_handler))
        .route("/api/process/eval", post(health::process_run_eval_handler))
        .route("/api/process/stop", post(health::process_stop_handler))
        .route("/api/process/inject", post(health::process_inject_handler))
        .route("/api/process/list", get(health::process_list_handler))
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        .route(
            "/api/permission",
            post(permission::permission_request_handler),
        )
        .route(
            "/api/permission/{request_id}",
            get(permission::permission_poll_handler),
        )
        // User prompt API routes (REQ-PROMPT-001)
        .route("/api/prompt", post(prompt::prompt_request_handler))
        .route(
            "/api/prompt/{request_id}",
            get(prompt::prompt_poll_handler).post(prompt::prompt_respond_handler),
        )
        .route(
            "/api/prompts/pending",
            get(prompt::prompts_list_pending_handler),
        )
        // World API routes
        .route(
            "/api/world/projects",
            get(world::world_list_projects).post(world::world_add_project),
        )
        .route(
            "/api/world/projects/reorder",
            post(world::world_reorder_projects),
        )
        .route(
            "/api/world/projects/update",
            post(world::world_update_project),
        )
        .route(
            "/api/world/projects/remove",
            post(world::world_remove_project),
        )
        .route("/api/world/processes", get(world::world_list_processes))
        .route(
            "/api/world/processes/{project_name}/start",
            post(world::world_start_process),
        )
        .route(
            "/api/world/processes/{project_name}/stop",
            post(world::world_stop_process),
        )
        .route(
            "/api/world/processes/{project_name}/restart",
            post(world::world_restart_process),
        )
        .route(
            "/api/world/processes/{project_name}/pointview",
            post(world::world_open_pointview),
        )
        .route("/api/world/refresh", post(world::world_refresh))
        .route(
            "/api/world/ccwire/sessions",
            get(world::world_ccwire_sessions),
        )
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    // Phase 5-D: dual-stack listen (IPv4 + IPv6) ─ Win の IPV6_V6ONLY=true default を明示的に false に。
    //  旧コメント: "0.0.0.0 で IPv4 wildcard 統一" は IPv6 client (`http://[::1]:port`) を弾いてた。
    //  SP register 等が `[::1]:32000` を使ってたため永続失敗していた問題を解消。
    let listener = bind_dual_stack(port).await?;
    tracing::info!("Starting vp on http://[::]:{} (dual-stack)", port);

    // Auto-open browser
    if auto_open_browser {
        let url = format!("http://localhost:{}", port);
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if let Err(e) = health::open_browser(&url) {
                tracing::warn!("Failed to open browser: {}", e);
            }
        });
    }

    // Unison QUIC サーバーを並行起動（readiness signal 付き）
    let quic_port = port + unison_server::QUIC_PORT_OFFSET;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    {
        let state_for_quic = state.clone();
        tokio::spawn(async move {
            unison_server::start_unison_server(state_for_quic, port, ready_tx).await;
        });
    }

    // QUIC サーバーのバインド完了を待つ
    let _ = ready_rx.await;
    tracing::info!("QUIC server ready on port {}", quic_port);

    // デバッグモード時のみトレースログ監視を起動
    if debug_mode != DebugMode::None {
        let hub_for_log = state.hub.clone();
        tokio::spawn(async move {
            crate::trace_log::watch_and_broadcast(hub_for_log).await;
        });
    }

    // TheWorld に QUIC Registry 登録（永続接続 + heartbeat）
    // 切断時に TheWorld が即時除去するため、HTTP 登録は不要
    let pid = std::process::id();
    crate::discovery::spawn_registry_keepalive(
        port,
        &state.project_dir,
        pid,
        &terminal_token,
        shutdown_token.clone(),
    );

    // Phase 5-D: Lane lifecycle monitor — child PtySlot (例: `claude --continue`) が
    //   spawn_with_fallback の 800ms early-exit window を抜けた後で死んだ時に、
    //   Lane state を Dead に mark する periodic task。
    //   - 5s 間隔で全 Lane の is_alive() を check
    //   - Dead 検出 → state 更新 + pty_slots remove (zombie reap)
    //   - sidebar が /api/lanes を polling するので Dead 状態が UI に伝播
    //   関連: 2026-04-28 unison-kdl で post-spawn zombie 観測 → 検知機構が無く Lead コンソール
    //         が壊れたまま user が気付かない問題の解消。
    spawn_lane_lifecycle_monitor(state.lane_pool.clone(), shutdown_token.clone());

    // メニューバーアプリに起動完了を通知
    crate::notify::post_process_changed(port, "started");

    // Clone for shutdown
    let capabilities_for_shutdown = state.capabilities.clone();
    let file_watchers_for_shutdown = state.file_watchers.clone();
    let state_for_shutdown = state.clone();

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await?;

    // QUIC Registry 切断で TheWorld が即時除去するため、明示的 unregister は不要
    // （spawn_registry_keepalive の shutdown handler が unregister を送信済み）

    // ペイン状態をディスクに保存（次回起動時に復元、RetainedStore から取得）
    state_for_shutdown.persist_pane_contents().await;

    // メニューバーアプリに停止を通知
    crate::notify::post_process_changed(port, "stopped");

    // ファイル監視を全停止
    file_watchers_for_shutdown.lock().await.stop_all();

    // tmux / ccwire は vp sp stop で管理（SP 停止時には触らない）

    // Shutdown all capabilities
    tracing::info!("Shutting down capabilities...");
    if let Err(e) = capabilities_for_shutdown.shutdown().await {
        tracing::warn!("Error during capability shutdown: {}", e);
    }

    tracing::info!("Server stopped");
    Ok(())
}

/// WorldモードでProcessサーバーを起動
/// 複数のProject Processを管理するための専用モード
/// Daemon（PTY管理 QUIC サーバー）も統合して起動する
pub async fn run_world(port: u16) -> Result<()> {
    use crate::capability::core::{Capability, CapabilityContext};
    use crate::daemon::process;

    // PID ファイルはポートバインド成功後に書き出す（下記参照）

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Initialize World Capability
    let mut world_cap = ProcessManagerCapability::new();
    let ctx = CapabilityContext::new();

    if let Err(e) = world_cap.initialize(&ctx).await {
        tracing::error!("Failed to initialize ProcessManagerCapability: {}", e);
        return Err(anyhow::anyhow!(
            "ProcessManagerCapability initialization failed: {}",
            e
        ));
    }

    // Initialize Update Capability
    let mut update_cap = UpdateCapability::new();
    if let Err(e) = update_cap.initialize(&ctx).await {
        tracing::warn!("Failed to initialize UpdateCapability: {}", e);
    }

    // SurrealDB (embedded) に接続してスキーマ定義
    // surrealkv backend で in-process DB を開く。外部 `surreal` バイナリ不要。
    let vpdb: Option<vp_db::SharedVpDb> = {
        let data_dir = vp_db::db_data_dir();
        match vp_db::VpDb::connect_embedded(&data_dir).await {
            Ok(db) => {
                if let Err(e) = db.define_schema().await {
                    tracing::warn!("SurrealDB スキーマ定義失敗: {}", e);
                }
                Some(std::sync::Arc::new(db))
            }
            Err(e) => {
                tracing::warn!("SurrealDB 接続失敗（DB なしで継続）: {}", e);
                None
            }
        }
    };

    // VpDb を ProcessManagerCapability に注入し、DB からプロジェクトを再読み込み
    // （initialize 時点では vpdb 未設定のため config.toml から読み込まれている。
    //   ここで DB マイグレーション + DB → HashMap 同期を実行する）
    if let Some(ref db) = vpdb {
        world_cap.set_vpdb(db.clone());
        if let Err(e) = world_cap.load_config().await {
            tracing::warn!("DB 付き config 再読み込み失敗: {}", e);
        }
    }

    let world_cap = Arc::new(RwLock::new(world_cap));
    let update_cap = Arc::new(RwLock::new(update_cap));
    let hub = Hub::new();

    // TopicRouter（World モードでは Hub ブリッジ不要だが、AppState の必須フィールド）
    let topic_router = Arc::new(TopicRouter::new());

    // Create minimal state for world mode
    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(SessionManager::new())),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode: DebugMode::None,
        shutdown_token: shutdown_token.clone(),
        project_dir: String::new(),
        pending_permissions: Arc::new(RwLock::new(HashMap::new())),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities: Arc::new(
            ProcessCapabilities::new(CapabilityConfig {
                project_dir: String::new(),
                #[cfg(feature = "midi")]
                midi_config: None,
                whitesnake: None,     // World モードは永続 msgbox 不要
                remote_routing: None, // World モードは cross-Process forward 不要
            })
            .await,
        ),
        world: Some(world_cap.clone()),
        msgbox_registry: Some(Arc::new(crate::capability::MsgboxRegistry::new())),
        update: Some(update_cap.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "WORLD_DISABLED".to_string(),
        tmux: Arc::new(tokio::sync::Mutex::new(None)),
        tmux_session_name: String::new(),
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        mcp_msgbox: None,   // World モードでは MCP Msgbox 不要
        vpdb: vpdb.clone(), // World モードでも DB 参照あり
        // TheWorld もポート別ディレクトリで分離
        whitesnake: crate::capability::Whitesnake::file_backed_for_port(port),
        // Phase A4-2b: World モードでは Lane / Project Stand を持たない (空 Pool で AppState を満たす)
        // 多 scope architecture: World は App scope の component、Lane/ProjectStand は Project scope
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::new())),
        project_stands: Arc::new(RwLock::new(
            super::project_stands_state::ProjectStandsPool::new(),
        )),
    });

    let app = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        // Canvas HTML（PP window が TheWorld ポートから直接ロードするため必要）
        .route("/canvas", get(health::canvas_handler))
        .route("/vendor/{filename}", get(health::vendor_handler))
        // Canvas Lane 集約 WebSocket
        .route("/ws/lanes", get(project_feed::project_feed_ws_handler))
        // Canvas API（TheWorld 経由で Canvas WS に到達 — 一元管理）
        .route("/api/canvas/capture", post(health::canvas_capture_handler))
        .route(
            "/api/canvas/switch_lane",
            post(health::canvas_switch_lane_handler),
        )
        .route(
            "/api/canvas/layout",
            get(health::canvas_layout_get_handler).post(health::canvas_layout_save_handler),
        )
        // World API routes
        .route(
            "/api/world/projects",
            get(world::world_list_projects).post(world::world_add_project),
        )
        .route(
            "/api/world/projects/reorder",
            post(world::world_reorder_projects),
        )
        .route(
            "/api/world/projects/update",
            post(world::world_update_project),
        )
        .route(
            "/api/world/projects/remove",
            post(world::world_remove_project),
        )
        .route("/api/world/processes", get(world::world_list_processes))
        .route(
            "/api/world/processes/{project_name}/start",
            post(world::world_start_process),
        )
        .route(
            "/api/world/processes/{project_name}/stop",
            post(world::world_stop_process),
        )
        .route(
            "/api/world/processes/{project_name}/restart",
            post(world::world_restart_process),
        )
        .route(
            "/api/world/processes/{project_name}/pointview",
            post(world::world_open_pointview),
        )
        .route("/api/world/refresh", post(world::world_refresh))
        .route(
            "/api/world/ccwire/sessions",
            get(world::world_ccwire_sessions),
        )
        // HTTP register/unregister: Swift メニューバーアプリの移行完了まで残す（後方互換）
        // SP は QUIC registry チャネルで自己登録するため、これらは外部ツール用
        .route(
            "/api/world/processes/register",
            post(world::world_register_process),
        )
        .route(
            "/api/world/processes/unregister",
            post(world::world_unregister_process),
        )
        // Msgbox Registry (Phase 3: cross-Process actor messaging)
        .route(
            "/api/world/msgbox/register",
            post(world::world_msgbox_register),
        )
        .route(
            "/api/world/msgbox/unregister",
            post(world::world_msgbox_unregister),
        )
        .route(
            "/api/world/msgbox/unregister-process",
            post(world::world_msgbox_unregister_process),
        )
        .route("/api/world/msgbox/lookup", get(world::world_msgbox_lookup))
        .route("/api/world/msgbox/list", get(world::world_msgbox_list))
        // Update API routes (vp CLI)
        .route("/api/update/check", get(update::update_check))
        .route("/api/update/apply", post(update::update_apply))
        .route("/api/update/rollback", post(update::update_rollback))
        .route("/api/update/restart", post(update::update_restart))
        // Update API routes (VantagePoint.app)
        .route("/api/update/mac/check", get(update::update_mac_check))
        .route("/api/update/mac/apply", post(update::update_mac_apply))
        .route(
            "/api/update/mac/rollback",
            post(update::update_mac_rollback),
        )
        // VP-93 Step 2a: vp-app からの terminal WebSocket bridge
        .route("/ws/terminal", get(ws_terminal::ws_terminal_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Phase 5-D: dual-stack listen (IPv4 + IPv6) ─ vp-app の `http://127.0.0.1:32000` ping、
    //  SP からの `http://[::1]:32000` register、 LAN IPv6 access の 3 経路を全部受け取れるように。
    let listener = bind_dual_stack(port).await?;
    tracing::info!(
        "{} 起動 http://[::]:{} (dual-stack)",
        crate::stands::WORLD.display(),
        port
    );

    // ポートバインド成功後に PID ファイルを書き出す
    // （バインド前に書くと、失敗時に既存デーモンの PID が上書きされ制御不能になる）
    process::write_pid_file()?;

    // Clone for shutdown
    let world_for_shutdown = world_cap.clone();

    // Daemon QUIC サーバー起動（PTY セッション管理 + Registry チャネル、同一ポートで UDP/QUIC）
    // ProcessManagerCapability の running_processes を DaemonState と共有
    let running_processes_ref = world_cap.read().await.running_processes_ref();
    let projects_ref = world_cap.read().await.projects_ref();
    let daemon_state = std::sync::Arc::new(
        crate::daemon::server::DaemonState::new()
            .with_running_processes(running_processes_ref, projects_ref),
    );
    let daemon_handle = tokio::spawn(crate::daemon::server::start_daemon_server(
        daemon_state,
        port,
    ));
    tracing::info!(
        "Daemon QUIC サーバー統合起動 (port: {}, registry チャネル有効)",
        port
    );

    // ヘルスモニター起動（30秒間隔で Process 監視 + ゴースト除去 + クラッシュ復旧）
    let health_monitor = tokio::spawn(ProcessManagerCapability::run_health_monitor(
        world_cap.clone(),
        shutdown_token.clone(),
    ));

    // LIVE SELECT → 通知ブリッジ（VP-21 Phase 4）
    // processes テーブルの変更を検知して DistributedNotification に変換
    // DB 切断でストリームが終了した場合は再接続ループで自律復帰する
    if let Some(db) = vpdb.clone() {
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            use futures::StreamExt;
            tracing::info!("LIVE SELECT processes ブリッジ起動");
            // 再接続ループ: ストリームが切断されたら 5秒待って再サブスクライブ
            'reconnect: loop {
                if shutdown.is_cancelled() {
                    break 'reconnect;
                }

                let stream = match db.live_processes().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("LIVE SELECT 起動失敗（5秒後に再試行）: {}", e);
                        tokio::select! {
                            _ = shutdown.cancelled() => break 'reconnect,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        }
                        continue 'reconnect;
                    }
                };

                let mut stream = std::pin::pin!(stream);
                let mut error_count: u32 = 0;
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => {
                            tracing::info!("LIVE SELECT ブリッジ: shutdown");
                            break 'reconnect;
                        }
                        item = stream.next() => {
                            match item {
                                Some(Ok(notification)) => {
                                    error_count = 0; // 成功時にリセット
                                    let action = notification.action;
                                    let data = &notification.data;
                                    let port_val = data["port"].as_u64().unwrap_or(0) as u16;
                                    let project_name = data["project_name"]
                                        .as_str()
                                        .unwrap_or("unknown");

                                    let event = match action {
                                        vp_db::Action::Create => "started",
                                        vp_db::Action::Update => "updated",
                                        vp_db::Action::Delete => "stopped",
                                        _ => "changed",
                                    };

                                    tracing::info!(
                                        "LIVE SELECT: {} '{}' (port={})",
                                        event,
                                        project_name,
                                        port_val
                                    );

                                    if port_val > 0 {
                                        crate::notify::post_process_changed(port_val, event);
                                    }
                                }
                                Some(Err(e)) => {
                                    error_count += 1;
                                    tracing::warn!("LIVE SELECT エラー ({}/5): {}", error_count, e);
                                    // 連続 5 回エラーで再接続ループに移行
                                    if error_count >= 5 {
                                        tracing::warn!("LIVE SELECT 連続エラー → 再接続...");
                                        tokio::select! {
                                            _ = shutdown.cancelled() => break 'reconnect,
                                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                                        }
                                        continue 'reconnect;
                                    }
                                }
                                None => {
                                    // ストリーム終了（DB 再起動など）— 再接続を試みる
                                    tracing::warn!(
                                        "LIVE SELECT ストリーム切断、5秒後に再接続..."
                                    );
                                    tokio::select! {
                                        _ = shutdown.cancelled() => break 'reconnect,
                                        _ = tokio::time::sleep(
                                            std::time::Duration::from_secs(5)
                                        ) => {}
                                    }
                                    continue 'reconnect;
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    // シグナルハンドラ: Unix は SIGTERM、Windows は Ctrl-C を代替イベントに
    let shutdown_for_signal = shutdown_token.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT (Ctrl-C) 受信、シャットダウン開始");
            }
            _ = crate::platform::wait_for_terminate_signal() => {
                tracing::info!("SIGTERM 受信、シャットダウン開始");
            }
        }
        shutdown_for_signal.cancel();
    });

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("World graceful shutdown initiated");
        })
        .await?;

    // クリーンアップ
    health_monitor.abort();
    daemon_handle.abort();

    // Shutdown capabilities
    tracing::info!("Shutting down World...");
    if let Err(e) = world_for_shutdown.write().await.shutdown().await {
        tracing::warn!("Error during world shutdown: {}", e);
    }
    {
        let mut update = update_cap.write().await;
        if let Err(e) = update.shutdown().await {
            tracing::warn!("Error during update shutdown: {}", e);
        }
    }

    // SurrealDB は独立デーモンなので TheWorld 終了時には止めない
    // 再起動が必要な場合は `vp db restart` を使用

    // PID ファイル削除
    process::remove_pid_file();
    tracing::info!("World stopped");
    Ok(())
}

/// Phase 5-D: dual-stack TCP listener (IPv4 + IPv6 同 port)。
///
/// - `[::]` (IPv6 wildcard) に bind ─ macOS / Linux は default で v6only=false なので IPv4 client
///   (`127.0.0.1:port`) も IPv4-mapped IPv6 経由で受け取れる
/// - これで `127.0.0.1:port` と `[::1]:port` の両方の client が同じ listener に届く
/// - **Windows 注意**: default で `IPV6_V6ONLY=true` のため IPv4 client が届かない。
///   tokio `TcpSocket` には `set_only_v6` API が無いため、 Windows サポート時は socket2 crate
///   経由で setsockopt(IPV6_V6ONLY, 0) する必要あり。 現在は macOS / Linux のみ正しく動く。
///
/// 関連: SP register が `http://[::1]:32000` で TheWorld に register していた箇所が
///   旧 `0.0.0.0:port` listen で connection refused していた問題の根治。
async fn bind_dual_stack(port: u16) -> Result<tokio::net::TcpListener> {
    let addr = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0);
    Ok(tokio::net::TcpListener::bind(addr).await?)
}

/// Phase 5-D: Lane lifecycle monitor — periodic task that detects Lane の child process が後で
/// 死んだ場合に state=Dead を mark する。
///
/// ## 動機
/// `spawn_with_fallback` の 800ms early-exit window では `claude --continue` が後で
/// (= spawn 後 1 秒以上経ってから) exit するパターンを捕まえられない。
/// 2026-04-28 dogfooding で unison-kdl が zombie 化、 sidebar には running 表示、
/// PTY write が `Input/output error (os error 5)` で失敗、 Lead コンソールが壊れた状態
/// で user が気付かないという問題があった。
///
/// ## 動作
/// - 5 秒間隔で `LanePool::detect_and_mark_dead()` を呼ぶ
/// - Dead 検出 = state を Dead に更新 + pty_slots から remove (PtySlot Drop で zombie reap)
/// - sidebar は /api/lanes polling で更新後 state を picker → 赤 dot 表示 → user の Restart SP に誘導
///
/// ## 設計判断: 検知のみ (auto-respawn なし)
/// 「自動再起動」は max retries / cooldown / 無限 loop 防止が必要で複雑化する。
/// まず「Dead 状態を即時 UI に反映」 で user の最低要件を満たし、 auto-respawn は別 PR で。
///
/// ## shutdown
/// `shutdown_token.cancelled()` で graceful 終了。 SP shutdown で task も clean に止まる。
fn spawn_lane_lifecycle_monitor(
    lane_pool: Arc<RwLock<super::lanes_state::LanePool>>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        // 初回 tick は即時発火するので 1 周回飛ばす (SP 起動直後の他 setup を妨げない配慮)
        tick.tick().await;

        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = shutdown.cancelled() => {
                    tracing::debug!("Lane lifecycle monitor: shutdown");
                    return;
                }
            }

            let mut pool = lane_pool.write().await;
            let transitioned = pool.detect_and_mark_dead();
            drop(pool);

            if transitioned > 0 {
                tracing::info!(
                    "Lane lifecycle monitor: {} lane(s) marked Dead this tick",
                    transitioned
                );
            }
        }
    });
}
