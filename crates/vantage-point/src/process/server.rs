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
use super::routes::{health, update, world};
use super::session::SessionManager;
use super::state::AppState;
use super::topic_router::TopicRouter;
use crate::capability::{ProcessManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::protocol::DebugMode;

/// Run the Process server
pub async fn run(port: u16, debug_mode: DebugMode, cap_config: CapabilityConfig) -> Result<()> {
    let project_dir = cap_config.project_dir.clone();
    let config_for_init = crate::config::Config::load().unwrap_or_default();

    // Whitesnake 退役: 永続は SurrealDB 一本化 (PP pane state は pane_contents)。

    // project_name は project_dir から解決（AppState / lane pool 等で使用）
    let project_name_for_remote =
        crate::resolve::project_name_from_path(&project_dir, &config_for_init).to_string();

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

    // wiremsg R5-4: 旧 msgbox の registry サブシステム (TheWorld registry への actor
    // register / unregister) は撤去済。 wire の cross-process delivery は TheWorld の
    // project registry (project → SP port) を使う別経路で、 msgbox registry には依存しない。

    let hub = Hub::new();

    // Start event bridge: EventBus -> Hub（shutdown token で停止可能）
    let _event_bridge = capabilities.start_event_bridge(hub.sender(), shutdown_token.clone());
    tracing::info!("Capability event bridge started");

    // Terminal チャネル認証トークンを生成
    let terminal_token = crate::discovery::generate_terminal_token();

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
    // VP-182: SP は project slug 別の独立 DB ディレクトリ (`db/sp_{slug}/`) を使う。
    // 旧実装は World と同一 `db/` を共有していたため surrealkv の OS 排他ロックで
    // 衝突し、 SP 側が `vpdb = None` に陥って msgbox_store が初期化されない regression
    // が発生していた (VP-179 で msg routing が WhitesnakeStore 単一経路化した結果顕在化)。
    // ディレクトリ分離で LOCK 衝突を構造的に解消。 接続失敗時の DB なし fallback は
    // 保険として残す。
    let vpdb: Option<crate::db::SharedVpDb> = {
        let slug = crate::resolve::project_slug(&project_dir, &config_for_init);
        let data_dir = crate::db::db_data_dir_for_project(&slug);
        match crate::db::VpDb::connect_embedded(&data_dir).await {
            Ok(db) => {
                if let Err(e) = db.define_schema().await {
                    tracing::warn!("SP: SurrealDB スキーマ定義失敗（DB なしで継続）: {}", e);
                    None
                } else {
                    tracing::info!("SP: SurrealDB 接続成功 (embedded: {})", data_dir.display());
                    Some(std::sync::Arc::new(db))
                }
            }
            Err(e) => {
                // respawn-leak 根治 (c): per-project db (`db/sp_{slug}/`) の LOCK を生存
                // holder が保持し続けている = 同 project の SP が既に稼働中。 旧挙動の
                // 「DB なしで継続」だと重複 SP が silent に並走して事故を増幅していた
                // (実測: daemon 再起動 race で 4 project × 2 世代 = SP 8 本)。 重複 spawn
                // と判断して起動を中止する。 先行 SP は QUIC heal 再接続で registry に
                // 自力復帰するので、 daemon 側の後始末は不要。
                if e.downcast_ref::<crate::db::DbLockHeldByLiveHolder>()
                    .is_some()
                {
                    tracing::error!(
                        "SP: project db の LOCK を既存 SP が保持 → 重複 spawn と判断して起動中止: {}",
                        e
                    );
                    return Err(anyhow::anyhow!(
                        "重複 spawn 検出: project db の LOCK を既存 SP が保持しています \
                         (この project の SP は既に稼働中)"
                    ));
                }
                tracing::warn!("SP: SurrealDB 未接続、DB なしで継続: {}", e);
                None
            }
        }
    };

    // VP-159 PR-4b: Stand / Service actor の supervisor 受け皿。 SP-local Service (= lane-spawn)
    // を `spawn_service` 経由で起動・register、 JoinHandle を保持。 World scope の
    // MidiCapability metadata register は dynamic routing vision 確定後 (cf. design-spark
    // mem_1CavFi5D1aMSpEkas89SvQ)、 PR-5 supervisor 統一で JoinHandle 経由 abort を activate。
    let actor_registry = crate::capability::ActorRegistry::new();

    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(sessions)),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
        // Phase A4-2b: lane_pool init で同 var を後続参照するため clone
        project_dir: project_dir.clone(),
        capabilities,
        // R3: wire cross-process delivery の宛先分類用 — 解決済 project 名
        project_name: project_name_for_remote.clone(),
        // VP-159 PR-4b: ActorRegistry を move (= lane-spawn は AppState 構築後に追加)
        actor_registry: Arc::new(RwLock::new(actor_registry)),
        world: None,
        update: None,
        // SP mode は hub federation を持たない（TheWorld のみ）→ Disabled / 空のまま。
        hub_status: crate::daemon::hub_client::HubFederationStatus::new(),
        hub_worlds: crate::daemon::hub_client::HubWorldsCache::new(),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: terminal_token.clone(),
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb: vpdb.clone(),
        // wiremsg R2-a: SP は wire store を持たない (TheWorld に中央化、 handler は proxy)
        wiremsg_store: None,
        // wire_notifier / delivery_notify は world mode 専用 (TheWorld の long-poll 起床 /
        // delivery loop wake)。 SP では未使用だが AppState 共有 field のため空で満たす
        wire_notifier: crate::capability::WireNotifier::new(),
        delivery_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        // Phase A4-2b: Lane scope の Stand pool — Conductor Lane 1 つ pre-populate
        // memory rule: 多 scope architecture (App/Project/Lane/Pane)、HD/TH は Lane scope。
        // Performer Lane の動的 create は A4-4、Stand spawn 連動は A5 で実装。
        //
        // (I-b、 2026-04-30): Performer auto-spawn は AppState 構築後に Mailbox actor 経由で実施。
        // lane performers を `LaneCmd::SpawnLane` Cmd 化して `lane-spawn` mailbox に投入する
        // (= concurrency 制御を `Arc<Semaphore::new(N)>` で表現、 N=config.startup.max_concurrent_lane_spawn)。
        // 詳細は run() 内 lane_spawn_actor wiring 参照。
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::with_conductor(
            project_name_for_remote.clone(),
            project_dir.clone(),
        ))),
        // Phase 2 (Step E): system 系 lifecycle event の central broadcast bus。
        // capacity 64 = lifecycle 変更が短時間に集中しても drop しない buffer。
        // caller publish (SystemEvent::Lane(LaneDiff::*) 等) + spawn_world_uplink subscribe
        // で SP → TheWorld push 経路。 将来 Pane / Stand 等の event も同 bus に variant 追加で乗る。
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        // Phase A4-2b: Project scope の Stand pool (PP/GE/HP) — skeleton
        project_stands: Arc::new(RwLock::new(
            super::project_stands_state::ProjectStandsPool::new(),
        )),
        // PR-α-1 (VP-111): SP モードでは WorldCapabilities を持たない (World mode 専用)
        world_capabilities: None,
        // PR-β-1 (VP-119): SP モードで LaneCapabilities pool 受け皿を Some で初期化。
        // 物理移管 (PP) は PR-β-2、 本 PR では空 HashMap で構築のみ。
        lane_capabilities: Some(Arc::new(RwLock::new(
            super::lane_capabilities::LaneCapabilitiesPool::new(),
        ))),
        terminal_pumps: Arc::new(RwLock::new(std::collections::HashMap::new())),
        // SP mode は delegation store を持たない (World 中央 store に proxy する)。
        delegation_store: None,
    });

    // Phase review fix #2: LanePool::with_conductor は内部で PtySlot::spawn (openpty + spawn_command)
    // で OS syscall ブロッキング → spawn_blocking で tokio worker thread (= tokio runtime の OS thread) を保護。
    // でも... AppState 既に構築済なので restructure したいけど不可。 代替:
    // with_conductor 自体は sync だが state 構築段階で `tokio::task::block_in_place` も使えない。
    // 結果的に SP 起動時 1 回だけの呼び出しなので影響は軽微。 review 指摘は記録、 現実装維持。
    // (`create_handler` 側の spawn_blocking 化は完了済 = lanes.rs の方が頻繁に呼ばれる重要 path)

    // ペイン状態をディスクから復元（前回 Process 終了時の状態 → RetainedStore）
    state.restore_pane_contents().await;

    // PR-β-2 (VP-120): Conductor Lane の LaneCapabilities entry を populate (LanePool::with_conductor と同期)。
    // PR-β-1 で空 HashMap だった lane_capabilities pool に、 Conductor Lane の独立 PaisleyParkState を host。
    // doc 13 §6 自動 spawn rule = Lane 起動時に PP 同時 spawn (default) を default で実現。
    if let Some(lc_pool) = state.lane_capabilities.as_ref() {
        let conductor_addr = super::lanes_state::LaneAddress::conductor(&project_name_for_remote);
        let default_stand = crate::config::Config::load()
            .unwrap_or_default()
            .default_stand_or_echoes()
            .to_string();
        lc_pool
            .write()
            .await
            .populate_lane(conductor_addr, default_stand);
        tracing::info!(
            "PR-β-2: LaneCapabilities pool に Conductor Lane populate (project={}, PP host 化)",
            project_name_for_remote
        );
    }

    // (I-b、 2026-04-30) Lane spawn actor を起動し、 既存 lane performers を Cmd 化して投入。
    // in-process channel + Semaphore で並列 spawn を gate する設計。
    // - actor は `cmd_rx` (unbounded channel) を recv し、 `Arc<Semaphore::new(N)>` で gate しつつ並列実行
    // - bootstrap は lane performers をスキャンして `LaneCmd::SpawnLane` を投入 (= 1 回限りの seed)。
    //   block 終端で Sender drop → actor は buffered Cmd を全 drain 後に正常終了する
    // - N=config.startup.max_concurrent_lane_spawn (default 1、 dogfood で計測 log を集計して tweak)
    // PR-β-2 (VP-120): lane_capabilities pool clone も渡し、 Performer spawn 時に populate_lane する。
    //
    // in-process 直結 (2026-07-09): 旧 wiremsg R2-a 経路 (TheWorld 中央 wire store の
    // `lane-spawn@<project>` mailbox 往復) を撤去。 producer は本 bootstrap のみで、 at-most-once
    // 配送 + SP 再起動時の幽霊 long-poll 消費で Cmd が失われ performer が永久 Spawning になる
    // 障害があった (詳細は lane_spawn_actor.rs module doc)。 channel は process-local なので
    // この failure mode が構造的に消滅し、 TheWorld 不達 retry も不要 (standalone SP でも spawn 可)。
    {
        let max_concurrent = crate::config::Config::load()
            .unwrap_or_default()
            .startup
            .max_concurrent_lane_spawn as usize;
        // bootstrap → actor の in-process 直結 channel。 unbounded なので send は同期・即時
        // (receiver 生存中は infallible)、 recv loop 開始前の send もバッファされる。
        let (lane_spawn_tx, lane_spawn_rx) =
            tokio::sync::mpsc::unbounded_channel::<super::lane_cmd::LaneCmd>();
        // VP-159 PR-4b: ActorRegistry 経由で spawn + register (= JoinHandle を registry が保持、
        // PR-5 supervisor 統一で abort / await を activate)。 Semaphore gate / race guard は完全互換。
        state.actor_registry.write().await.spawn_service(
            super::lane_spawn_actor::LaneSpawnActor::new(
                state.lane_pool.clone(),
                state.lane_capabilities.clone(), // PR-β-2 (VP-120): Performer spawn 時に populate_lane する
                state.system_event_tx.clone(),   // Phase 2 (Step E): system event central bus
                max_concurrent,
                lane_spawn_rx,
            ),
            shutdown_token.clone(),
        );

        let performers_project_id = std::path::Path::new(&state.project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        // project-local lane refactor PR 1: list_performers_for_repo は repo_root: &Path を受け取る。
        let performers = crate::lane::commands::list_performers_for_repo(std::path::Path::new(
            &state.project_dir,
        ));
        let total = performers.len();
        if total > 0 {
            tracing::info!(
                "SP startup bootstrap: {} 本の Performer SpawnLane Cmd を投入 (project_id={}, max_concurrent={})",
                total,
                performers_project_id,
                max_concurrent
            );
            // doc 11 PR-B: stand は String 化、 default は config の `default_stand`
            // (未設定なら "echoes" fallback、 PR-pre2 / VP-118 で "hd" → "echoes")。
            let default_stand = crate::config::Config::load()
                .unwrap_or_default()
                .default_stand_or_echoes()
                .to_string();
            // in-process 直結: 型付き LaneCmd を channel に同期 send (serialize / retry 不要)。
            // send が Err を返すのは receiver drop 後のみ (= actor task 終了後。 startup 時点では
            // 起き得ないが防御的に warn)。 投入順序は Semaphore gate が並列度を制御するため保証不要。
            for entry in &performers {
                let cmd = super::lane_cmd::LaneCmd::SpawnLane {
                    project_id: performers_project_id.clone(),
                    name: entry.name.clone(),
                    cwd: entry.path.clone(),
                    stand: default_stand.clone(),
                };
                if lane_spawn_tx.send(cmd).is_err() {
                    tracing::warn!(
                        "SP startup bootstrap: lane-spawn channel closed (actor 未起動?) name={}",
                        entry.name
                    );
                }
            }
        } else {
            tracing::info!(
                "SP startup bootstrap: lane performers なし (project_id={})",
                performers_project_id
            );
        }
    }

    // L0 finale (doc 27 §3.4.5): SP HTTP listener + QUIC listen 全廃 = SP 完全 portless (outbound-only)。
    // 旧 SP HTTP route / SP QUIC listen channel は全て World process-proxy reverse-routing に移管済:
    // - reconciliation (旧 /api/health port-scan) → Push-only registry (QUIC register/disconnect)
    // - MCP / lanes / tmux / ruby / canvas / wire 等 process 操作 → World process-proxy ask
    //   (World → SP control channel → run_control_driver → dispatch_process_method)
    // - stop_process の graceful shutdown も World process-proxy "shutdown" 経由
    // よって SP は listen を一切持たず、 registry / canvas-ingest / control の outbound stream のみ
    // (spawn_world_uplink)。 health/shutdown handler は run_world (World :32000) が使うため
    // routes/health.rs に残置。

    // SP-portless (doc 27 §3.4.5 / rebuild Epic L0 finale): SP は QUIC listen を持たない。
    // 全 process 操作は World :32000 → SP control channel の reverse-routing
    // (spawn_world_uplink の control stream → run_control_driver → dispatch_process_method) で
    // serve する。listen server (start_unison_server) は撤去済 = SP は outbound-only。

    // デバッグモード時のみトレースログ監視を起動
    if debug_mode != DebugMode::None {
        let hub_for_log = state.hub.clone();
        tokio::spawn(async move {
            crate::trace_log::watch_and_broadcast(hub_for_log).await;
        });
    }

    // F1a (doc 27 §3.4.4): SP → TheWorld の outbound を 1 connection に集約した uplink。
    // registry (自己登録 + heartbeat + lane diff push) / canvas-ingest (paisley-park /
    // terminal topic push) / control (World reverse-routing 受け) の 3 channel を 1 共有
    // QUIC connection 上の別 stream として張る (旧: 3 別 connection)。 依存は全て AppState
    // から引くため引数は (state, shutdown) のみ。 切断時に TheWorld が即時除去 (HTTP 登録不要)。
    crate::discovery::spawn_world_uplink(state.clone(), shutdown_token.clone());

    // wiremsg Stage 0: Lane lifecycle event を retained topic に publish する。
    // `SystemEvent::Lane` を購読し、LanePool の全 list snapshot を
    // `process/star-platinum/state/lanes`（category=state → RetainedStore で保持）へ流す。
    // hub.broadcast → Hub→TopicRouter pump → retained。consumer（Stage 1 で vp-app が
    // subscribe）不在でも no-op で安全。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
    {
        let mut sys_rx = state.system_event_tx.subscribe();
        let state_for_pub = state.clone();
        let hub = state.hub.clone();
        let shutdown = shutdown_token.clone();
        // 起動直後の現 snapshot を 1 度 publish して retained を seed する
        // （Conductor Lane は既に pre-populate 済）。
        // project-local lane refactor PR 1: build_lanes_snapshot で disk-scan Inactive Performer
        // も含める (= HTTP /api/lanes と同一 logic、 sidebar QUIC 経路でも Inactive 表示)。
        hub.broadcast(crate::protocol::ProcessMessage::LanesSnapshot {
            lanes: super::routes::lanes::build_lanes_snapshot(&state_for_pub).await,
        });
        tokio::spawn(async move {
            use super::lanes_state::SystemEvent;
            use tokio::sync::broadcast::error::RecvError;
            // project-local lane refactor PR 1: CLI `vp lane new` は SystemEvent::Lane を
            // fire しない (= 直 fs op、 SP 経由しない)。 disk-only performer を sidebar に届ける
            // safety net として 5s periodic tick で snapshot 再 publish する。
            // (FSEvents-based lane watcher の project-local 拡張は後 PR の範囲)
            let mut periodic = tokio::time::interval(std::time::Duration::from_secs(5));
            periodic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = periodic.tick() => {
                        let lanes = super::routes::lanes::build_lanes_snapshot(
                            &state_for_pub,
                        ).await;
                        hub.broadcast(
                            crate::protocol::ProcessMessage::LanesSnapshot { lanes },
                        );
                    }
                    ev = sys_rx.recv() => match ev {
                        // Lane lifecycle 変化 / lag → 現 snapshot を全量 publish（idempotent）
                        Ok(SystemEvent::Lane(_)) | Err(RecvError::Lagged(_)) => {
                            let lanes = super::routes::lanes::build_lanes_snapshot(
                                &state_for_pub,
                            ).await;
                            hub.broadcast(
                                crate::protocol::ProcessMessage::LanesSnapshot { lanes },
                            );
                        }
                        Err(RecvError::Closed) => break,
                    },
                }
            }
        });
    }

    // Phase 5-D: Lane lifecycle monitor — child PtySlot (例: `claude --continue`) が
    //   spawn_with_fallback の 800ms early-exit window を抜けた後で死んだ時に、
    //   Lane state を Dead に mark する periodic task。
    //   - 5s 間隔で全 Lane の is_alive() を check
    //   - Dead 検出 → state 更新 + pty_slots remove (zombie reap)
    //   - sidebar が /api/lanes を polling するので Dead 状態が UI に伝播
    //   関連: 2026-04-28 unison-kdl で post-spawn zombie 観測 → 検知機構が無く Conductor コンソール
    //         が壊れたまま user が気付かない問題の解消。
    spawn_lane_lifecycle_monitor(state.lane_pool.clone(), shutdown_token.clone());

    // Clone for shutdown
    let capabilities_for_shutdown = state.capabilities.clone();
    let file_watchers_for_shutdown = state.file_watchers.clone();

    // SP-portless: SP は listen を持たず、 run() は shutdown_token で block する。 process 操作は
    // spawn_world_uplink の control stream (reverse-routing) で process 生存中 serve され続ける。
    // World process-proxy `shutdown` / SIGTERM 経由で shutdown_token が cancel されると cleanup へ進む。
    shutdown_token_clone.cancelled().await;
    tracing::info!("Graceful shutdown initiated (outbound-only SP)");

    // QUIC Registry 切断で TheWorld が即時除去するため、明示的 unregister は不要
    // （spawn_world_uplink の shutdown handler が unregister を送信済み）

    // pane 状態は webview が /api/pp/state で逐次 pane_contents に保存済 (旧 Whitesnake
    // shutdown snapshot は退役)。 shutdown 時の明示保存は不要。

    // ファイル監視を全停止
    file_watchers_for_shutdown.lock().await.stop_all();

    // (tmux decoupling PR2: lane は PtySlot の子 — SP 停止で完全に落ちる)

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
///
/// `midi_config` (feature = "midi") は `vp daemon start --midi <arg>` で構築される MidiConfig。
/// `None` なら `MidiConfig::default()` を使う (PR-α-4 / VP-114 で復活した CLI 経路)。
pub async fn run_world(
    port: u16,
    #[cfg(feature = "midi")] midi_config: Option<crate::midi::MidiConfig>,
) -> Result<()> {
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
    // VP-182: World は `db/world/` 専用ディレクトリを使う (= SP の `db/sp_{slug}/` と
    // 分離、 surrealkv OS 排他ロックの衝突を回避)。
    let vpdb: Option<crate::db::SharedVpDb> = {
        let data_dir = crate::db::db_data_dir_for_world();
        match crate::db::VpDb::connect_embedded(&data_dir).await {
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

    // federation L2 (ADR-020 D2): home-World の位置独立 routing key `wld_id` を db/world から
    // load_or_create する。daemon が初回起動で 1 度発行し、 以降の再起動は復元する (machine /
    // hostname / endpoint から独立な不変番地)。db 不在 (degraded) なら None — その場合は
    // federation の routing key を名乗れないが machine-local 動作は継続する (= hub down 時と同 degrade)。
    let world_id: Option<crate::world::WorldId> = if let Some(ref db) = vpdb {
        match db.load_or_create_world_id().await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(
                    "wld_id 発行/復元に失敗 (federation routing なしで継続): {}",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    let world_cap = Arc::new(RwLock::new(world_cap));
    let update_cap = Arc::new(RwLock::new(update_cap));
    let hub = Hub::new();

    // TopicRouter（World モードでは Hub ブリッジ不要だが、AppState の必須フィールド）
    let topic_router = Arc::new(TopicRouter::new());

    // PR-α-1 (VP-111): World 階層 Stand を 1 instance ずつ生成して、 AppState 既存 field と
    // WorldCapabilities container の両方に share させる (二重生成は避ける)。
    //
    // PR-α-2 (VP-112): MidiCapability を World 階層に移管。 feature = "midi" 有効時は
    // `with_midi` で host 化、 無効時は `new` で空 placeholder のまま。
    //
    // PR-α-4 (VP-114): `vp daemon start --midi <arg>` で構築された MidiConfig を受け取り、
    // None なら `MidiConfig::default()` (= PR-α-2/3 後の既存挙動と同じ port auto-pick) で fallback。
    let world_capabilities = {
        #[cfg(feature = "midi")]
        {
            let resolved_midi_config = midi_config.unwrap_or_default();
            Arc::new(
                crate::daemon::world_capabilities::WorldCapabilities::with_midi(
                    world_cap.clone(),
                    update_cap.clone(),
                    resolved_midi_config,
                )
                .await?,
            )
        }
        #[cfg(not(feature = "midi"))]
        {
            Arc::new(crate::daemon::world_capabilities::WorldCapabilities::new(
                world_cap.clone(),
                update_cap.clone(),
            ))
        }
    };

    // Bastet 🧲 — ROTO 持続セッションを World lifecycle に enclose して起動する。
    // 前景 `vp midi roto control` のフル接続（open + handshake + keepalive + LCD/routing）を
    // daemon 常駐 + 自動再接続に昇格。lane data は world_cap(ProcessManagerCapability) を
    // in-process 直読み、switch_lane は SP 越境なので QUIC。shutdown_token の子 token で
    // graceful 停止する。bastet_for_shutdown は cleanup chain 用に Arc を clone しておく。
    #[cfg(feature = "midi")]
    let bastet_for_shutdown = world_capabilities.bastet.clone();
    #[cfg(feature = "midi")]
    if let Some(bastet) = world_capabilities.bastet.as_ref() {
        bastet
            .write()
            .await
            .start_roto_control(world_cap.clone(), shutdown_token.clone())
            .await;
    }

    // Phase A ① / R1: World モードでも wiremsg store を build (= 将来 World 階層 actor 用)。
    // R1 で `WiremsgStore::new` は async (起動時に local_seq 採番を math::max で復元)。
    let wiremsg_store = match vpdb.as_ref() {
        Some(db) => Some(
            crate::capability::WiremsgStore::new(std::sync::Arc::new(db.inner().clone())).await?,
        ),
        None => None,
    };

    // 委譲 (delegation) の World 中央 store (doc 28 §4 / §6)。wire と同じく TheWorld の DB に持つ。
    let delegation_store = vpdb
        .as_ref()
        .map(|db| crate::capability::DelegationStore::new(std::sync::Arc::new(db.inner().clone())));

    // chronista-hub federation の接続状態。run_hub_federation（writer）と AppState（= /api/health
    // reader）で同一 instance を共有する（World mode のみ更新、初期 Disabled）。
    let hub_status = crate::daemon::hub_client::HubFederationStatus::new();
    // hub registry の available worlds cache も同 pattern で共有（writer = run_hub_federation の
    // 定期 discover、reader = /api/health の `hub_worlds` field。初期 = 空）。
    let hub_worlds = crate::daemon::hub_client::HubWorldsCache::new();

    // Create minimal state for world mode
    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(SessionManager::new())),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode: DebugMode::None,
        shutdown_token: shutdown_token.clone(),
        hub_status: hub_status.clone(),
        hub_worlds: hub_worlds.clone(),
        project_dir: String::new(),
        // R3: World mode は cross-process forward の対象外 (= 自 project を持たない)
        project_name: String::new(),
        capabilities: Arc::new(
            ProcessCapabilities::new(CapabilityConfig {
                project_dir: String::new(),
            })
            .await,
        ),
        // VP-159 PR-4b: World mode では空で構築 (= World scope actor の register は後続 PR、
        // MidiCapability metadata register は dynamic routing vision 確定後)
        actor_registry: Arc::new(RwLock::new(crate::capability::ActorRegistry::new())),
        world: Some(world_cap.clone()),
        update: Some(update_cap.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "WORLD_DISABLED".to_string(),
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb: vpdb.clone(), // World モードでも DB 参照あり
        // Phase A ① / R1: World モードでも wiremsg store を build (上で async build 済)
        wiremsg_store,
        wire_notifier: crate::capability::WireNotifier::new(),
        // R2-b: wire delivery loop の即時 wake (world_wire_send_handler が command 着信で notify)
        delivery_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        // Phase A4-2b: World モードでは Lane / Project Stand を持たない (空 Pool で AppState を満たす)
        // 多 scope architecture: World は App scope の component、Lane/ProjectStand は Project scope
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::new())),
        // Phase 2 (Step E): system event central bus
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        project_stands: Arc::new(RwLock::new(
            super::project_stands_state::ProjectStandsPool::new(),
        )),
        // PR-α-1 (VP-111): World 階層 Stand container (LSCM doc 12 §3 / §9)
        world_capabilities: Some(world_capabilities),
        // PR-β-1 (VP-119): World mode では LaneCapabilities を持たない (Lane scope は SP per project)
        lane_capabilities: None,
        // S2: World mode は SP の per-lane pump を持たない (terminal pump は SP scope)。
        terminal_pumps: Arc::new(RwLock::new(std::collections::HashMap::new())),
        // 委譲 (delegation) の World 中央 store (doc 28 §6)。World mode のみ Some。
        delegation_store,
    });

    // tmux decoupling PR1: SP control channel registry を hoist する。 daemon server (下記
    // daemon_state) がこの map を populate し、 World-side の nudge loop (delivery / reconcile) が
    // 同一 Arc を引いて `lane_nudge` を所有 SP に forward する。 別々に new() すると map が分裂して
    // forward 不能になるため、 ここで作った 1 つを 3 者 (daemon_state + 両 loop) に配る。
    let control_channels: crate::daemon::server::ControlChannels =
        std::sync::Arc::new(RwLock::new(std::collections::HashMap::new()));

    // R2-b: wire delivery loop (未 ack command の nudge + 再掲示) を spawn。
    // store 未構築 (DB 接続失敗) なら skip — wire 自体が動かないため delivery も不要。
    if let Some(store) = state.wiremsg_store.clone() {
        let lane_registry = world_cap.read().await.lane_registry_ref();
        state.actor_registry.write().await.spawn_service(
            super::delivery_actor::DeliveryActor::new(
                store,
                lane_registry,
                control_channels.clone(),
                state.delivery_notify.clone(),
            ),
            shutdown_token.clone(),
        );
    }

    // 委譲 reconcile loop (doc 28 §7、 Push+Pull の Pull パス) を spawn。
    // delivered=false の再 nudge + stale な未終了の timeout → Failed{timeout}。
    // World-side wake (lane_registry + SP-proxy lane_nudge) なので delivery loop と同じ
    // lane_registry / control_channels を使う。
    if let Some(store) = state.delegation_store.clone() {
        let lane_registry = world_cap.read().await.lane_registry_ref();
        super::delegation::spawn_reconcile_loop(
            store,
            lane_registry,
            control_channels.clone(),
            shutdown_token.clone(),
        );
    }

    let app = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        // L0 portless: `/ws/lanes` (project_feed WS) は consumer 消滅で dead のため撤去。
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
        .route(
            "/api/world/projects/reload",
            post(world::world_reload_projects),
        )
        .route("/api/world/projects/set_slot", post(world::world_set_slot))
        .route(
            "/api/world/projects/unassign_slot",
            post(world::world_unassign_slot),
        )
        .route("/api/world/projects/sync", post(world::world_sync_projects))
        .route("/api/world/processes", get(world::world_list_processes))
        .route(
            "/api/world/lanes",
            get(world::world_list_lanes).post(world::world_create_lane),
        )
        .route(
            "/api/world/lanes/active",
            post(world::world_set_active_lane),
        )
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
        // L0 portless B-4 (wire-unison): 中央 wire/delegation store の HTTP 入口 (`/api/wire/*`
        // `/api/delegation/*`) は daemon の "wire" unison channel に移行 (doc 27 §62)。
        // `world_wire::call` が QUIC で叩き、 `handle_wire_channel` が `routes::{wire,delegation}::
        // dispatch_*` に振る。 観測 (`vp wire deleg-thread`) / pull-hook (`vp wire hook-check`) も
        // 同 channel 経由。
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
        // VP-165 PR-6: slot ベース SP port resolver (decision C 完成、TheWorld を port authority に)
        .route("/api/world/port_for", get(world::world_port_for))
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
        .layer(CorsLayer::permissive())
        // L0 portless B-4: state は後段の daemon_state_builder.with_wire でも参照するため clone
        // (Arc clone は安価、 router と daemon QUIC server が同一 AppState を共有)。
        .with_state(state.clone());

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
    // Phase 1b: lane_registry も共有 (SP register の lanes payload を cache する)
    let lane_registry_ref = world_cap.read().await.lane_registry_ref();
    // L1 lifecycle: process_presence も共有 (registry handler が presence を遷移させる)
    let process_presence_ref = world_cap.read().await.process_presence_ref();
    let mut daemon_state_builder = crate::daemon::server::DaemonState::new()
        .with_running_processes(
            running_processes_ref,
            projects_ref,
            lane_registry_ref,
            process_presence_ref,
        )
        // control plane 一元化: world_cap (= HTTP AppState.world と同一 Arc) を共有し、
        // Unison "world-control" channel から projects mutation を受けられるようにする。
        .with_world_cap(world_cap.clone())
        // tmux decoupling PR1: 上で hoist した control channel map を daemon server と共有する
        // (daemon が SP 接続で populate → nudge loop がここから forward 先を引く)。
        .with_control_channels(control_channels.clone());
    // doc 24 §10 Phase 2: lane descriptor の durable 永続先 (capability boot load と同一 db)。
    if let Some(ref db) = vpdb {
        daemon_state_builder = daemon_state_builder.with_vpdb(db.clone());
    }
    // L0 portless B-4 (wire-unison): World 中央 wire/delegation store を daemon QUIC server と共有する。
    // `state` (World process AppState) が保持する **同一 Arc** を渡す (同一プロセス) — "wire" channel が
    // これを使って旧 `/api/wire/*` `/api/delegation/*` HTTP を unison channel で serve する (doc 27 §62)。
    daemon_state_builder = daemon_state_builder.with_wire(
        state.wiremsg_store.clone(),
        state.wire_notifier.clone(),
        state.delivery_notify.clone(),
        state.delegation_store.clone(),
    );
    // Bastet 🧲 EventBus を共有 — world-device channel が device event を vp-app に bridge する。
    // world_capabilities は L810 で move 済みなので、 move 前に clone した bastet_for_shutdown を使う。
    #[cfg(feature = "midi")]
    if let Some(bastet) = bastet_for_shutdown.as_ref() {
        let event_bus = bastet.read().await.event_bus().clone();
        daemon_state_builder = daemon_state_builder.with_bastet_event_bus(event_bus);
        // M2 / doc 26 §2: device channel (agent → daemon) が registry を更新するため registry 本体も共有。
        daemon_state_builder = daemon_state_builder.with_bastet(bastet.clone());
    }
    let daemon_state = std::sync::Arc::new(daemon_state_builder);
    let daemon_handle = tokio::spawn(crate::daemon::server::start_daemon_server(
        daemon_state,
        port,
    ));
    tracing::info!(
        "Daemon QUIC サーバー統合起動 (port: {}, registry チャネル有効)",
        port
    );

    // chronista-hub federation (opt-in): hub addr（env `CHRONISTA_HUB_ADDR` > config.kdl `hub-addr`、
    // `hub_client::hub_addr()` が解決）が設定されていれば、この world を hub registry に register
    // （他 world から discover 可能に）し、**relay の target inbound を常駐で受ける**（ADR-020 §S4）。
    // 旧実装は起動時に register して即 drop する使い捨てだったが、relay 受信には接続維持が必要なため
    // 常駐セッション（[`run_hub_federation`]）へ昇格した（接続が切れたら自律再接続）。未設定なら
    // machine-local 動作（= skip）。SSOT 原則により hub と話すのは TheWorld のみ。
    if let Some(hub_addr) = crate::daemon::hub_client::hub_addr() {
        // handle = この machine の identity（OS hostname → "vp-world" fallback）。
        let handle = crate::daemon::hub_client::resolve_handle(None);
        let name = format!("VP World ({handle})");
        // wld_id = federation の位置独立 routing key (ADR-020 D2)。db 不在で None なら空文字を
        // 送る (= 現状 hub は S2 未実装で無視するため非破壊、 handle ベース discover は維持)。
        let wld_id = world_id
            .as_ref()
            .map(|w| w.as_str().to_string())
            .unwrap_or_default();
        // endpoints = direct 到達候補 (ADR-020 D3-a、IPv6 GUA 優先・tailnet 非依存)。IPv6 経路が
        // 無ければ空配列 (= direct 候補なし、 dialer は relay floor に落ちる)。
        let endpoints = crate::world::endpoint::local_advertised_endpoints(port);

        // relay → VP wire 配送ポリシー（flow ③+⑤）。別 world が relay で送ってきた wire envelope
        // (`{from, to, body}`) を **ローカル中央 wire store に inject** する（= 遠方からの relay を
        // 「ローカル送信」に畳む）。宛先 lane は `wire_recv` で普通に拾う。store/notifier/notify は
        // AppState の Arc を capture（再接続ごとに handler 再登録するため closure は Clone）。
        let wire_store = state.wiremsg_store.clone();
        let wire_notifier = state.wire_notifier.clone();
        let wire_notify = state.delivery_notify.clone();
        // discovery（flow step 2）: lanes-query に応答するため lane_registry と hub_addr も capture。
        let fed_lane_registry = world_cap.read().await.lane_registry_ref();
        let fed_hub_addr = hub_addr.clone();
        let on_relay = move |inbound: crate::daemon::hub_client::RelayInbound| {
            let store = wire_store.clone();
            let notifier = wire_notifier.clone();
            let notify = wire_notify.clone();
            let lane_registry = fed_lane_registry.clone();
            let hub_addr = fed_hub_addr.clone();
            async move {
                // envelope の kind で分岐: wire（既定 = メッセージ配送）/ lanes-query（discovery 要求）。
                let kind = inbound
                    .payload
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("wire");
                if kind == "lanes-query" {
                    // discovery: 自分の lane を集めて reply_to（送信元の一時 wld_id）へ lanes-reply を
                    // relay で返す（片方向 relay × 2 で request-response を創発）。
                    let Some(reply_to) = inbound.payload.get("reply_to").and_then(|v| v.as_str())
                    else {
                        tracing::warn!("lanes-query に reply_to が無い — drop");
                        return;
                    };
                    let request_id = inbound
                        .payload
                        .get("request_id")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    // lane_registry（project_path → Vec<LaneInfo>）を flatten。discovery は未認証の
                    // 相手にも返り得る（federation auth は当面 permissive）ため、**allow-list で
                    // {address, kind, name, state} だけ**に絞る。LaneInfo の cwd（FS パス）/
                    // performer_status（git 状態）/ pid 等の sensitive field は漏らさない（露出最小化）。
                    // 本丸の「誰が discover できるか」の gate は S3 auth（Creo ID）で別途。
                    let lanes: Vec<serde_json::Value> = {
                        let reg = lane_registry.read().await;
                        reg.values()
                            .flatten()
                            .filter_map(|l| serde_json::to_value(l).ok())
                            .map(|v| {
                                serde_json::json!({
                                    "address": v.get("address"),
                                    "kind": v.get("kind"),
                                    "name": v.get("name"),
                                    "state": v.get("state"),
                                })
                            })
                            .collect()
                    };
                    let n = lanes.len();
                    let reply = serde_json::json!({
                        "kind": "lanes-reply",
                        "request_id": request_id,
                        "lanes": lanes,
                    });
                    let from_label = crate::daemon::hub_client::resolve_handle(None);
                    match crate::daemon::hub_client::relay_send_to_wld(
                        &hub_addr,
                        reply_to,
                        &from_label,
                        &reply,
                    )
                    .await
                    {
                        Ok(_) => tracing::info!("lanes-query に応答: {n} lanes → {reply_to}"),
                        Err(e) => tracing::warn!("lanes-reply 返信に失敗（to={reply_to}）: {e}"),
                    }
                    return;
                }
                // wire メッセージ → ローカル中央 store へ inject（flow ⑤）。
                let Some(store) = store else {
                    tracing::warn!(
                        from = %inbound.from,
                        "federation relay 受信したが wire store 不在（db なし）— drop"
                    );
                    return;
                };
                match crate::process::routes::wire::dispatch_wire(
                    &store,
                    &notifier,
                    &notify,
                    "send",
                    inbound.payload,
                )
                .await
                {
                    Ok(_) => tracing::info!(
                        from = %inbound.from,
                        "federation relay → VP wire 配送成功"
                    ),
                    Err(e) => tracing::warn!(
                        from = %inbound.from,
                        "federation relay → VP wire 配送失敗: {}", e
                    ),
                }
            }
        };

        // 常駐ループ。接続/登録失敗は run_hub_federation 内で warn に落として再接続（degradation）。
        // hub_status / hub_worlds は AppState と共有（run_hub_federation が更新、/api/health が読む）。
        tokio::spawn(crate::daemon::hub_client::run_hub_federation(
            hub_addr,
            wld_id,
            endpoints,
            handle,
            name,
            hub_status,
            hub_worlds,
            shutdown_token.clone(),
            on_relay,
        ));
    } else {
        tracing::debug!(
            "chronista-hub federation 無効 (env {} / config.kdl hub-addr とも未設定) — machine-local 動作",
            crate::daemon::hub_client::HUB_ADDR_ENV
        );
    }

    // ヘルスモニター起動（30秒間隔で Process 監視 + ゴースト除去 + クラッシュ復旧）
    let health_monitor = tokio::spawn(ProcessManagerCapability::run_health_monitor(
        world_cap.clone(),
        shutdown_token.clone(),
    ));

    // 起動時設定の復帰: enabled な project の SP を自動起動（VP-207）。
    // daemon restart 後に working set を復元する。1 回限りの startup タスク。
    let _autostart = tokio::spawn(ProcessManagerCapability::autostart_enabled_projects(
        world_cap.clone(),
    ));

    // VP-129 MVP: lane root FSEvents watcher 起動。 user の Finder / `rm -rf` で performer dir
    // を削除した時、 OS file system event → SP `DELETE /api/lanes` 自動発火 (= D10 Reconciliation
    // の 3rd path 拡張、 Push QUIC + Pull port scan + FSEvents の 3-trigger model 完成)。
    let _lane_watcher = tokio::spawn(ProcessManagerCapability::run_lane_watcher(
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
                                        crate::db::Action::Create => "started",
                                        crate::db::Action::Update => "updated",
                                        crate::db::Action::Delete => "stopped",
                                        _ => "changed",
                                    };

                                    tracing::info!(
                                        "LIVE SELECT: {} '{}' (port={})",
                                        event,
                                        project_name,
                                        port_val
                                    );
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
    // Bastet ROTO 持続セッションを停止（子 token は shutdown_token から伝播済だが、明示 abort で確実に畳む）。
    #[cfg(feature = "midi")]
    if let Some(bastet) = bastet_for_shutdown.as_ref() {
        bastet.write().await.stop_roto_control().await;
    }
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
/// - `[::]` (IPv6 wildcard) に bind し、 IPV6_V6ONLY を明示 false にすることで
///   `127.0.0.1:port` と `[::1]:port` の両方の client が同じ listener に届く。
/// - **Windows 必須**: Windows は default で `IPV6_V6ONLY=true` のため、 これを明示
///   false に setsockopt しないと IPv4 client (vp-app の `http://127.0.0.1:32000` 等) が
///   接続不能になる。 macOS / Linux は default で false だが、 platform 差異を消すため
///   全 OS で明示設定する。 tokio の `TcpSocket` API には `set_only_v6` が無いため
///   `socket2` 経由で raw socket option を叩く。
///
/// 関連: SP register が `http://[::1]:32000` で TheWorld に register していた箇所が
///   旧 `0.0.0.0:port` listen で connection refused していた問題の根治。
async fn bind_dual_stack(port: u16) -> Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let addr = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0);
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    // IPV6_V6ONLY=0 — IPv4 client も IPv4-mapped IPv6 経由で受け付ける (cross-platform で明示)
    socket.set_only_v6(false)?;
    // tokio は non-blocking 必須
    socket.set_nonblocking(true)?;
    // SO_REUSEADDR は port reuse 用、 LISTEN backlog は default 128
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    let std_listener: std::net::TcpListener = socket.into();
    Ok(tokio::net::TcpListener::from_std(std_listener)?)
}

/// Phase 5-D: Lane lifecycle monitor — periodic task that detects Lane の child process が後で
/// 死んだ場合に state=Dead を mark する。
///
/// ## 動機
/// `spawn_with_fallback` の 800ms early-exit window では `claude --continue` が後で
/// (= spawn 後 1 秒以上経ってから) exit するパターンを捕まえられない。
/// 2026-04-28 dogfooding で unison-kdl が zombie 化、 sidebar には running 表示、
/// PTY write が `Input/output error (os error 5)` で失敗、 Conductor コンソールが壊れた状態
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
