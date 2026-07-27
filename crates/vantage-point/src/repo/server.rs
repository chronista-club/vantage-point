//! HTTP server with WebSocket support
//!
//! Process サーバーのエントリーポイント。`run()` と `run_daemon()` でサーバーを起動する。
//! ルートハンドラーは `routes/` モジュールに分離されている。

use std::net::{Ipv6Addr, SocketAddrV6};
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::routing::{get, post};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use super::capabilities::{CapabilityConfig, RepoCapabilities};
use super::hub::Hub;
use super::routes::{health, update};
use super::state::AppState;
use super::topic_router::TopicRouter;
use crate::capability::{RepoManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;

/// daemon が持つ「repo path_key → lane 一覧」集約 view の共有参照。
///
/// doc 44 P1 (fold-in): 旧構成では repo が QUIC "lanes" channel 越しにこの view へ
/// register snapshot を push していた。同一プロセスになった今、その push は
/// **map への書き込み**に退化する（[`publish_lanes`]）。
pub(crate) type NodeLaneView =
    Arc<RwLock<std::collections::HashMap<String, Vec<super::lanes_state::LaneInfo>>>>;

/// LanePool の現 snapshot を「daemon の集約 view」と「repo の hub」の両方へ配る。
///
/// doc 44 P1 (fold-in): lanes が daemon へ流れる供給点は 3 つ（起動時 seed / 5s periodic /
/// `SystemEvent::Lane`）あり、`lanes_state.rs` の規約どおり**全供給点で同じ enrich を通す**
/// 必要がある。旧構成ではこの 3 点が hub へ broadcast し、repo の uplink が QUIC で daemon の
/// `lane_registry` へ中継していた。fold-in で中継が消えたため、daemon 側 view の更新を
/// ここに並置する — これを怠ると daemon の view が boot 時の db 値で固まり、
/// Unison `lanes/list` が実在しない lane（過去 pid）を配り続ける。
/// vp-app への push を起こす通知路（daemon の `lane_change_tx`）と、
/// 前回 publish した内容の指紋。
///
/// doc 44 §11: fold-in で切れた「更新したら起こす」辺を戻すために publish 側が持つ。
/// 指紋は **同じ内容で起こさない**ため（5s tick がそのまま 5s ごとの全 snapshot push に
/// なるのを防ぐ）。
pub(crate) struct LaneChangeNotifier {
    tx: Option<tokio::sync::broadcast::Sender<String>>,
    last: Option<String>,
}

impl LaneChangeNotifier {
    pub(crate) fn new(tx: Option<tokio::sync::broadcast::Sender<String>>) -> Self {
        Self { tx, last: None }
    }

    /// 内容が前回と変わっていれば起床通知を撃つ。戻り値は撃ったかどうか（test 用）。
    ///
    /// 指紋は「vp-app に届く値そのもの」（lanes + origin）から取る。ここを snapshot の
    /// 一部だけにすると、**見えている値が変わったのに起こさない**穴ができる。
    fn notify_if_changed(&mut self, path_key: &str, fingerprint: String) -> bool {
        if self.last.as_deref() == Some(fingerprint.as_str()) {
            return false;
        }
        self.last = Some(fingerprint);
        match &self.tx {
            // receiver 不在（vp-app 未接続）の SendError は無害
            Some(tx) => tx.send(path_key.to_string()).is_ok(),
            None => false,
        }
    }
}

async fn publish_lanes(
    state: &Arc<AppState>,
    hub: &Hub,
    node_lanes: &Option<NodeLaneView>,
    path_key: &str,
    notifier: &mut LaneChangeNotifier,
) {
    let lanes = super::routes::lanes::build_lanes_snapshot(state).await;
    if let Some(view) = node_lanes {
        view.write()
            .await
            .insert(path_key.to_string(), lanes.clone());
    }
    // doc 44 D4: 開発起点を帳簿から解決して snapshot に添える（lane の属性ではなく
    // repo の指定なので `LaneInfo` には入れない）。
    let origin =
        crate::host::ledger::origin_name_for_lanes(state.vpdb.as_ref(), &state.repo_dir, &lanes)
            .await;
    let msg = crate::protocol::RepoMessage::LanesSnapshot {
        lanes,
        origin: Some(origin),
    };
    // doc 44 §11: daemon の "lanes" channel は `lane_change_tx` でしか再 push しない。
    // fold-in 前は repo の uplink（register / lanes-diff）がこの辺を担っていたが、
    // 中継が消えた際に **view の更新だけ移管され、起床通知が移管されなかった**。
    // 結果 vp-app の sidebar は wire 活動（hook）がある間しか新鮮でなかった。
    let fingerprint = serde_json::to_string(&msg).unwrap_or_default();
    notifier.notify_if_changed(path_key, fingerprint);
    hub.broadcast(msg);
}

/// repo 1 件分の実行状態を in-process で起動する（旧 SP プロセスの中身）。
///
/// doc 44 P1 (fold-in): 旧 `run()` から **uplink と終端 block を除いた部分**を切り出したもの。
/// repo プロセスとして動く間は [`run`] が本関数を呼んで uplink を張り、daemon 一枚化後は
/// daemon が repo ごとに本関数を直接呼んで `Arc<AppState>` を map に抱える。
///
/// `node_lanes` は daemon の lane 集約 view（repo プロセスとして動く場合は `None`）。
/// 旧 SP uplink の代わりに、本関数が起こす publish task が直接ここへ書き込む。
///
/// `vpdb` は daemon が開いた**唯一の DB handle**（doc 44 P1 PR4）。旧構成では本関数が
/// repo ごとに `db/sp_{slug}/` を開いていたが、同一プロセスになった今は handle を
/// 共有する。`None` は「DB なしで継続」（daemon の接続が失敗した場合）。
///
/// 返る時点で lane bootstrap / lifecycle monitor / lanes snapshot publish まで起動済み。
/// 停止は `shutdown_token` を cancel して [`shutdown_repo`] を呼ぶ。
pub(crate) async fn start_repo(
    port: u16,
    cap_config: CapabilityConfig,
    shutdown_token: CancellationToken,
    node_lanes: Option<NodeLaneView>,
    vpdb: Option<crate::db::SharedVpDb>,
    // doc 44 §11: vp-app への push を起こす通知路（daemon の `lane_change_tx`）。
    // `None` は Daemon 以外の文脈（test / 単体起動）で、その場合 push 先が居ない。
    lane_change_tx: Option<tokio::sync::broadcast::Sender<String>>,
    // boot 窓の根治: 先行 subscribe（daemon の canvas channel）が作った placeholder router。
    // Some ならそれを本 repo の topic_router として採用する（既存購読者ごと実 router 化。
    // demand hook は placeholder 生成時に登録済みなので二重登録しない）。
    adopted_router: Option<Arc<TopicRouter>>,
) -> Result<Arc<AppState>> {
    let repo_dir = cap_config.repo_dir.clone();
    let config_for_init = crate::config::Config::load().unwrap_or_default();

    // 旧 file-backed 永続化レイヤー退役: 永続は SurrealDB 一本化 (board pane state は pane_contents)。

    // repo_name は repo_dir から解決（AppState / lane pool 等で使用）
    let repo_name_for_remote =
        crate::resolve::repo_name_from_path(&repo_dir, &config_for_init).to_string();

    // rustls 0.23+ は CryptoProvider の明示的な設定が必要
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // トレースログファイルを早期初期化
    crate::trace_log::init_log_file();

    // Initialize Capability system
    let capabilities = Arc::new(RepoCapabilities::new(cap_config).await);

    // Initialize all capabilities
    if let Err(e) = capabilities.initialize().await {
        tracing::warn!("Failed to initialize capabilities: {}", e);
    }

    // wiremsg R5-4: 旧 msgbox の registry サブシステム (daemon registry への actor
    // register / unregister) は撤去済。 wire の cross-process delivery は daemon の
    // repo registry (repo → repo port) を使う別経路で、 msgbox registry には依存しない。

    let hub = Hub::new();

    // Start event bridge: EventBus -> Hub（shutdown token で停止可能）
    let _event_bridge = capabilities.start_event_bridge(hub.sender(), shutdown_token.clone());
    tracing::info!("Capability event bridge started");

    // Terminal チャネル認証トークンを生成
    let terminal_token = crate::discovery::generate_terminal_token();

    // TopicRouter 初期化 + Hub → TopicRouter ブリッジ（shutdown token で停止可能）。
    // 養子縁組（adopted_router = Some）の場合は購読者付きの placeholder をそのまま使う
    let topic_router = adopted_router.unwrap_or_else(|| Arc::new(TopicRouter::new()));
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

    // SurrealDB — daemon が開いた唯一の handle をそのまま使う（doc 44 P1 PR4）。
    //
    // 旧構成では本関数が repo ごとに `db/sp_{slug}/` を開いていた。ディレクトリ分離は
    // VP-182 の対処で、別プロセスの daemon と repo が同一 db を open すると surrealkv の OS 排他
    // ロックで 2 番目が失敗するためだった。fold-in で repo プロセスが消えた今、この分離は
    // 不要になっただけでなく害がある — repo ごとに db handle を持つと「repo の runtime
    // 実体」が復活し、doc 44 D2（repo は認知境界に退化する）と矛盾する。
    //
    // repo 次元は table の `repo_path` 列が持つ（repo 固有 table も元から全て持っており、
    // クエリも全て `WHERE repo_path = $path` で絞っている）ので、handle 共有で意味論は変わらない。
    // スキーマ定義は daemon 側の接続時に済んでいるため、ここでは行わない。
    //
    // 旧経路が担っていた「LOCK 保持 = 同 repo の repo が既に稼働中 → 重複 spawn 中止」は
    // `RepoRuntimes` の map への二重 insert 防止が引き継いだ（プロセスが無いので、
    // 重複は HashMap のキー衝突として表現される）。

    // VP-159 PR-4b: Agent / Service actor の supervisor 受け皿。 repo-local Service (= lane-spawn)
    // を `spawn_service` 経由で起動・register、 JoinHandle を保持。 machine scope の
    // device registry の metadata register は dynamic routing vision 確定後 (cf. design-spark
    // mem_1CavFi5D1aMSpEkas89SvQ)、 PR-5 supervisor 統一で JoinHandle 経由 abort を activate。
    let actor_registry = crate::capability::ActorRegistry::new();

    let state = Arc::new(AppState {
        hub,
        shutdown_token: shutdown_token.clone(),
        // Phase A4-2b: lane_pool init で同 var を後続参照するため clone
        repo_dir: repo_dir.clone(),
        capabilities,
        // R3: wire cross-process delivery の宛先分類用 — 解決済 repo 名
        repo_name: repo_name_for_remote.clone(),
        // VP-159 PR-4b: ActorRegistry を move (= lane-spawn は AppState 構築後に追加)
        actor_registry: Arc::new(RwLock::new(actor_registry)),
        daemon: None,
        update: None,
        // repo mode は hub federation を持たない（daemon のみ）→ Disabled / 空のまま。
        hub_status: crate::daemon::hub_client::HubFederationStatus::new(),
        hub_nodes: crate::daemon::hub_client::HubNodesCache::new(),
        interactive_agent: Arc::new(RwLock::new(None)),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: terminal_token.clone(),
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::repo::process_runner::ProcessRegistry::new(),
        )),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb: vpdb.clone(),
        // wiremsg R2-a: repo は wire store を持たない (daemon に中央化、 handler は proxy)
        wiremsg_store: None,
        // wire_notifier / delivery_notify は daemon mode 専用 (daemon の long-poll 起床 /
        // delivery loop wake)。 repo では未使用だが AppState 共有 field のため空で満たす
        wire_notifier: crate::capability::WireNotifier::new(),
        delivery_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        // Phase A4-2b: Lane scope の Agent pool — Conductor Lane 1 つ pre-populate
        // memory rule: 多 scope architecture (App/Repo/Lane/Pane)、HD/TH は Lane scope。
        // Performer Lane の動的 create は A4-4、Agent spawn 連動は A5 で実装。
        //
        // (I-b、 2026-04-30): Performer auto-spawn は AppState 構築後に Mailbox actor 経由で実施。
        // lane performers を `LaneCmd::SpawnLane` Cmd 化して `lane-spawn` mailbox に投入する
        // (= concurrency 制御を `Arc<Semaphore::new(N)>` で表現、 N=config.startup.max_concurrent_lane_spawn)。
        // 詳細は run() 内 lane_spawn_actor wiring 参照。
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::with_root(
            repo_name_for_remote.clone(),
            repo_dir.clone(),
        ))),
        // Phase 2 (Step E): system 系 lifecycle event の central broadcast bus。
        // capacity 64 = lifecycle 変更が短時間に集中しても drop しない buffer。
        // caller publish (SystemEvent::Lane(LaneDiff::*) 等) + `publish_lanes` subscribe で
        // daemon の集約 view を更新する経路。 将来 Pane / Agent 等の event も同 bus に variant
        // 追加で乗る。
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        // Phase A4-2b: Repo scope の Agent pool (board/runner ほか) — skeleton
        // PR-α-1 (VP-111): repo モードでは MachineCapabilities を持たない (daemon mode 専用)
        machine_capabilities: None,
        // PR-β-1 (VP-119): repo モードで LaneCapabilities pool 受け皿を Some で初期化。
        // 物理移管 (board) は PR-β-2、 本 PR では空 HashMap で構築のみ。
        lane_capabilities: Some(Arc::new(RwLock::new(
            super::lane_capabilities::LaneCapabilitiesPool::new(),
        ))),
        terminal_pumps: Arc::new(RwLock::new(std::collections::HashMap::new())),
        // repo mode は delegation store を持たない (daemon 中央 store に proxy する)。
        delegation_store: None,
        editor_pending: Default::default(),
    });

    // Phase review fix #2: LanePool::with_root は内部で PtySlot::spawn (openpty + spawn_command)
    // で OS syscall ブロッキング → spawn_blocking で tokio worker thread (= tokio runtime の OS thread) を保護。
    // でも... AppState 既に構築済なので restructure したいけど不可。 代替:
    // with_root 自体は sync だが state 構築段階で `tokio::task::block_in_place` も使えない。
    // 結果的に repo 起動時 1 回だけの呼び出しなので影響は軽微。 review 指摘は記録、 現実装維持。
    // (`create_handler` 側の spawn_blocking 化は完了済 = lanes.rs の方が頻繁に呼ばれる重要 path)

    // ペイン状態をディスクから復元（前回 Process 終了時の状態 → RetainedStore）
    state.restore_pane_contents().await;

    // PR-β-2 (VP-120): Conductor Lane の LaneCapabilities entry を populate (LanePool::with_root と同期)。
    // PR-β-1 で空 HashMap だった lane_capabilities pool に、 Conductor Lane の独立 BoardState を host。
    // doc 13 §6 自動 spawn rule = Lane 起動時に board 同時 spawn (default) を default で実現。
    if let Some(lc_pool) = state.lane_capabilities.as_ref() {
        let conductor_addr = super::lanes_state::LaneAddress::root(&repo_name_for_remote);
        let default_agent = crate::config::Config::load()
            .unwrap_or_default()
            .default_agent_or_claude()
            .to_string();
        lc_pool
            .write()
            .await
            .populate_lane(conductor_addr, default_agent);
        tracing::info!(
            "PR-β-2: LaneCapabilities pool に Conductor Lane populate (repo={}, board host 化)",
            repo_name_for_remote
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
    // in-process 直結 (2026-07-09): 旧 wiremsg R2-a 経路 (daemon 中央 wire store の
    // `lane-spawn@<repo>` mailbox 往復) を撤去。 producer は本 bootstrap のみで、 at-most-once
    // 配送 + repo 再起動時の幽霊 long-poll 消費で Cmd が失われ performer が永久 Spawning になる
    // 障害があった (詳細は lane_spawn_actor.rs module doc)。 channel は process-local なので
    // この failure mode が構造的に消滅し、 daemon 不達 retry も不要 (standalone repo でも spawn 可)。
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
                state.terminal_pumps.clone(),    // doc 53 R2: 復元後 pump reconcile 用
                state.topic_router.clone(),
                max_concurrent,
                lane_spawn_rx,
            ),
            shutdown_token.clone(),
        );

        let performers_repo_id = std::path::Path::new(&state.repo_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        // repo-local lane refactor PR 1: list_performers_for_repo は repo_root: &Path を受け取る。
        let performers =
            crate::lane::commands::list_performers_for_repo(std::path::Path::new(&state.repo_dir));
        let total = performers.len();
        if total > 0 {
            tracing::info!(
                "repo startup bootstrap: {} 本の Performer SpawnLane Cmd を投入 (repo_id={}, max_concurrent={})",
                total,
                performers_repo_id,
                max_concurrent
            );
            // doc 11 PR-B: agent は String 化、 default は config の `default_agent`
            // (未設定なら "claude" fallback、 PR-pre2 / VP-118 で "hd" → "claude")。
            let default_agent = crate::config::Config::load()
                .unwrap_or_default()
                .default_agent_or_claude()
                .to_string();
            // in-process 直結: 型付き LaneCmd を channel に同期 send (serialize / retry 不要)。
            // send が Err を返すのは receiver drop 後のみ (= actor task 終了後。 startup 時点では
            // 起き得ないが防御的に warn)。 投入順序は Semaphore gate が並列度を制御するため保証不要。
            for entry in &performers {
                // per-lane agent 永続 (mem_1Cd4M7i5Enp3HHMLVYayRe): create 時に記録された agent で
                // respawn する。記録不在 (旧 lane / 手動 `vp lane new`) は従来どおり default。
                // これが無いと非 conversation performer (codex/grok) が repo 再起動で conversation に化ける
                // (agent 非永続の既知バグの根治)。
                let agent = crate::lane::agent_store::last(&performers_repo_id, &entry.name)
                    .unwrap_or_else(|| default_agent.clone());
                let cmd = super::lane_cmd::LaneCmd::SpawnLane {
                    repo_id: performers_repo_id.clone(),
                    name: entry.name.clone(),
                    cwd: entry.path.clone(),
                    agent,
                };
                if lane_spawn_tx.send(cmd).is_err() {
                    tracing::warn!(
                        "repo startup bootstrap: lane-spawn channel closed (actor 未起動?) name={}",
                        entry.name
                    );
                }
            }
        } else {
            tracing::info!(
                "repo startup bootstrap: lane performers なし (repo_id={})",
                performers_repo_id
            );
        }

        // doc 53 §12: **conductor lane の実体はここで立つ**（`with_root` は登録だけ）。
        //
        // reconcile が registry に従って mode=Tui の全 session に slot を立て、末尾で pump も
        // 合わせる（R2）。旧実装は ①`with_root` が root を spawn ②`restore_term_slots` が
        // 非 root を spawn ③ここで pump だけ reconcile、の 3 段で、①② が **AppState 構築中の
        // sync 文脈**（server.rs 自身が「restructure したいが不可」と書いていた場所）だった。
        //
        // pump 側の事情も引き続き満たす: router を養子縁組した場合（repo 起動前から GUI が
        // 購読 = demand count ごと引き継ぎ）は 0→1 edge が来ないので、level 読みの reconcile が
        // 要る。demand 不在なら no-op。（performer 側の同じ契機は lane_spawn_actor の末尾）
        //
        // address の repo 名は with_root と同じ解決済の名（`state.repo_name`）を使う —
        // `performers_repo_id`（dir 名）は登録名と異なり得る。
        let conductor_addr = super::lanes_state::LaneAddress::root(&state.repo_name);
        super::lane_reconcile::reconcile_lane(
            &state.lane_pool,
            &state.terminal_pumps,
            &state.topic_router,
            &conductor_addr,
        )
        .await;
    }

    // doc 44 P1 (fold-in): repo は listener も outbound 接続も持たない。
    //
    // 経緯: L0 finale (doc 27 §3.4.5) で repo は HTTP/QUIC listen を全廃し、daemon からの操作は
    // 「Daemon → repo control channel の reverse-routing」で serve していた。fold-in はその
    // control channel ごと不要にした — repo は daemon と同一プロセスなので、process 操作は
    // `RepoRuntimes::dispatch` → `dispatch_repo_method` の**直呼び**になる。
    //
    // 旧経路を構成していた `spawn_daemon_uplink` / `run_control_driver` / "control" channel は
    // いずれも撤去済（残っていた 451 行は `run()` を外した時点で孤児化してコンパイラが検出した）。
    // health/shutdown handler は run_daemon (Daemon) が使うため routes/health.rs に残置。

    // wiremsg Stage 0: Lane lifecycle event を retained topic に publish する。
    // `SystemEvent::Lane` を購読し、LanePool の全 list snapshot を
    // `repo/runtime/state/lanes`（category=state → RetainedStore で保持）へ流す。
    // hub.broadcast → Hub→TopicRouter pump → retained。consumer（Stage 1 で vp-app が
    // subscribe）不在でも no-op で安全。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
    {
        let mut sys_rx = state.system_event_tx.subscribe();
        let state_for_pub = state.clone();
        let hub = state.hub.clone();
        let shutdown = shutdown_token.clone();
        // doc 44 P1 (fold-in): 3 つの供給点はいずれも `publish_lanes` を通す
        // （daemon の集約 view 更新と hub broadcast が常に対で起きることを型で担保する）。
        let daemon_lanes_for_pub = node_lanes.clone();
        let path_key_for_pub =
            crate::capability::normalize_path_key(std::path::Path::new(&repo_dir));
        // doc 44 §11: 供給点は 3 つとも同じ notifier を通す（指紋が 1 本でないと
        // 「別の供給点が publish した直後は起こさない」等の取りこぼしが出る）。
        let mut notifier = LaneChangeNotifier::new(lane_change_tx);
        // 起動直後の現 snapshot を 1 度 publish して retained を seed する
        // （Conductor Lane は既に pre-populate 済）。
        // repo-local lane refactor PR 1: build_lanes_snapshot で disk-scan Inactive Performer
        // も含める (= HTTP /api/lanes と同一 logic、 sidebar QUIC 経路でも Inactive 表示)。
        publish_lanes(
            &state_for_pub,
            &hub,
            &daemon_lanes_for_pub,
            &path_key_for_pub,
            &mut notifier,
        )
        .await;
        // board モデル (2026-07-15): DB の全 board を起動直後に retained topic へ seed する。
        // webview が canvas channel を購読した瞬間、 BoardUpdated(retained) で全 board が初期配信される
        // （repo 再起動を越えて board が復元される。 別 load 経路は不要）。
        super::unison_server::seed_boards(&state).await;
        tokio::spawn(async move {
            use super::lanes_state::SystemEvent;
            use tokio::sync::broadcast::error::RecvError;
            // repo-local lane refactor PR 1: CLI `vp lane new` は SystemEvent::Lane を
            // fire しない (= 直 fs op、 repo 経由しない)。 disk-only performer を sidebar に届ける
            // safety net として 5s periodic tick で snapshot 再 publish する。
            // (FSEvents-based lane watcher の repo-local 拡張は後 PR の範囲)
            let mut periodic = tokio::time::interval(std::time::Duration::from_secs(5));
            periodic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = periodic.tick() => {
                        publish_lanes(
                            &state_for_pub, &hub, &daemon_lanes_for_pub, &path_key_for_pub,
                            &mut notifier,
                        ).await;
                    }
                    ev = sys_rx.recv() => match ev {
                        // Lane lifecycle 変化 / 並び替え / lag → 現 snapshot を全量 publish（idempotent）。
                        // 帳簿由来の投影変化（並び順 / 開発起点）は per-lane の diff を持たないが
                        // snapshot の見え方が変わるので、同じ全量 publish で届く。
                        Ok(SystemEvent::Lane(_) | SystemEvent::LanesProjectionChanged)
                        | Err(RecvError::Lagged(_)) => {
                            publish_lanes(
                                &state_for_pub, &hub, &daemon_lanes_for_pub, &path_key_for_pub,
                                &mut notifier,
                            ).await;
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

    Ok(state)
}

/// [`start_repo`] で起動した repo の後始末（file watcher 停止 + capability shutdown）。
///
/// shutdown_token を cancel した**後**に呼ぶこと（token cancel は spawn 済 task の停止、
/// 本関数は token では止まらないリソースの解放を担当する）。
pub(crate) async fn shutdown_repo(state: &Arc<AppState>) {
    // pane 状態は webview が board state ask（repo-proxy）で逐次 pane_contents に保存済 (旧 DISC
    // shutdown snapshot は退役)。 shutdown 時の明示保存は不要。

    // ファイル監視を全停止
    state.file_watchers.lock().await.stop_all();

    // (tmux decoupling PR2: lane は PtySlot の子 — 親が落ちれば完全に落ちる)

    tracing::info!("Shutting down capabilities...");
    if let Err(e) = state.capabilities.shutdown().await {
        tracing::warn!("Error during capability shutdown: {}", e);
    }
}

/// daemon の HTTP router を組む（= 残っている HTTP 面の全て）。
///
/// ## doc 45 段 4 — control plane の HTTP route は撤去済み
///
/// control plane（repos CRUD / lifecycle / lanes / canvas）は Unison "daemon-control" channel
/// に一本化した（doc 45 §3、消費者は段 2 で CLI・段 3 で vp-app が移設済み）。ここに残るのは:
///
/// - `/api/health` `/api/shutdown` — **意図的に鈍い外殻**（doc 45 §2）。health は
///   「他が壊れている時に動いてほしい」probe で、Unison 層が wedge した時に診断手段ごと
///   失わないよう HTTP に置く。`.mise/tasks/app/swap`（Ruby）と `apple/VantagePointAgent`
///   （Swift）という **VP 外の消費者**もいて、彼らに Unison client を持たせる理由がない。
/// - `/api/update/*` — self-update（doc 45 §3 で「churn が低いので後回しでよい」と判断）。
///
/// `run_daemon` から関数として切り出してあるのは、**route 登録そのものをテストで固定する**ため
/// （撤去の巻き添えで health / shutdown を落とすと、診断手段と緊急停止を同時に失う）。
fn build_daemon_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        // L0 portless: `/ws/lanes` (repo_feed WS) は consumer 消滅で dead のため撤去。
        // doc 45 段 4: `/api/canvas/{switch_lane,layout}` は撤去。宛先の `canvas_senders` を
        // populate する書き手が旧 localhost browser Canvas の WS 撤去で消えており、
        // end-to-end で dead だった（doc 45 §3.1 — 移設ではなく撤去が正解の例）。
        // doc 45 段 4: `/api/daemon/*`（repos CRUD / processes lifecycle / lanes）は撤去。
        // 同じ操作は Unison "daemon-control" channel が持ち、実装は
        // `routes::daemon` の共有関数（apply_repo_update / collect_lanes /
        // resolve_create_lane_args）に畳んであるので面が減っても振る舞いは変わらない。
        // L0 portless B-4 (wire-unison): 中央 wire/delegation store の HTTP 入口 (`/api/wire/*`
        // `/api/delegation/*`) は daemon の "wire" unison channel に移行 (doc 27 §62)。
        // `daemon_wire::call` が QUIC で叩き、 `handle_wire_channel` が `routes::{wire,delegation}::
        // dispatch_*` に振る。 観測 (`vp wire deleg-thread`) / pull-hook (`vp wire hook-check`) も
        // 同 channel 経由。
        // doc 44 P1 (fold-in): 旧「Process が自己登録する」HTTP register/unregister は撤去。
        // repo は Daemon 自身が起こすので外から登録される概念が無く、残しておくと
        // 外部由来の port/pid で running_repos を書ける穴になる（起動していない
        // repo を稼働中に見せられる）。稼働状態の唯一の writer は start/stop_process。
        // doc 44 P1 (fold-in): slot ベース port resolver (`/api/daemon/port_for`) と
        // slot 割当 route (set_slot / unassign_slot) は `vp port` 退役とともに撤去。
        // repo は portless（port=0）になり、slot が解決する listen port が存在しない。
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
        .with_state(state)
}

/// daemon モードで Process サーバーを起動
/// 複数のProject Processを管理するための専用モード
/// Daemon（PTY管理 QUIC サーバー）も統合して起動する
pub async fn run_daemon(port: u16) -> Result<()> {
    use crate::capability::core::{Capability, CapabilityContext};
    use crate::daemon::process;

    // PID ファイルはポートバインド成功後に書き出す（下記参照）

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Initialize Daemon Capability
    let mut daemon_cap = RepoManagerCapability::new();
    let ctx = CapabilityContext::new();

    if let Err(e) = daemon_cap.initialize(&ctx).await {
        tracing::error!("Failed to initialize RepoManagerCapability: {}", e);
        return Err(anyhow::anyhow!(
            "RepoManagerCapability initialization failed: {}",
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
    // doc 44 P1 PR4 (DB 統合): ここで開く `db/machine/` が**唯一の DB**で、全 repo が
    // この handle を共有する（`RepoRuntimes::for_daemon` 経由で配る）。旧 VP-182 の
    // per-repo 分離 (`db/sp_{slug}/`) は撤去済 — repo 次元は table の repo_path 列が持つ。
    let vpdb: Option<crate::db::SharedVpDb> = {
        let data_dir = crate::db::db_data_dir_for_machine();
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

    // VpDb を RepoManagerCapability に注入し、DB からrepoを再読み込み
    // （initialize 時点では vpdb 未設定のため config.toml から読み込まれている。
    //   ここで DB マイグレーション + DB → HashMap 同期を実行する）
    if let Some(ref db) = vpdb {
        daemon_cap.set_vpdb(db.clone());
        if let Err(e) = daemon_cap.load_config().await {
            tracing::warn!("DB 付き config 再読み込み失敗: {}", e);
        }
    }

    // federation L2 (ADR-020 D2): home-node の位置独立 routing key `wld_id` を db/machine から
    // load_or_create する。daemon が初回起動で 1 度発行し、 以降の再起動は復元する (machine /
    // hostname / endpoint から独立な不変番地)。db 不在 (degraded) なら None — その場合は
    // federation の routing key を名乗れないが machine-local 動作は継続する (= hub down 時と同 degrade)。
    let node_id: Option<crate::node::NodeId> = if let Some(ref db) = vpdb {
        match db.load_or_create_node_id().await {
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

    let daemon_cap = Arc::new(RwLock::new(daemon_cap));
    let update_cap = Arc::new(RwLock::new(update_cap));
    let hub = Hub::new();

    // TopicRouter（Daemon モードでは Hub ブリッジ不要だが、AppState の必須フィールド）
    let topic_router = Arc::new(TopicRouter::new());

    // PR-α-1 (VP-111): machine 階層 Stand を 1 instance ずつ生成して、 AppState 既存 field と
    // MachineCapabilities container の両方に share させる (二重生成は避ける)。
    //
    // device 管理は DeviceRegistry 🧲 に一本化（feature = "midi" 時は `with_devices` で host 化）。
    // 旧 MidiCapability hosting（単一 port の無条件 grab）は退役 — 消費者不在のまま
    // enumeration 先頭 device（実機で LPD8）を掴み、DeviceRegistry listener を沈黙させていた。
    let machine_capabilities = {
        #[cfg(feature = "midi")]
        {
            Arc::new(
                crate::daemon::machine_capabilities::MachineCapabilities::with_devices(
                    daemon_cap.clone(),
                    update_cap.clone(),
                )
                .await,
            )
        }
        #[cfg(not(feature = "midi"))]
        {
            Arc::new(
                crate::daemon::machine_capabilities::MachineCapabilities::new(
                    daemon_cap.clone(),
                    update_cap.clone(),
                ),
            )
        }
    };

    // DeviceRegistry 🧲 — ROTO 持続セッションを Daemon lifecycle に enclose して起動する。
    // 前景 `vp midi roto control` のフル接続（open + handshake + keepalive + LCD/routing）を
    // daemon 常駐 + 自動再接続に昇格。lane data は daemon_cap(RepoManagerCapability) を
    // in-process 直読み、switch_lane は repo 越境なので QUIC。shutdown_token の子 token で
    // graceful 停止する。devices_for_shutdown は cleanup chain 用に Arc を clone しておく。
    #[cfg(feature = "midi")]
    let devices_for_shutdown = machine_capabilities.devices.clone();
    #[cfg(feature = "midi")]
    if let Some(devices) = machine_capabilities.devices.as_ref() {
        devices
            .write()
            .await
            .start_roto_control(daemon_cap.clone(), shutdown_token.clone())
            .await;
    }

    // Phase A ① / R1: Daemon モードでも wiremsg store を build (= 将来 machine 階層 actor 用)。
    // R1 で `WiremsgStore::new` は async (起動時に local_seq 採番を math::max で復元)。
    let wiremsg_store = match vpdb.as_ref() {
        Some(db) => Some(
            crate::capability::WiremsgStore::new(std::sync::Arc::new(db.inner().clone())).await?,
        ),
        None => None,
    };

    // 委譲 (delegation) の daemon 中央 store (doc 28 §4 / §6)。wire と同じく daemon の DB に持つ。
    let delegation_store = vpdb
        .as_ref()
        .map(|db| crate::capability::DelegationStore::new(std::sync::Arc::new(db.inner().clone())));

    // chronista-hub federation の接続状態。run_hub_federation（writer）と AppState（= /api/health
    // reader）で同一 instance を共有する（daemon mode のみ更新、初期 Disabled）。
    let hub_status = crate::daemon::hub_client::HubFederationStatus::new();
    // hub registry の available nodes cache も同 pattern で共有（writer = run_hub_federation の
    // 定期 discover、reader = /api/health の `hub_nodes` field。初期 = 空）。
    let hub_nodes = crate::daemon::hub_client::HubNodesCache::new();

    // Create minimal state for daemon mode
    let state = Arc::new(AppState {
        hub,
        shutdown_token: shutdown_token.clone(),
        hub_status: hub_status.clone(),
        hub_nodes: hub_nodes.clone(),
        repo_dir: String::new(),
        // R3: daemon mode は cross-process forward の対象外 (= 自 repo を持たない)
        repo_name: String::new(),
        capabilities: Arc::new(
            RepoCapabilities::new(CapabilityConfig {
                repo_dir: String::new(),
            })
            .await,
        ),
        // VP-159 PR-4b: daemon mode では空で構築 (= machine scope actor の register は後続 PR、
        // device registry の metadata register は dynamic routing vision 確定後)
        actor_registry: Arc::new(RwLock::new(crate::capability::ActorRegistry::new())),
        daemon: Some(daemon_cap.clone()),
        update: Some(update_cap.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "DAEMON_DISABLED".to_string(),
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::repo::process_runner::ProcessRegistry::new(),
        )),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb: vpdb.clone(), // Daemon モードでも DB 参照あり
        // Phase A ① / R1: Daemon モードでも wiremsg store を build (上で async build 済)
        wiremsg_store,
        wire_notifier: crate::capability::WireNotifier::new(),
        // R2-b: wire delivery loop の即時 wake (daemon_wire_send_handler が command 着信で notify)
        delivery_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
        // Phase A4-2b: Daemon モードでは Lane / Repo Stand を持たない (空 Pool で AppState を満たす)
        // 多 scope architecture: daemon は App scope の component、Lane/RepoStand は Repo scope
        lane_pool: Arc::new(RwLock::new(super::lanes_state::LanePool::new())),
        // Phase 2 (Step E): system event central bus
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        // PR-α-1 (VP-111): machine 階層 Agent container (LSCM doc 12 §3 / §9)
        machine_capabilities: Some(machine_capabilities),
        // PR-β-1 (VP-119): daemon mode では LaneCapabilities を持たない (Lane scope は repo per repo)
        lane_capabilities: None,
        // S2: daemon mode は repo の per-lane pump を持たない (terminal pump は repo scope)。
        terminal_pumps: Arc::new(RwLock::new(std::collections::HashMap::new())),
        // 委譲 (delegation) の daemon 中央 store (doc 28 §6)。daemon mode のみ Some。
        delegation_store,
        editor_pending: Default::default(),
    });

    // in-app update: GitHub Releases latest の定期チェック（起動時 + 24h 毎）で
    // UpdateCapability の cache を温める。/api/health がこの cache を読んで
    // `update_available` / `latest_version` を vp-app に露出する（handler 側は network なし）。
    // launchd daemon は shell env を持たない前提で reqwest 直（proxy 等に依存しない）。
    // チェック失敗は静かに無視（オフライン耐性 — 次の tick で再試行）。
    {
        let update_cap = update_cap.clone();
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    // interval の初回 tick は即時発火 = 起動時チェックを兼ねる
                    _ = tick.tick() => {
                        match update_cap.write().await.check_update().await {
                            Ok(r) if r.update_available => tracing::info!(
                                current = %r.current_version,
                                latest = %r.latest_version,
                                "update check: 新しい release が利用可能"
                            ),
                            Ok(r) => tracing::debug!(
                                current = %r.current_version,
                                "update check: 最新です"
                            ),
                            Err(e) => tracing::debug!("update check 失敗（無視して次回再試行）: {}", e),
                        }
                    }
                }
            }
        });
    }

    // tmux decoupling PR1: repo control channel registry を hoist する。 daemon server (下記
    // daemon_state) がこの map を populate し、 Daemon-side の nudge loop (delivery / reconcile) が
    // 同一 Arc を引いて `lane_nudge` を所有 repo に forward する。 別々に new() すると map が分裂して
    // forward 不能になるため、 ここで作った 1 つを 3 者 (daemon_state + 両 loop) に配る。
    // doc 44 P1 (fold-in): 旧「repo control channel registry」を per-repo 実行状態の
    // registry に置き換える。 repo プロセスが無くなったので、 daemon は repo を
    // `Arc<AppState>` として直接抱え、 forward ではなく in-process dispatch で操作する。
    // ここで作った 1 つを 3 者 (daemon_state + delivery loop + delegation reconcile loop) に
    // 配るのは旧構成と同じ (別々に new() すると map が分裂して到達不能になる)。
    // doc 44 P1 (fold-in): daemon の lane 集約 view を registry に結線する。旧構成では
    // repo の QUIC uplink がこの view を最新化していたので、結線を落とすと repo は
    // 動くのに daemon からは boot 時の db 値しか見えない（= 過去 pid の ghost lane 配信）。
    // doc 44 P1 PR4 (DB 統合): db handle も同時に配る。repo は自分では db を開かず、
    // daemon が開いたこの 1 本を共有する（repo 次元は table の repo_path 列が持つ）。
    // doc 44 §11: vp-app の "lanes" push を起こす通知路。DaemonState より**先に**作って
    // 両方へ配る — 生産者は repo 側の `publish_lanes`、消費者は daemon の push loop で、
    // DaemonState 任せにすると生産者に渡す手段が無い（`process_lifecycle_tx` を capability と
    // 共有しているのと同じ構図）。
    let (lane_change_tx, _) = tokio::sync::broadcast::channel::<String>(64);
    // canvas 集約 map は DaemonState と RepoRuntimes の**両方より先**に作って共有する
    // （boot 窓の根治: repo 起動が先行 subscribe の placeholder を養子縁組できるように）
    let canvas_routers: super::topic_router::CanvasRouters = Default::default();
    let control_channels: crate::daemon::server::ControlChannels =
        std::sync::Arc::new(super::repo_registry::RepoRuntimes::for_daemon(
            daemon_cap.read().await.lane_registry_ref(),
            vpdb.clone(),
            // fold-in で落ちた「view を更新したら vp-app を起こす」辺を戻す。view
            // (`lane_registry`) の更新と通知が同じ経路に載る（旧 SP uplink と同じ組）。
            lane_change_tx.clone(),
            canvas_routers.clone(),
        ));

    // RepoManagerCapability に registry を差し込む（`start_process` が in-process 起動に使う）。
    daemon_cap
        .write()
        .await
        .set_repo_runtimes(control_channels.clone());

    // R2-b: wire delivery loop (未 ack command の nudge + 再掲示) を spawn。
    // store 未構築 (DB 接続失敗) なら skip — wire 自体が動かないため delivery も不要。
    if let Some(store) = state.wiremsg_store.clone() {
        let lane_registry = daemon_cap.read().await.lane_registry_ref();
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
    // Daemon-side wake (lane_registry + repo-proxy lane_nudge) なので delivery loop と同じ
    // lane_registry / control_channels を使う。
    if let Some(store) = state.delegation_store.clone() {
        let lane_registry = daemon_cap.read().await.lane_registry_ref();
        super::delegation::spawn_reconcile_loop(
            store,
            lane_registry,
            control_channels.clone(),
            shutdown_token.clone(),
        );
    }

    // L0 portless B-4: state は後段の daemon_state_builder.with_wire でも参照するため clone
    // (Arc clone は安価、 router と daemon QUIC server が同一 AppState を共有)。
    let app = build_daemon_router(state.clone());

    // Phase 5-D: dual-stack listen (IPv4 + IPv6) ─ vp-app の `http://127.0.0.1:32000` ping、
    //  repo からの `http://[::1]:32000` register、 LAN IPv6 access の 3 経路を全部受け取れるように。
    let listener = bind_dual_stack(port).await?;
    tracing::info!(
        "{} 起動 http://[::]:{} (dual-stack)",
        crate::stands::DAEMON.display(),
        port
    );

    // ポートバインド成功後に PID ファイルを書き出す
    // （バインド前に書くと、失敗時に既存デーモンの PID が上書きされ制御不能になる）
    process::write_pid_file()?;

    // Clone for shutdown
    let daemon_for_shutdown = daemon_cap.clone();

    // Daemon QUIC サーバー起動（PTY セッション管理 + Registry チャネル、同一ポートで UDP/QUIC）
    // RepoManagerCapability の running_repos を DaemonState と共有
    let running_processes_ref = daemon_cap.read().await.running_processes_ref();
    let repos_ref = daemon_cap.read().await.repos_ref();
    // Phase 1b: lane_registry も共有 (repo register の lanes payload を cache する)
    let lane_registry_ref = daemon_cap.read().await.lane_registry_ref();
    // L1 lifecycle: process_presence も共有 (registry handler が presence を遷移させる)
    let process_presence_ref = daemon_cap.read().await.process_presence_ref();
    let mut daemon_state_builder = crate::daemon::server::DaemonState::new()
        .with_running_processes(
            running_processes_ref,
            repos_ref,
            lane_registry_ref,
            process_presence_ref,
        )
        // control plane 一元化: daemon_cap (= HTTP AppState.daemon と同一 Arc) を共有し、
        // Unison "daemon-control" channel から repos mutation を受けられるようにする。
        .with_daemon_cap(daemon_cap.clone())
        // tmux decoupling PR1: 上で hoist した control channel map を daemon server と共有する
        // (daemon が repo 接続で populate → nudge loop がここから forward 先を引く)。
        .with_control_channels(control_channels.clone())
        // boot 窓の根治: RepoRuntimes と同一の canvas map を共有（分裂すると養子縁組不能）
        .with_canvas_routers(canvas_routers.clone())
        // doc 44 §11: repo 側の publish が撃つのと**同一の** channel を daemon の
        // push loop に購読させる（別々に作ると生産者ゼロで永久沈黙する）。
        .with_lane_change_tx(lane_change_tx.clone());
    // doc 24 §10 Phase 2: lane descriptor の durable 永続先 (capability boot load と同一 db)。
    if let Some(ref db) = vpdb {
        daemon_state_builder = daemon_state_builder.with_vpdb(db.clone());
    }
    // L0 portless B-4 (wire-unison): daemon 中央 wire/delegation store を daemon QUIC server と共有する。
    // `state` (daemon process AppState) が保持する **同一 Arc** を渡す (同一プロセス) — "wire" channel が
    // これを使って旧 `/api/wire/*` `/api/delegation/*` HTTP を unison channel で serve する (doc 27 §62)。
    daemon_state_builder = daemon_state_builder.with_wire(
        state.wiremsg_store.clone(),
        state.wire_notifier.clone(),
        state.delivery_notify.clone(),
        state.delegation_store.clone(),
    );
    // DeviceRegistry 🧲 EventBus を共有 — daemon-device channel が device event を vp-app に bridge する。
    // machine_capabilities は L810 で move 済みなので、 move 前に clone した devices_for_shutdown を使う。
    #[cfg(feature = "midi")]
    if let Some(devices) = devices_for_shutdown.as_ref() {
        let event_bus = devices.read().await.event_bus().clone();
        daemon_state_builder = daemon_state_builder.with_devices_event_bus(event_bus);
        // M2 / doc 26 §2: device channel (agent → daemon) が registry を更新するため registry 本体も共有。
        daemon_state_builder = daemon_state_builder.with_devices(devices.clone());
    }
    // doc 44 P1 (fold-in): capability の start_process / stop_process が lifecycle event を
    // 流せるよう、DaemonState と**同一の** broadcast Sender を共有する（clone しても同じ
    // channel を指す）。これが無いと `vp daemon processes --watch` / event log の
    // process.up/down が生産者ゼロで永久沈黙する（旧 registry handler が担っていた経路）。
    daemon_cap
        .write()
        .await
        .set_process_lifecycle_tx(daemon_state_builder.process_lifecycle_tx.clone());

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
    // `hub_client::hub_addr()` が解決）が設定されていれば、この daemon を hub registry に register
    // （他 daemon から discover 可能に）し、**relay の target inbound を常駐で受ける**（ADR-020 §S4）。
    // 旧実装は起動時に register して即 drop する使い捨てだったが、relay 受信には接続維持が必要なため
    // 常駐セッション（[`run_hub_federation`]）へ昇格した（接続が切れたら自律再接続）。未設定なら
    // machine-local 動作（= skip）。SSOT 原則により hub と話すのは daemon のみ。
    if let Some(hub_addr) = crate::daemon::hub_client::hub_addr() {
        // handle = この machine の identity（OS hostname → "vp-node" fallback）。
        let handle = crate::daemon::hub_client::resolve_handle(None);
        let name = format!("VP Daemon ({handle})");
        // wld_id = federation の位置独立 routing key (ADR-020 D2)。db 不在で None なら空文字を
        // 送る (= 現状 hub は S2 未実装で無視するため非破壊、 handle ベース discover は維持)。
        let wld_id = node_id
            .as_ref()
            .map(|w| w.as_str().to_string())
            .unwrap_or_default();
        // endpoints = direct 到達候補 (ADR-020 D3-a、IPv6 GUA 優先・tailnet 非依存)。IPv6 経路が
        // 無ければ空配列 (= direct 候補なし、 dialer は relay floor に落ちる)。
        let endpoints = crate::node::endpoint::local_advertised_endpoints(port);

        // relay → VP wire 配送ポリシー（flow ③+⑤）。別 node が relay で送ってきた wire envelope
        // (`{from, to, body}`) を **ローカル中央 wire store に inject** する（= 遠方からの relay を
        // 「ローカル送信」に畳む）。宛先 lane は `wire_recv` で普通に拾う。store/notifier/notify は
        // AppState の Arc を capture（再接続ごとに handler 再登録するため closure は Clone）。
        let wire_store = state.wiremsg_store.clone();
        let wire_notifier = state.wire_notifier.clone();
        let wire_notify = state.delivery_notify.clone();
        // discovery（flow step 2）: lanes-query に応答するため lane_registry と hub_addr も capture。
        let fed_lane_registry = daemon_cap.read().await.lane_registry_ref();
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
                    // lane_registry（repo_path → Vec<LaneInfo>）を flatten。discovery は未認証の
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
                match crate::repo::routes::wire::dispatch_wire(
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
        // hub_status / hub_nodes は AppState と共有（run_hub_federation が更新、/api/health が読む）。
        tokio::spawn(crate::daemon::hub_client::run_hub_federation(
            hub_addr,
            wld_id,
            endpoints,
            handle,
            name,
            hub_status,
            hub_nodes,
            shutdown_token.clone(),
            on_relay,
        ));
    } else {
        tracing::debug!(
            "chronista-hub federation 無効 (env {} / config.kdl hub-addr とも未設定) — machine-local 動作",
            crate::daemon::hub_client::HUB_ADDR_ENV
        );
    }

    // doc 44 P1 (fold-in): health monitor は退役。旧構成では「別プロセスの repo が crash して
    // registry から消える」のを PID liveness で検知し respawn していたが、repo が Daemon 内の
    // Arc<AppState> になり、pid が全 repo 共通で Daemon 自身になったため、監視対象
    // （死にうる repo プロセス）が存在しなくなった。lane の engine（claude/codex）の死は
    // 別途 lane lifecycle monitor が見る。

    // 起動時設定の復帰: enabled な repo の repo を自動起動（VP-207）。
    // daemon restart 後に working set を復元する。1 回限りの startup タスク。
    let _autostart = tokio::spawn(RepoManagerCapability::autostart_enabled_repos(
        daemon_cap.clone(),
    ));

    // VP-129 MVP: lane root FSEvents watcher 起動。 user の Finder / `rm -rf` で performer dir
    // を削除した時、 OS file system event → repo `DELETE /api/lanes` 自動発火 (= D10 Reconciliation
    // の 3rd path 拡張、 Push QUIC + Pull port scan + FSEvents の 3-trigger model 完成)。
    let _lane_watcher = tokio::spawn(RepoManagerCapability::run_lane_watcher(
        daemon_cap.clone(),
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
                                    let repo_name = data["repo_name"]
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
                                        repo_name,
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
            tracing::info!("Daemon graceful shutdown initiated");
        })
        .await?;

    // クリーンアップ
    daemon_handle.abort();

    // Shutdown capabilities
    tracing::info!("Shutting down Daemon...");
    // doc 44 P1 (fold-in): capability を畳む前に、daemon が抱える repo を全部停止する。
    // 旧構成では repo = 別プロセスで daemon 停止後も生き残るのが正だったため、この
    // 後始末はどこにも無かった。in-process 化でその責務が daemon に移っている。
    let stopped_repos = control_channels.shutdown_all().await;
    if stopped_repos > 0 {
        tracing::info!("Daemon shutdown: {} repo を停止", stopped_repos);
    }
    // DeviceRegistry ROTO 持続セッションを停止（子 token は shutdown_token から伝播済だが、明示 abort で確実に畳む）。
    #[cfg(feature = "midi")]
    if let Some(devices) = devices_for_shutdown.as_ref() {
        devices.write().await.stop_roto_control().await;
    }
    if let Err(e) = daemon_for_shutdown.write().await.shutdown().await {
        tracing::warn!("Error during daemon shutdown: {}", e);
    }
    {
        let mut update = update_cap.write().await;
        if let Err(e) = update.shutdown().await {
            tracing::warn!("Error during update shutdown: {}", e);
        }
    }

    // SurrealDB は独立デーモンなので daemon 終了時には止めない
    // 再起動が必要な場合は `vp db restart` を使用

    // PID ファイル削除
    process::remove_pid_file();
    tracing::info!("Daemon stopped");
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
/// 関連: repo register が `http://[::1]:32000` で daemon に register していた箇所が
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
/// - sidebar は /api/lanes polling で更新後 state を picker → 赤 dot 表示 → user の Restart repo に誘導
///
/// ## 設計判断: 検知のみ (auto-respawn なし)
/// 「自動再起動」は max retries / cooldown / 無限 loop 防止が必要で複雑化する。
/// まず「Dead 状態を即時 UI に反映」 で user の最低要件を満たし、 auto-respawn は別 PR で。
///
/// ## shutdown
/// `shutdown_token.cancelled()` で graceful 終了。 repo shutdown で task も clean に止まる。
fn spawn_lane_lifecycle_monitor(
    lane_pool: Arc<RwLock<super::lanes_state::LanePool>>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        // 初回 tick は即時発火するので 1 周回飛ばす (repo 起動直後の他 setup を妨げない配慮)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 44 P1 (fold-in) 回帰固定: lanes の供給点は daemon の集約 view を必ず更新する。
    ///
    /// この不変条件は旧構成では **repo の QUIC uplink が担うプロセス跨ぎの約束**だったため
    /// 単体テストの射程外にあり、fold-in で uplink を落とした際に誰にも気付かれずに
    /// 失われた（実機で lane 一覧が boot 時 db 値のまま固まり、存在しない
    /// 過去 pid の lane を配り続けた）。関数呼び出しになった今はここで固定できる。
    #[tokio::test]
    async fn publish_lanes_updates_daemon_view() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let hub = state.hub.clone();
        let view: NodeLaneView = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let key = "/tmp/proj-publish-lanes";

        publish_lanes(
            &state,
            &hub,
            &Some(view.clone()),
            key,
            &mut LaneChangeNotifier::new(None),
        )
        .await;

        // lane 数が 0 でも **entry 自体は入る**ことが要点。空 Vec の insert が
        // 「この repo にはもう lane が無い」を表明し、boot 時に db から載った
        // stale 行を上書きして消す役割を持つ。
        assert!(
            view.read().await.contains_key(key),
            "publish 後、Daemon view に当該 repo の entry が存在すること"
        );
    }

    /// Daemon view 不在（= repo プロセス経路 / test）でも publish は成立する。
    ///
    /// `run()` 経路は uplink が daemon へ中継するので view を持たない。ここが panic すると
    /// fold-in 完了前の repo 単体起動が壊れる。
    #[tokio::test]
    async fn publish_lanes_without_daemon_view_is_noop() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let hub = state.hub.clone();

        publish_lanes(
            &state,
            &hub,
            &None,
            "/tmp/proj-no-view",
            &mut LaneChangeNotifier::new(None),
        )
        .await;
    }

    /// doc 44 §11 回帰固定: **publish が vp-app への push を起こす**。
    ///
    /// fold-in で repo の uplink（register / lanes-diff）が消えた際、daemon の集約 view の
    /// 更新は `publish_lanes` へ移管されたが、**同じ uplink が担っていた `lane_change_tx`
    /// の発火は移管されなかった**。結果、daemon の "lanes" push loop を起こすのは
    /// wire send/ack だけになり、vp-app の sidebar は「何か打っている間だけ新鮮」
    /// という状態になっていた（idle 中は lane 追加・死活・git meta が固まる）。
    ///
    /// `process_lifecycle_tx` が同じ形の抜けを起こしていた前例がある（そちらは
    /// 「生産者ゼロで永久沈黙」として fold-in 中に発見・再配線済）。
    #[tokio::test]
    async fn publish_lanes_wakes_vp_app_push_loop() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let hub = state.hub.clone();
        let key = "/tmp/proj-wakeup";
        let (tx, mut rx) = tokio::sync::broadcast::channel::<String>(8);
        let mut notifier = LaneChangeNotifier::new(Some(tx));

        publish_lanes(&state, &hub, &None, key, &mut notifier).await;
        assert_eq!(
            rx.try_recv().ok().as_deref(),
            Some(key),
            "初回 publish は push loop を起こす"
        );
    }

    /// 5s tick が**そのまま 5s ごとの全 snapshot push にならない**こと。
    ///
    /// 供給点の 1 つは 5s periodic tick（disk-only performer の safety net）で、
    /// 内容が変わっていなくても回る。ここで毎回起こすと repo 数ぶんの全 snapshot が
    /// 定期的に流れる。指紋で「変わった時だけ」に絞る。
    #[tokio::test]
    async fn publish_lanes_does_not_wake_on_unchanged_snapshot() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let hub = state.hub.clone();
        let key = "/tmp/proj-unchanged";
        let (tx, mut rx) = tokio::sync::broadcast::channel::<String>(8);
        let mut notifier = LaneChangeNotifier::new(Some(tx));

        publish_lanes(&state, &hub, &None, key, &mut notifier).await;
        assert!(rx.try_recv().is_ok(), "初回は起こす");

        publish_lanes(&state, &hub, &None, key, &mut notifier).await;
        assert!(
            rx.try_recv().is_err(),
            "内容が同じなら起こさない（5s tick が push 源にならない）"
        );
    }

    /// 指紋は「vp-app に届く値そのもの」から取る = **見えている値が変われば必ず起こす**。
    ///
    /// lanes だけを指紋にすると、起点（`origin`）だけが変わった時に起こさない穴ができる
    /// （= D4 の「開発起点にする」を押しても star が動かない）。
    #[tokio::test]
    async fn notifier_wakes_when_only_origin_changes() {
        let key = "/tmp/proj-origin-only";
        let (tx, mut rx) = tokio::sync::broadcast::channel::<String>(8);
        let mut notifier = LaneChangeNotifier::new(Some(tx));

        // lanes は同一で origin だけ違う 2 つの snapshot を直接与える。
        let snapshot = |origin: &str| {
            serde_json::to_string(&crate::protocol::RepoMessage::LanesSnapshot {
                lanes: vec![],
                origin: Some(origin.to_string()),
            })
            .unwrap()
        };

        assert!(notifier.notify_if_changed(key, snapshot("root")));
        assert!(rx.try_recv().is_ok());
        assert!(
            notifier.notify_if_changed(key, snapshot("feat-x")),
            "origin だけの変化でも起こす"
        );
        assert!(rx.try_recv().is_ok());
    }

    // =====================================================================
    // doc 45 段 4 — HTTP 面の route 登録そのものを固定する
    //
    // 撤去 PR の危険は 2 方向ある: (a) 残すべきものを巻き添えで落とす、
    // (b) 消したつもりの route が登録に残る。route 表は「登録」と「handler」が
    // 別ファイルにあるので、片方だけ消しても静的には気付けない。
    // `build_daemon_router` を組んで実際に叩き、両方向を 1 箇所で見る。
    // =====================================================================

    async fn route_status(uri: &str, method: &str) -> axum::http::StatusCode {
        use tower::ServiceExt;
        let state = crate::repo::state::build_test_app_state(None).await;
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .expect("request");
        build_daemon_router(state)
            .oneshot(req)
            .await
            .expect("oneshot")
            .status()
    }

    /// doc 45 §2 で HTTP に残すと決めた 2 本が、撤去の巻き添えで消えていないこと。
    ///
    /// health は「他が壊れている時に動いてほしい」probe（`.mise/tasks/app/swap` の Ruby と
    /// `apple/VantagePointAgent` の Swift という **VP 外の消費者**も居る）、shutdown は
    /// 緊急停止。両方同時に失うと診断手段と止める手段が同時に消える。
    #[tokio::test]
    async fn daemon_router_keeps_health_and_shutdown() {
        assert_eq!(
            route_status("/api/health", "GET").await,
            axum::http::StatusCode::OK,
            "GET /api/health は HTTP に残す（doc 45 §2）"
        );
        assert_eq!(
            route_status("/api/shutdown", "POST").await,
            axum::http::StatusCode::OK,
            "POST /api/shutdown は HTTP に残す（doc 45 §2）"
        );
    }

    /// 段 4 で撤去した control plane route が **登録にも残っていない**こと。
    ///
    /// handler を消しても route 登録が残っていれば compile エラーになるが、逆
    /// （登録だけ消して handler が残る）は dead code 警告でしか気付けない。ここは
    /// 「外から見て面が消えている」を直接確かめる側。
    #[tokio::test]
    async fn daemon_router_drops_removed_control_routes() {
        for (uri, method) in [
            ("/api/daemon/repos", "GET"),
            ("/api/daemon/repos", "POST"),
            ("/api/daemon/repos/reorder", "POST"),
            ("/api/daemon/repos/update", "POST"),
            ("/api/daemon/repos/remove", "POST"),
            ("/api/daemon/repos/reload", "POST"),
            ("/api/daemon/repos/sync", "POST"),
            ("/api/daemon/processes", "GET"),
            ("/api/daemon/lanes", "GET"),
            ("/api/daemon/lanes", "POST"),
            ("/api/daemon/lanes/active", "POST"),
            ("/api/daemon/processes/vp/start", "POST"),
            ("/api/daemon/processes/vp/stop", "POST"),
            ("/api/daemon/processes/vp/restart", "POST"),
            ("/api/daemon/processes/vp/pointview", "POST"),
            ("/api/canvas/switch_lane", "POST"),
            ("/api/canvas/layout", "GET"),
            ("/api/canvas/layout", "POST"),
        ] {
            assert_eq!(
                route_status(uri, method).await,
                axum::http::StatusCode::NOT_FOUND,
                "{method} {uri} は Unison daemon-control に移設済み（doc 45 段 4）"
            );
        }
    }

    /// `/api/update/*` は段 4 のスコープ外（doc 45 §3「churn が低いので後回しでよい」）。
    /// 「ついでに消えた」を検出する側の網。
    #[tokio::test]
    async fn daemon_router_keeps_update_routes() {
        for (uri, method) in [
            ("/api/update/check", "GET"),
            ("/api/update/apply", "POST"),
            ("/api/update/rollback", "POST"),
            ("/api/update/restart", "POST"),
            ("/api/update/mac/check", "GET"),
            ("/api/update/mac/apply", "POST"),
            ("/api/update/mac/rollback", "POST"),
        ] {
            assert_ne!(
                route_status(uri, method).await,
                axum::http::StatusCode::NOT_FOUND,
                "{method} {uri} は段 4 のスコープ外（route は残す）"
            );
        }
    }
}
