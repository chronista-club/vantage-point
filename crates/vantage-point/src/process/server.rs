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
use super::routes::{health, lanes, project_feed, prompt, stands, update, world, ws_terminal};
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
    let config_for_init = crate::config::Config::load().unwrap_or_default();

    // VP-165 (doc 17 決定B): Whitesnake を project slug 別ディレクトリで早期初期化
    // （Msgbox persistence で使用）。旧 port-keyed (`discs/{port}/`) は port が
    // project リスト変更で reshuffle する不安定 ID だったため `discs/p_{slug}/` に。
    let whitesnake = crate::capability::Whitesnake::file_backed_for_project(
        &crate::resolve::project_slug(&project_dir, &config_for_init),
    );
    cap_config.whitesnake = Some(whitesnake.clone());

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
                tracing::warn!("SP: SurrealDB 未接続、DB なしで継続: {}", e);
                None
            }
        }
    };

    // VP-159 PR-4b: Stand / Service actor の supervisor 受け皿。 SP-local Service (= notify /
    // lane-spawn) を `spawn_service` 経由で起動・register、 JoinHandle を保持。 World scope の
    // MidiCapability metadata register は dynamic routing vision 確定後 (cf. design-spark
    // mem_1CavFi5D1aMSpEkas89SvQ)、 PR-5 supervisor 統一で JoinHandle 経由 abort を activate。
    let mut actor_registry = crate::capability::ActorRegistry::new();

    // Phase A ① / R1: wiremsg threaded inbox store。 msgs table と並存。
    // R1 で `WiremsgStore::new` は async (起動時に local_seq 採番を math::max で復元)。
    // wiremsg R4: group B actor (notify / lane-spawn) の recv 元なので、 actor spawn より
    // 先に build しておく。
    let wiremsg_store = match vpdb.as_ref() {
        Some(db) => Some(
            crate::capability::WiremsgStore::new(std::sync::Arc::new(db.inner().clone())).await?,
        ),
        None => None,
    };
    // wiremsg long-poll の in-process notifier。 group B actor と AppState で共有するため
    // ここで 1 つ作り clone を配る (WireNotifier は内部 Arc で実体共有)。
    let wire_notifier = crate::capability::WireNotifier::new();

    // Notification ブリッジ: wire `notify@<project>` → DistributedNotification
    // wiremsg R4 (group B 移行): 旧 WhitesnakeStore.claim polling を廃し、 wire accumulation の
    // per-agent cursor recv に rewire。 producer は `wire_send(to=["notify@<project>"])`。
    actor_registry.spawn_service(
        super::notification_actor::NotificationActor::new(
            wiremsg_store.clone(),
            wire_notifier.clone(),
            project_name_for_remote.clone(),
            project_dir.clone(),
        ),
        shutdown_token.clone(),
    );

    // VP-179 (Phase 5): TheWorld registry への actor register snapshot は廃止。
    // 旧実装は mpsc MsgboxRouter の `addresses()` (= register("agent") 等で蓄積された
    // address list) を TheWorld に flat 登録していたが、 全 register caller が VP-178
    // (Phase 4) で撤去済のため空 vec を渡す no-op に成り下がっていた。 cross-process
    // forward が必要な場合は msgs table 経由の discovery (= 別 epic) を検討。

    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(sessions)),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
        // Phase A4-2b: lane_pool init で同 var を後続参照するため clone
        project_dir: project_dir.clone(),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities,
        // R3: wire cross-process delivery の宛先分類用 — 解決済 project 名
        project_name: project_name_for_remote.clone(),
        // VP-159 PR-4b: notify を spawn_service 済の ActorRegistry を move (= lane-spawn は AppState 構築後に追加)
        actor_registry: Arc::new(RwLock::new(actor_registry)),
        world: None,
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
        vpdb: vpdb.clone(),
        // Phase A ① / R1: wiremsg threaded inbox store (上で async build 済)。
        wiremsg_store,
        // Phase A ①: wiremsg long-poll の in-process notifier
        // wiremsg R4: run() 冒頭で build した notifier を move (group B actor と同一実体共有)
        wire_notifier,
        // ポート別ディレクトリで分離（複数プロセスの namespace 衝突を防ぐ）
        // run() 冒頭で作成した Whitesnake を共有（Msgbox persistent と同一インスタンス）
        whitesnake: whitesnake.clone(),
        // Phase A4-2b: Lane scope の Stand pool — Lead Lane 1 つ pre-populate
        // memory rule: 多 scope architecture (App/Project/Lane/Pane)、HD/TH は Lane scope。
        // Wing Lane の動的 create は A4-4、Stand spawn 連動は A5 で実装。
        //
        // (I-b、 2026-04-30): Wing auto-spawn は AppState 構築後に Mailbox actor 経由で実施。
        // 旧 PR #228 の `populate_workers_from_disk` (= 旧 Worker 名称時代、 削除済) sync 経路は
        // 削除し、 lane wings を
        // `LaneCmd::SpawnLane` Cmd 化して `lane-spawn` mailbox に投入する設計に移行
        // (= concurrency 制御を `Arc<Semaphore::new(N)>` で表現、 N=config.startup.max_concurrent_lane_spawn)。
        // 詳細は run() 内 lane_spawn_actor wiring 参照。
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::with_lead(
            project_name_for_remote.clone(),
            project_dir.clone(),
        ))),
        // Phase 2 (Step E): system 系 lifecycle event の central broadcast bus。
        // capacity 64 = lifecycle 変更が短時間に集中しても drop しない buffer。
        // caller publish (SystemEvent::Lane(LaneDiff::*) 等) + spawn_registry_keepalive subscribe
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
    });

    // Phase review fix #2: LanePool::with_lead は内部で PtySlot::spawn (openpty + spawn_command)
    // で OS syscall ブロッキング → spawn_blocking で tokio worker thread (= tokio runtime の OS thread) を保護。
    // でも... AppState 既に構築済なので restructure したいけど不可。 代替:
    // with_lead 自体は sync だが state 構築段階で `tokio::task::block_in_place` も使えない。
    // 結果的に SP 起動時 1 回だけの呼び出しなので影響は軽微。 review 指摘は記録、 現実装維持。
    // (`create_handler` 側の spawn_blocking 化は完了済 = lanes.rs の方が頻繁に呼ばれる重要 path)

    // ペイン状態をディスクから復元（前回 Process 終了時の状態 → RetainedStore）
    state.restore_pane_contents().await;

    // PR-β-2 (VP-120): Lead Lane の LaneCapabilities entry を populate (LanePool::with_lead と同期)。
    // PR-β-1 で空 HashMap だった lane_capabilities pool に、 Lead Lane の独立 PaisleyParkState を host。
    // doc 13 §6 自動 spawn rule = Lane 起動時に PP 同時 spawn (default) を default で実現。
    if let Some(lc_pool) = state.lane_capabilities.as_ref() {
        let lead_addr = super::lanes_state::LaneAddress::lead(&project_name_for_remote);
        let default_stand = crate::config::Config::load()
            .unwrap_or_default()
            .default_stand_or_echoes()
            .to_string();
        lc_pool
            .write()
            .await
            .populate_lane(lead_addr, default_stand);
        tracing::info!(
            "PR-β-2: LaneCapabilities pool に Lead Lane populate (project={}, PP host 化)",
            project_name_for_remote
        );
    }

    // (I-b、 2026-04-30) Lane spawn actor を起動し、 既存 lane wings を Cmd 化して投入。
    // 旧 PR #228 の sync `populate_workers_from_disk` (= 旧 Worker 名称時代の API、 削除済)
    // 経路を Mailbox actor + Semaphore に置換。
    // - actor は `lane-spawn` mailbox を recv し、 `Arc<Semaphore::new(N)>` で gate しつつ並列実行
    // - bootstrap は lane wings をスキャンして `LaneCmd::SpawnLane` を投入 (= 1 回限りの seed)
    // - N=config.startup.max_concurrent_lane_spawn (default 1、 dogfood で計測 log を集計して tweak)
    // PR-β-2 (VP-120): lane_capabilities pool clone も渡し、 Wing spawn 時に populate_lane する。
    {
        // wiremsg R4: wire accumulation store + notifier に rewire (= 旧 WhitesnakeStore.claim 廃止)
        let lane_spawn_store = state.wiremsg_store.clone();
        let max_concurrent = crate::config::Config::load()
            .unwrap_or_default()
            .startup
            .max_concurrent_lane_spawn as usize;
        // VP-159 PR-4b: ActorRegistry 経由で spawn + register (= JoinHandle を registry が保持、
        // PR-5 supervisor 統一で abort / await を activate)。 Semaphore gate / race guard は完全互換。
        state.actor_registry.write().await.spawn_service(
            super::lane_spawn_actor::LaneSpawnActor::new(
                lane_spawn_store,
                state.wire_notifier.clone(), // wiremsg R4: long-poll 起床
                project_name_for_remote.clone(), // wiremsg R4: `lane-spawn@<project>` の project
                state.lane_pool.clone(),
                state.lane_capabilities.clone(), // PR-β-2 (VP-120): Wing spawn 時に populate_lane する
                state.system_event_tx.clone(),   // Phase 2 (Step E): system event central bus
                max_concurrent,
            ),
            shutdown_token.clone(),
        );

        // wiremsg R4: bootstrap producer も wire accumulation に rewire。
        // `lane-spawn@<project>` 宛に `send_root` → LaneSpawnActor が wire recv で取り出す。
        let bootstrap_store = state.wiremsg_store.clone();
        let bootstrap_from = format!("sp-bootstrap@{}", project_name_for_remote);
        let lane_spawn_addr = format!("lane-spawn@{}", project_name_for_remote);
        let wings_project_id = std::path::Path::new(&state.project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        // project-local lane refactor PR 1: list_wings_for_repo は repo_root: &Path を受け取る。
        let wings =
            crate::lane::commands::list_wings_for_repo(std::path::Path::new(&state.project_dir));
        let total = wings.len();
        if total > 0 {
            tracing::info!(
                "SP startup bootstrap: {} 本の Wing SpawnLane Cmd を投入 (project_id={}, max_concurrent={})",
                total,
                wings_project_id,
                max_concurrent
            );
            for entry in wings {
                // doc 11 PR-B: stand は String 化、 default は config の `default_stand`
                // (未設定なら "echoes" fallback、 PR-pre2 / VP-118 で "hd" → "echoes")。
                let default_stand = crate::config::Config::load()
                    .unwrap_or_default()
                    .default_stand_or_echoes()
                    .to_string();
                let cmd = super::lane_cmd::LaneCmd::SpawnLane {
                    project_id: wings_project_id.clone(),
                    name: entry.name.clone(),
                    cwd: entry.path.clone(),
                    stand: default_stand,
                };
                // wire `lane-spawn@<project>` 宛に send_root → LaneSpawnActor が wire recv で
                // handle_cmd する。 send 後に WireNotifier.notify で actor の long-poll を起こす。
                if let Some(store) = &bootstrap_store {
                    let body = match serde_json::to_value(&cmd) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(
                                "SP startup bootstrap: LaneCmd serialize 失敗 name={} err={}",
                                entry.name,
                                e
                            );
                            continue;
                        }
                    };
                    match store
                        .send_root(
                            &bootstrap_from,
                            std::slice::from_ref(&lane_spawn_addr),
                            body,
                        )
                        .await
                    {
                        Ok(_) => state.wire_notifier.notify(&lane_spawn_addr).await,
                        Err(e) => tracing::warn!(
                            "SP startup bootstrap: wire send_root 失敗 name={} cwd={} err={}",
                            entry.name,
                            entry.path,
                            e
                        ),
                    }
                } else {
                    tracing::warn!(
                        "SP startup bootstrap: wiremsg_store 未配線、 Wing spawn skip name={}",
                        entry.name
                    );
                }
            }
        } else {
            tracing::info!(
                "SP startup bootstrap: lane wings なし (project_id={})",
                wings_project_id
            );
        }
    }

    let app = Router::new()
        .route("/", get(health::index_handler))
        .route("/canvas", get(health::canvas_handler))
        .route("/vendor/{filename}", get(health::vendor_handler))
        // 旧 `.route("/wasm/{filename}", ...)` (vp-mdast-wasm 配信) は 2026-05-25 削除
        // (= frontend は marked + creoui-editor-host に移行済、 dead endpoint)。
        // wiremsg Stage 3: `/ws` endpoint は撤去済。Canvas が Stage 2 で "canvas" topic 購読に
        // 移行した結果 `/ws` の接続 client が消滅 (= dead)。chat/permission の双方向経路も
        // Echoes が tmux+claude に移行して以降 unused。
        // Canvas Project Feed 集約 WebSocket（全 Process のメッセージを Project Feed でラップして中継）
        // 注: URL `/ws/lanes` は外部互換のため維持。内部命名は `project_feed` (mem_1CaSsN7xj69aVQtLPQFJxQ 命名整理)
        .route("/ws/lanes", get(project_feed::project_feed_ws_handler))
        // Phase 2 (Architecture v4): vp-app から Lane の PtySlot に attach する WS endpoint。
        // `?lane=<address>` で既存 LanePool の PtySlot に subscribe + write 経路を貼る。
        // 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Lane = Session Process)
        .route("/ws/terminal", get(ws_terminal::ws_terminal_handler))
        // Phase A4-2b: Lane (Lead/Wing) lifecycle の REST endpoint
        // GET: list、 POST: Wing create (A6 minimum)
        .route(
            "/api/lanes",
            get(lanes::list_handler)
                .post(lanes::create_handler)
                .delete(lanes::delete_handler),
        )
        // Lane の Lead Stand restart (PtySlot kill + 同 stand で respawn)
        .route("/api/lanes/restart", post(lanes::restart_handler))
        // doc 11 §4.1 PR-C: 利用可能な Stand 一覧 (sidebar の + Add Wing dropdown 用)
        .route("/api/stands", get(stands::list_handler))
        .route("/api/show", post(health::show_handler))
        // R3: cross-process wire delivery — 他 SP からの forward 受信口
        .route(
            "/api/wire/remote-deliver",
            post(health::wire_remote_deliver_handler),
        )
        // wiremsg R5-2: wire accumulation 経路の HTTP 入口 (旧 /api/msgbox/* を置換)
        .route("/api/wire/send", post(health::wire_send_handler))
        .route("/api/wire/recv", post(health::wire_recv_handler))
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
        .route(
            "/api/world/projects/reload",
            post(world::world_reload_projects),
        )
        .route("/api/world/processes", get(world::world_list_processes))
        .route("/api/world/lanes", get(world::world_list_lanes))
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
        state.lane_pool.clone(),
        state.system_event_tx.clone(), // Phase 2 (Step E): system event central bus
        shutdown_token.clone(),
    );

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
        // （Lead Lane は既に pre-populate 済）。
        // project-local lane refactor PR 1: build_lanes_snapshot で disk-scan Inactive Wing
        // も含める (= HTTP /api/lanes と同一 logic、 sidebar QUIC 経路でも Inactive 表示)。
        hub.broadcast(crate::protocol::ProcessMessage::LanesSnapshot {
            lanes: super::routes::lanes::build_lanes_snapshot(&state_for_pub).await,
        });
        tokio::spawn(async move {
            use super::lanes_state::SystemEvent;
            use tokio::sync::broadcast::error::RecvError;
            // project-local lane refactor PR 1: CLI `vp lane new` は SystemEvent::Lane を
            // fire しない (= 直 fs op、 SP 経由しない)。 disk-only wing を sidebar に届ける
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
    //   関連: 2026-04-28 unison-kdl で post-spawn zombie 観測 → 検知機構が無く Lead コンソール
    //         が壊れたまま user が気付かない問題の解消。
    spawn_lane_lifecycle_monitor(state.lane_pool.clone(), shutdown_token.clone());

    // VP-154 PR-1: SP も自身を mDNS で broadcast (= per-project unique instance)。
    // instance_name は `sp-<project>-<localhost>` 形式 (例: `sp-creo-ui-mito-mac-4`)、
    // TXT record に `kind=sp` + `project=<name>` + `port=<sp_port>` を含める。
    // World announce (= `world-<localhost>`) と instance namespace が分離、 collision なし。
    // 戻り値の MdnsAnnouncer は serve 終了 (= graceful shutdown) で scope exit、 Drop で auto deregister。
    //
    // Moody Blues fix #1 (Score 82): announce() は内部で `os_local_hostname()` (= scutil
    // shell-out) を呼ぶ sync blocking call、 tokio async context から直接 call せず
    // `spawn_blocking` で wrap して tokio worker thread 占有を回避 (= VP-153 fix と整合)。
    let project_for_announce = project_name_for_remote.clone();
    // VP-154 PR-3.5: config の advertise_hostname を読んで announce に渡す。
    // OS LocalHostName auto-increment 由来の identity 揺れを config 固定で吸収。
    let identity_override_for_announce = crate::config::Config::load()
        .ok()
        .and_then(|c| c.network.advertise_hostname);
    let _sp_mdns_announcer = match tokio::task::spawn_blocking(move || {
        crate::lan_discovery::announce(
            crate::lan_discovery::AnnounceKind::Sp {
                project: project_for_announce,
            },
            port,
            crate::lan_discovery::PUBKEY_PLACEHOLDER,
            identity_override_for_announce.as_deref(),
        )
    })
    .await
    {
        Ok(Ok(a)) => Some(a),
        Ok(Err(e)) => {
            tracing::warn!(
                "mDNS announce 失敗 (SP {} LAN discovery 不能、 起動継続): {}",
                project_name_for_remote,
                e
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "mDNS announce spawn_blocking join 失敗 (SP {}): {}",
                project_name_for_remote,
                e
            );
            None
        }
    };

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

    // VP-154 PR-1: graceful shutdown 後、 _sp_mdns_announcer が drop されて deregister。
    tracing::debug!("SP mDNS announcer dropping (deregister via Drop trait)");

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

    let world_cap = Arc::new(RwLock::new(world_cap));
    let update_cap = Arc::new(RwLock::new(update_cap));
    let hub = Hub::new();

    // TopicRouter（World モードでは Hub ブリッジ不要だが、AppState の必須フィールド）
    let topic_router = Arc::new(TopicRouter::new());

    // PR-α-1 (VP-111): World 階層 Stand を 1 instance ずつ生成して、 AppState 既存 field と
    // WorldCapabilities container の両方に share させる。 二重生成すると
    // whitesnake DB connection が並走する。
    //
    // PR-α-2 (VP-112): MidiCapability を World 階層に移管。 feature = "midi" 有効時は
    // `with_midi` で host 化、 無効時は `new` で空 placeholder のまま。
    //
    // PR-α-4 (VP-114): `vp daemon start --midi <arg>` で構築された MidiConfig を受け取り、
    // None なら `MidiConfig::default()` (= PR-α-2/3 後の既存挙動と同じ port auto-pick) で fallback。
    // VP-165 (doc 17 決定B): World daemon の Whitesnake は固定キー `discs/world/`
    // （旧 `file_backed_for_port(32000)` は world_port も override 可能なので port-keyed をやめた）。
    let world_whitesnake = crate::capability::Whitesnake::file_backed_for_world();
    let world_capabilities = {
        #[cfg(feature = "midi")]
        {
            let resolved_midi_config = midi_config.unwrap_or_default();
            Arc::new(
                crate::daemon::world_capabilities::WorldCapabilities::with_midi(
                    world_cap.clone(),
                    update_cap.clone(),
                    world_whitesnake.clone(),
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
                world_whitesnake.clone(),
            ))
        }
    };

    // Phase A ① / R1: World モードでも wiremsg store を build (= 将来 World 階層 actor 用)。
    // R1 で `WiremsgStore::new` は async (起動時に local_seq 採番を math::max で復元)。
    let wiremsg_store = match vpdb.as_ref() {
        Some(db) => Some(
            crate::capability::WiremsgStore::new(std::sync::Arc::new(db.inner().clone())).await?,
        ),
        None => None,
    };

    // Create minimal state for world mode
    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(SessionManager::new())),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode: DebugMode::None,
        shutdown_token: shutdown_token.clone(),
        project_dir: String::new(),
        // R3: World mode は cross-process forward の対象外 (= 自 project を持たない)
        project_name: String::new(),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities: Arc::new(
            ProcessCapabilities::new(CapabilityConfig {
                project_dir: String::new(),
                whitesnake: None, // World モードは永続 msgbox 不要
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
        tmux: Arc::new(tokio::sync::Mutex::new(None)),
        tmux_session_name: String::new(),
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
        // TheWorld もポート別ディレクトリで分離
        whitesnake: world_whitesnake,
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
        .route(
            "/api/world/projects/reload",
            post(world::world_reload_projects),
        )
        .route("/api/world/processes", get(world::world_list_processes))
        .route("/api/world/lanes", get(world::world_list_lanes))
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
    // Phase 1b: lane_registry も共有 (SP register の lanes payload を cache する)
    let lane_registry_ref = world_cap.read().await.lane_registry_ref();
    let daemon_state = std::sync::Arc::new(
        crate::daemon::server::DaemonState::new().with_running_processes(
            running_processes_ref,
            projects_ref,
            lane_registry_ref,
        ),
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

    // 起動時設定の復帰: enabled な project の SP を自動起動（VP-207）。
    // daemon restart 後に working set を復元する。1 回限りの startup タスク。
    let _autostart = tokio::spawn(ProcessManagerCapability::autostart_enabled_projects(
        world_cap.clone(),
    ));

    // VP-129 MVP: lane root FSEvents watcher 起動。 user の Finder / `rm -rf` で wing dir
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

    // VP-148 PR-P3-1: mDNS で `_vp._tcp.local.` を announce、 同 LAN 上の他 VP world に可視化。
    // instance_name は hostname 由来で衝突回避、 port は World API port。 pubkey は P3-4 で
    // Ed25519 fingerprint に置換、 現状は placeholder。 戻り値の MdnsAnnouncer は serve 終了
    // (= graceful shutdown) で scope exit、 Drop で自動 deregister + daemon shutdown。
    // VP-154 PR-1: World announce は `AnnounceKind::World` で `world-{localhost}` instance に。
    // VP-153 Layer 2 (= 過去 announce stale entry との self collision) は instance prefix で
    // OS LocalHostName と異なる namespace になり 自然解消。
    //
    // Moody Blues fix #1 (Score 82): announce() は内部で sync `scutil` shell-out、
    // `spawn_blocking` で wrap して tokio worker thread 占有を回避。
    //
    // VP-154 PR-3.5: config の advertise_hostname を読んで instance_name 安定化。
    // OS LocalHostName auto-increment 由来の identity 揺れを config 固定で吸収。
    let identity_override_for_world = crate::config::Config::load()
        .ok()
        .and_then(|c| c.network.advertise_hostname);
    let _mdns_announcer = match tokio::task::spawn_blocking(move || {
        crate::lan_discovery::announce(
            crate::lan_discovery::AnnounceKind::World,
            port,
            crate::lan_discovery::PUBKEY_PLACEHOLDER,
            identity_override_for_world.as_deref(),
        )
    })
    .await
    {
        Ok(Ok(a)) => Some(a),
        Ok(Err(e)) => {
            tracing::warn!(
                "mDNS announce 失敗 (LAN discovery 不能、 World 起動継続): {}",
                e
            );
            None
        }
        Err(e) => {
            tracing::warn!("mDNS announce spawn_blocking join 失敗 (World): {}", e);
            None
        }
    };

    // VP-149: daemon 起動時 1-shot LAN discover + AddressBook auto-populate (best-effort)
    // 失敗は warn のみ (= LAN 探索不能でも World 起動継続)。
    {
        match tokio::task::spawn_blocking(|| crate::lan_discovery::discover(3000)).await {
            Ok(Ok(worlds)) => {
                if !worlds.is_empty() {
                    let mut book = match crate::commands::lan::AddressBook::load() {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!("AddressBook load 失敗 (auto-populate skip): {}", e);
                            crate::commands::lan::AddressBook::default()
                        }
                    };
                    for w in &worlds {
                        // self を含める (= mdns broadcast に self も resolve される) は
                        // alias collision で last-write-wins、 user 視点で実害なし。
                        book.auto_upsert_from_discovered(w);
                    }
                    if let Err(e) = book.save() {
                        tracing::warn!("AddressBook auto-populate save 失敗: {}", e);
                    } else {
                        tracing::info!(
                            "AddressBook auto-populate: {} world(s) (1-shot discover)",
                            worlds.len()
                        );
                    }
                }
            }
            Ok(Err(e)) => tracing::warn!("LAN 1-shot discover 失敗: {}", e),
            Err(e) => tracing::warn!("LAN 1-shot discover join 失敗: {}", e),
        }
    }

    // VP-149: continuous mDNS browse 起動 (= ServiceResolved / ServiceRemoved を listen)
    // tokio::sync::mpsc で event を bg task に流し、 AddressBook を reactive に upsert/remove。
    // ContinuousBrowser は scope 内 (run_world 関数内) で keep alive、 graceful shutdown 後の
    // scope exit で Drop → mDNS daemon shutdown → bg thread 終了。
    let (lan_event_tx, mut lan_event_rx) = tokio::sync::mpsc::channel(64);
    let _lan_browser = match crate::lan_discovery::start_continuous_browse(lan_event_tx) {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!("mDNS continuous browse 起動失敗 (auto-add 無効化): {}", e);
            None
        }
    };
    let lan_event_shutdown = shutdown_token.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = lan_event_shutdown.cancelled() => {
                    tracing::debug!("LAN event listener: shutdown");
                    break;
                }
                ev = lan_event_rx.recv() => {
                    match ev {
                        Some(crate::lan_discovery::LanEvent::Discovered(world)) => {
                            // VP-149 Moody Blues fix #1 (Score 82): spawn_blocking を `.await` で
                            // serialize し、 disk I/O 中に他 event を fire しない (= file write race
                            // 解消、 partial TOML を find_by_host 等が読む risk 排除)。 同時に Issue #2
                            // (shutdown 後 inflight write) も event loop break で fire 停止して緩和。
                            let _ = tokio::task::spawn_blocking(move || {
                                let mut book = match crate::commands::lan::AddressBook::load() {
                                    Ok(b) => b,
                                    Err(e) => {
                                        tracing::warn!(
                                            "AddressBook load 失敗 (auto-upsert skip): {}",
                                            e
                                        );
                                        return;
                                    }
                                };
                                book.auto_upsert_from_discovered(&world);
                                if let Err(e) = book.save() {
                                    tracing::warn!(
                                        "AddressBook auto-upsert save 失敗: {}",
                                        e
                                    );
                                }
                            })
                            .await;
                        }
                        Some(crate::lan_discovery::LanEvent::Removed { instance_name }) => {
                            // VP-149 Moody Blues fix #1: 同じく `.await` で serialize
                            let _ = tokio::task::spawn_blocking(move || {
                                let mut book = match crate::commands::lan::AddressBook::load() {
                                    Ok(b) => b,
                                    Err(e) => {
                                        tracing::warn!(
                                            "AddressBook load 失敗 (auto-remove skip): {}",
                                            e
                                        );
                                        return;
                                    }
                                };
                                let removed = book.auto_remove_by_instance_name(&instance_name);
                                if removed > 0 {
                                    if let Err(e) = book.save() {
                                        tracing::warn!(
                                            "AddressBook auto-remove save 失敗: {}",
                                            e
                                        );
                                    } else {
                                        tracing::info!(
                                            "AddressBook auto-remove: instance={} removed={}",
                                            instance_name,
                                            removed
                                        );
                                    }
                                }
                            })
                            .await;
                        }
                        None => {
                            tracing::debug!("LAN event listener: channel closed");
                            break;
                        }
                    }
                }
            }
        }
    });

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("World graceful shutdown initiated");
        })
        .await?;

    // VP-148 PR-P3-1: graceful shutdown 後、 _mdns_announcer が drop されて deregister。
    // explicit drop なしでも OK だが、 順序を明示するため log だけ出す。
    tracing::debug!("mDNS announcer dropping (deregister via Drop trait)");

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
