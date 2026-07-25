//! Lane spawn actor — `LaneCmd::SpawnLane` を in-process channel 経由で受信し、 内部 Semaphore で
//! 並列度を gate しつつ `stand_spawner::spawn_with_fallback` で Lane を spawn する Service actor。
//!
//! ## 背景 (I-b、 2026-04-30)
//!
//! 従来 SP 起動時に Performer N 本を **直列 sync ループ** で spawn していた。 内部の
//! `spawn_with_fallback` が `EARLY_EXIT_CHECK_MS = 800ms` の `std::thread::sleep` で
//! executor を block するため、 N 本で `800ms × N` の累積待ち → SP の axum listen
//! ready が遅延する設計上の問題があった。
//!
//! 本 actor は user 提案 (2026-04-30) を実装したもの:
//! - 「SP は一気に claude cli 叩くから、 最大数設定して、 順次、 Pane を復活させたいね」
//! - 「Cmd にして tokio channel で recv、 CommandRunner で常時 N 動かす、 cmd type で queue 振り分け」
//!
//! ## VP-159 PR-3 → PR-4b (2026-05-11) — struct 化 + Service + SpawnableService
//!
//! - **PR-3**: 既存 `pub fn spawn(...)` 経路を `LaneSpawnActor` struct に集約、 `Service` trait に
//!   形式登録 (= ECS 純度回復、 actor を struct で表現)。
//! - **PR-4b**: `SpawnableService` super-trait を impl (= `spawn(self)` → `spawn_loop(self) ->
//!   JoinHandle<()>` に統一)、 caller (= server.rs) は `ActorRegistry::spawn_service` 経由に集約
//!   (= JoinHandle を ActorRegistry が保持、 PR-5 supervisor 統一の foundation)。
//!
//! ## in-process channel 直結 (2026-07-09) — SP 再起動時の幽霊 long-poll 消費の根治
//!
//! 旧実装 (wiremsg R2-a、 2026-06-11) は recv path を TheWorld 中央 wire store への
//! HTTP long-poll (`lane-spawn@<project>` mailbox) で行っていたが、 producer は同一
//! process の bootstrap (server.rs) **のみ**であり、 自プロセス内の指示に TheWorld 往復
//! (4-hop) を挟む構造だった。 この配送は at-most-once (recv = fetch と同時に per-agent
//! cursor 前進の破壊的読み出し) のため、 SP 再起動シーケンスで Cmd が失われる:
//! 旧 SP の actor が張った long-poll が World 側に残存 (≤30s 窓) → 新 SP bootstrap の
//! Cmd を fetch → cursor 前進 → 応答は死んだ接続へ → 新 actor には何も届かない
//! → performer 永久 Spawning (2026-07-09 障害)。
//!
//! 本修正で bootstrap → actor を `tokio::sync::mpsc` unbounded channel に直結。
//! channel は process-local なので旧 SP の consumer が新 SP の Cmd を消費する経路が
//! 構造的に消滅し、 TheWorld 不達 retry も不要になった (standalone SP でも spawn 可能)。
//! Semaphore gate / race guard / `handle_cmd` の内部挙動は完全互換。
//!
//! ## 設計
//!
//! - **入口**: `cmd_rx: mpsc::UnboundedReceiver<LaneCmd>` (constructor 注入)。
//!   producer は同 Process の bootstrap (server.rs) が持つ Sender のみ。 bootstrap 完了で
//!   Sender drop → channel close → actor は buffered Cmd を全 drain 後に**正常終了**する
//!   (= actor は「起動時一斉 spawn の Semaphore gate」、 仕事が尽きたら畳む)
//! - **Cmd 型**: `LaneCmd::SpawnLane{...}` (= `crate::process::lane_cmd`) を型付きで直接 send
//!   (serialize 不要)
//! - **concurrency**: `Arc<Semaphore::new(max_concurrent)>` で permit gate、 各 Cmd は
//!   `tokio::spawn` で並列処理されるが Semaphore で同時実行上限を制御
//! - **blocking 隔離**: `spawn_with_fallback` の 800ms sync sleep を `tokio::task::spawn_blocking`
//!   で隔離し、 actor の recv loop と他 task を妨げない
//! - **race guard**: permit 待ち中に手動 `POST /api/lanes` で同 addr が create された場合、
//!   spawn 完了後の `pool.write()` で再 check し、 lost race なら spawn 済 PtySlot を drop で zombie reap
//! - **graceful degrade**: spawn 失敗 = `LaneState::Dead` + pid:None で record
//!   (= sidebar は Dead entry を dim 表示、 手動 retry 可能)
//!
//! ## 計測 log (dogfood で N 値決定の足場)
//!
//! - `Lane spawn requested: addr=... cwd=... stand=...` — permit acquire 後
//! - `Lane spawn completed: addr=... pid=... elapsed_ms=...` — slot insert 成功
//! - `Lane spawn failed: addr=... err=... elapsed_ms=...` — graceful degrade
//!
//! ## shutdown
//!
//! 2 経路とも clean: (1) `shutdown_token.cancelled()` で recv loop 終了、 (2) Sender drop
//! (= bootstrap 完了) で `recv() == None` → buffered Cmd を drain し切ってから**正常終了**
//! (終了 log に明記 — 将来の supervisor が「完了 = crash」と誤判定しないため)。
//! in-flight tokio task は detach (= 自然完了) で graceful。
//! max_concurrent 個までの待機時間を許容する trade-off。
//!
//! ## 関連
//!
//! - 設計 spec: memory `mem_1CaZiXoUVvZ4hSrYtVSW8R` (I-b design spark, 2026-04-30)
//! - Cmd 定義: `super::lane_cmd::LaneCmd`
//! - VP-159 PR-3 — Service trait 形式登録 (= ECS 純度回復)
//! - parent epic: VP-156 (Mailbox routing 統一)
//! - PR-2 同型 pattern: `AgentCapability` / `ProtocolCapability` (impl Stand)

use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capability::stand_service::{LayerScope, Service, SpawnableService};

use super::lane_capabilities::LaneCapabilitiesPool;
use super::lane_cmd::LaneCmd;
use super::lanes_state::{Diff, LaneAddress, LaneInfo, LanePool, LaneState, SystemEvent};

/// Lane spawn Service (= in-process channel から `LaneCmd::SpawnLane` を recv、
/// 並列度 N で gate しつつ Lane を spawn する infra actor)。
///
/// SP-local Service (= 1 Project per Process)、 channel receiver + dependencies を保持し、
/// `spawn_loop(shutdown)` で background recv loop を `tokio::spawn` 起動する。
///
/// PR-β-2 (VP-120): `lane_capabilities_pool: Option<...>` で Performer spawn 成功時に
/// `populate_lane` を呼び、 Lane あたり独立 PaisleyParkState を host する。
pub struct LaneSpawnActor {
    lane_pool: Arc<RwLock<LanePool>>,
    lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
    system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
    /// doc 53 R2: spawn / boot 復元の末尾で terminal pump を reconcile するための台帳 + router。
    /// boot 中に demand（購読）が先に立っていても、 復元完了後の reconcile が残りの slot に
    /// pump を揃える（doc 50 §4.7「直さないと決めた 1 件」の根治）。
    terminal_pumps: Arc<RwLock<crate::process::terminal_pump::TerminalPumps>>,
    topic_router: Arc<crate::process::topic_router::TopicRouter>,
    max_concurrent: usize,
    /// bootstrap (server.rs) からの in-process Cmd 入口。 Sender drop = 投入完了の合図
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<LaneCmd>,
}

impl LaneSpawnActor {
    /// 新しい `LaneSpawnActor` を構築する。
    ///
    /// in-process 直結 (2026-07-09): 旧 wire long-poll (TheWorld 中央 store の
    /// `lane-spawn@<project>` mailbox) を撤去し、 `cmd_rx` (unbounded channel) を
    /// constructor 注入する。 unbounded なので producer の send は receiver 生存中
    /// infallible かつ recv loop 開始前の send もバッファされる (投入順序に依存しない)。
    ///
    /// `max_concurrent=0` は意味的に「全 spawn を block」 だが事故 config の可能性が高いため、
    /// `spawn_loop()` 内で 1 に丸めて warn する (= sequential、 `Semaphore::new(0)` の永久 block 回避)。
    pub fn new(
        lane_pool: Arc<RwLock<LanePool>>,
        lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
        system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
        terminal_pumps: Arc<RwLock<crate::process::terminal_pump::TerminalPumps>>,
        topic_router: Arc<crate::process::topic_router::TopicRouter>,
        max_concurrent: usize,
        cmd_rx: tokio::sync::mpsc::UnboundedReceiver<LaneCmd>,
    ) -> Self {
        Self {
            lane_pool,
            lane_capabilities_pool,
            system_event_tx,
            terminal_pumps,
            topic_router,
            max_concurrent,
            cmd_rx,
        }
    }
}

impl Service for LaneSpawnActor {
    fn actor_name(&self) -> &str {
        "lane-spawn"
    }

    fn layer_scope(&self) -> LayerScope {
        // SP-local Service (= 1 Project per Process)
        LayerScope::Project
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl SpawnableService for LaneSpawnActor {
    /// recv loop を `tokio::spawn` で起動し、 `JoinHandle<()>` を返す。 `self` は consume される。
    ///
    /// shutdown_token.cancelled() で loop 終了、 channel close (= recv が None) でも終了。
    /// VP-159 PR-4b: 旧 `spawn(self, shutdown)` (= 戻り値なし) を `spawn_loop` に統一、
    /// ActorRegistry が JoinHandle を保持する path を開く。 max_concurrent=0 は意味的に
    /// 「全 spawn を block」 だが事故 config の可能性が高い、 1 に丸めて warn する
    /// (= sequential、 Semaphore::new(0) の永久 block 回避)。
    fn spawn_loop(self, shutdown: CancellationToken) -> JoinHandle<()> {
        let n = if self.max_concurrent == 0 {
            tracing::warn!(
                "Lane spawn actor: max_concurrent=0 は無効、 1 に丸めます (config 確認推奨)"
            );
            1
        } else {
            self.max_concurrent
        };
        let semaphore = Arc::new(Semaphore::new(n));

        let Self {
            lane_pool,
            lane_capabilities_pool,
            system_event_tx,
            terminal_pumps,
            topic_router,
            mut cmd_rx,
            ..
        } = self;

        tokio::spawn(async move {
            tracing::info!(
                "Lane spawn actor 起動 (in-process channel, max_concurrent={})",
                n
            );
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("Lane spawn actor: shutdown");
                        return;
                    }
                    maybe = cmd_rx.recv() => match maybe {
                        Some(cmd) => {
                            let sem = semaphore.clone();
                            let pool = lane_pool.clone();
                            let lc_pool = lane_capabilities_pool.clone();
                            let tx = system_event_tx.clone();
                            let pumps = terminal_pumps.clone();
                            let router = topic_router.clone();
                            tokio::spawn(async move {
                                handle_cmd(cmd, pool, lc_pool, tx, pumps, router, sem).await;
                            });
                        }
                        None => {
                            // Sender drop = bootstrap 投入完了。 buffered Cmd は全 drain 済
                            // (recv は close 後も buffer を返し切ってから None になる)。
                            // これは**正常終了** — 将来の supervisor が「完了 = crash」と
                            // 誤判定しないよう明記しておく。
                            tracing::info!(
                                "Lane spawn actor: channel closed (bootstrap 投入完了・全 Cmd drain 済) → 正常終了"
                            );
                            return;
                        }
                    }
                }
            }
        })
    }
}

/// 単一 `LaneCmd` を処理。 Semaphore permit を acquire してから heavy spawn を実行。
///
/// PR-β-2 (VP-120): `lane_capabilities_pool` 引数 (Option) を追加、 spawn 成功時に
/// `populate_lane` を呼んで Performer Lane あたり独立 PaisleyParkState を host する。
async fn handle_cmd(
    cmd: LaneCmd,
    pool: Arc<RwLock<LanePool>>,
    lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
    system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
    terminal_pumps: Arc<RwLock<crate::process::terminal_pump::TerminalPumps>>,
    topic_router: Arc<crate::process::topic_router::TopicRouter>,
    semaphore: Arc<Semaphore>,
) {
    let LaneCmd::SpawnLane {
        project_id,
        name,
        cwd,
        stand,
    } = cmd;

    let addr = LaneAddress::performer(&project_id, &name);

    // 早期 skip: permit 待つ前に既存 entry を check (= 手動 create と被った時の無駄 acquire 削減)
    {
        let pool_read = pool.read().await;
        if pool_read.get(&addr).is_some() {
            tracing::debug!(
                "Lane spawn actor: 既存 entry のため skip (pre-acquire) addr={}",
                addr
            );
            return;
        }
    }

    // permit acquire — N 本同時まで通過、 残りは queue で wait
    let _permit = match semaphore.acquire_owned().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Lane spawn actor: semaphore closed: {}", e);
            return;
        }
    };

    // permit 待ち中に手動 create で同 addr が入ってきた可能性を再 check
    {
        let pool_read = pool.read().await;
        if pool_read.get(&addr).is_some() {
            tracing::debug!(
                "Lane spawn actor: 既存 entry のため skip (post-acquire) addr={}",
                addr
            );
            return;
        }
    }

    tracing::info!(
        "Lane spawn requested: addr={} cwd={} stand={}",
        addr,
        cwd,
        stand
    );
    let started = Instant::now();

    // doc 53 §12: **actor は登録だけ**。実体（PtySlot / engine）は下の `reconcile_lane` が
    // registry に従って立てる。
    //
    // 旧実装はここに 2 つの分岐を持っていた: ①root の act が Chat なら engine-less で登録して
    // 早期 return ②Tui なら root を spawn → insert → さらに `restore_term_slots` で非 root を
    // 復元。どちらも「registry を読んで実体を作る」仕事で、reconcile と同じことを別の場所で
    // 書いていた（census §10.1 の boot 行）。act の分岐は desired の導出規則
    // （act=Tui → slot / act=Chat → engine は lazy）に吸収される。
    let lane_id = crate::lane::lane_id::load_or_create(&addr.project, &name);
    let info = LaneInfo {
        id: lane_id,
        address: addr.clone(),
        // 代表値は reconcile が実体から導出して上書きする（doc 53 §3.3）。
        state: LaneState::Running,
        stand: stand.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        pid: None,
        cwd,
        // 起動時点では git 状態取得しない (list_handler 側で必要時に enrich)。
        performer_status: None,
        cc_session_id: None,
        sessions: None,
        engine_session_id: None,
        engine_stand: None,
        flow_state: None,
    };
    {
        let mut pool_write = pool.write().await;
        if pool_write.get(&addr).is_some() {
            tracing::debug!(
                "Lane spawn actor: race lost (register) addr={}、 skip",
                addr
            );
            return;
        }
        pool_write.insert(info.clone());
    }
    // 実体を立てる（3 段隔離は reconcile の中 — 800ms×N を lock 下で回さない）。
    // 失敗しても intent は残る = 次の契機で再試行される（doc 53 §12.2）。
    let r = crate::process::lane_reconcile::reconcile_lane(
        &pool,
        &terminal_pumps,
        &topic_router,
        &addr,
    )
    .await;
    tracing::info!(
        "Lane spawn completed: addr={} spawned={} failed={} elapsed_ms={}",
        addr,
        r.spawned,
        r.failed,
        started.elapsed().as_millis() as u64
    );

    // Performer Lane spawn 完了 → LaneCapabilities pool に entry 追加
    // (Lane あたり独立 PaisleyParkState を host、 doc 13 §6 自動 spawn rule = default)。
    // None は World mode (Lane scope なし) で発生、 SP mode では常に Some。
    //
    // doc 53 §12.2: **spawn 失敗でも populate する**（旧: Dead なら skip）。intent が残る以上
    // lane は「立ち上がっていないが在る」— 次の契機で slot が立った時に PP だけ不在、を
    // 作らない。chat lane（PTY 無しで正常）も同じ扱いになり、旧 state 分岐の非対称も消える。
    if let Some(lc_pool) = lane_capabilities_pool.as_ref() {
        lc_pool.write().await.populate_lane(addr.clone(), &stand);
        tracing::debug!(
            "LaneCapabilities pool に Performer Lane populate (addr={}, stand={})",
            addr,
            stand
        );
    }

    // Phase 2 (Step E): Performer spawn 完了を SystemEvent::Lane(Diff::Add) で TheWorld に push。
    // QUIC registry channel 経由で realtime sync。 失敗は warn のみ (best-effort、
    // SP lane_pool が SSOT、 reconnect 時に register snapshot で必ず再構築される)。
    if let Err(e) = system_event_tx.send(SystemEvent::Lane(Diff::Add { payload: info })) {
        tracing::warn!(
            "Lane spawn actor: SystemEvent publish 失敗 addr={} err={}",
            addr,
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// max_concurrent=0 は 1 に丸められること。 Semaphore::new(0) を踏むと永久 block するため
    /// runtime に到達しないことを config 側ではなく actor 側で防ぐ contract test。
    #[tokio::test]
    async fn spawn_zero_concurrent_does_not_hang() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LaneCmd>();
        let shutdown = CancellationToken::new();

        // 0 を渡しても 1 に丸めて起動するはず (= タイムアウトせずに actor 起動 + shutdown 完了)
        // PR-β-2 (VP-120): lane_capabilities_pool = None で test (Lane scope なしの動作確認)
        let handle = LaneSpawnActor::new(
            pool,
            None,
            tx,
            Default::default(),
            std::sync::Arc::new(crate::process::topic_router::TopicRouter::new()),
            0,
            cmd_rx,
        )
        .spawn_loop(shutdown.clone());

        // shutdown して terminate を確認 (= 永久 block 回避)。 JoinHandle 完了を timeout 付きで
        // 決定的に検証 (旧 sleep ベースから改善)。
        shutdown.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("actor は shutdown cancel で 1s 以内に終了するはず")
            .expect("actor task は panic せず終了するはず");
    }

    /// actor 起動 → shutdown 完了の smoke test (shutdown_token 経路)。
    #[tokio::test]
    async fn actor_shuts_down_cleanly() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LaneCmd>();
        let shutdown = CancellationToken::new();

        // PR-β-2 (VP-120): lane_capabilities_pool = None で test
        let handle = LaneSpawnActor::new(
            pool.clone(),
            None,
            tx,
            Default::default(),
            std::sync::Arc::new(crate::process::topic_router::TopicRouter::new()),
            1,
            cmd_rx,
        )
        .spawn_loop(shutdown.clone());

        // Cmd 未投入なので pool は空のまま
        assert_eq!(pool.read().await.count(), 0);

        shutdown.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("actor は shutdown cancel で 1s 以内に終了するはず")
            .expect("actor task は panic せず終了するはず");
    }

    /// Sender drop (= bootstrap 投入完了) で actor が正常終了する contract test。
    ///
    /// in-process 直結 (2026-07-09) の終了契約: channel close → recv() が None → return。
    #[tokio::test]
    async fn actor_exits_when_channel_closes() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LaneCmd>();
        let shutdown = CancellationToken::new();

        let handle = LaneSpawnActor::new(
            pool,
            None,
            tx,
            Default::default(),
            std::sync::Arc::new(crate::process::topic_router::TopicRouter::new()),
            1,
            cmd_rx,
        )
        .spawn_loop(shutdown);

        // Sender を drop → channel close → actor は自発的に正常終了するはず
        drop(cmd_tx);
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("actor は Sender drop で 1s 以内に正常終了するはず")
            .expect("actor task は panic せず終了するはず");
    }

    /// recv loop 開始**前**に send された Cmd も失われず drain されること (投入順序の安全性)。
    ///
    /// unbounded channel は receiver 生存中の send をバッファするため、 bootstrap の send が
    /// actor の recv loop 開始より先でも喪失しない — 幽霊消費バグ (2026-07-09) の対極となる
    /// 配送保証の直接検証。 addr を事前に pool へ insert しておくことで handle_cmd は
    /// pre-acquire race guard で即 return し、 実 PTY spawn / file IO なしで完結する。
    #[tokio::test]
    async fn buffered_cmds_are_drained_before_close() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LaneCmd>();
        let shutdown = CancellationToken::new();

        // race guard を意図的に踏ませる: 同 addr を事前に pool へ insert しておく
        let addr = LaneAddress::performer("proj", "already-there");
        pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: addr,
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: "/nonexistent".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        });

        // actor 起動**前**に send → buffer される
        cmd_tx
            .send(LaneCmd::SpawnLane {
                project_id: "proj".to_string(),
                name: "already-there".to_string(),
                cwd: "/nonexistent".to_string(),
                stand: "echoes".to_string(),
            })
            .expect("receiver 生存中の send は成功するはず");
        drop(cmd_tx);

        let handle = LaneSpawnActor::new(
            pool.clone(),
            None,
            tx,
            Default::default(),
            std::sync::Arc::new(crate::process::topic_router::TopicRouter::new()),
            1,
            cmd_rx,
        )
        .spawn_loop(shutdown);

        // buffered Cmd を drain (→ race guard で skip) してから channel close で正常終了
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("actor は buffered Cmd drain 後 2s 以内に終了するはず")
            .expect("actor task は panic せず終了するはず");

        // handle_cmd の spawn task が guard で return するのを待つ (dispatch は detach のため)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // race guard により重複 spawn されず、 pool は事前 insert の 1 件のまま
        assert_eq!(pool.read().await.count(), 1);
    }

    /// **performer** の永続 console_mode=chat が boot spawn で honor され、 engine-less
    /// (pid=None + state=Running + PtySlot なし) で登録されること。
    ///
    /// これが「Act II の performer lane を再起動しても chat のまま復活する」の中核。
    /// 壊れると chat performer が boot で PTY を立て、 echoes_submit が 2 本目 engine を
    /// 呼んで 1 会話 2 エンジンになる (conductor `with_root` と同じ規律を performer に適用)。
    #[tokio::test]
    async fn chat_mode_performer_boots_engine_less() {
        use crate::lane::session_registry::SessionAct;
        // session_registry / lane_id は vp_state_dir() = $XDG_STATE_HOME/vp を読む。
        // crate 唯一のロック下で tempdir に向け、 guard の drop で復元する。
        let state = crate::test_env::state_dir_async().await;

        // performer "proj"/"chat-perf" の **root session の act** を Chat で永続化（doc 47 §4）
        crate::lane::session_registry::set_root_act(
            "proj",
            "chat-perf",
            "echoes",
            SessionAct::Chat,
        )
        .expect("record chat act");

        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<LaneCmd>();
        let shutdown = CancellationToken::new();

        cmd_tx
            .send(LaneCmd::SpawnLane {
                project_id: "proj".to_string(),
                name: "chat-perf".to_string(),
                cwd: state.path().to_string_lossy().to_string(),
                stand: "echoes".to_string(),
            })
            .expect("send SpawnLane");
        drop(cmd_tx);

        let handle = LaneSpawnActor::new(
            pool.clone(),
            None,
            tx,
            Default::default(),
            std::sync::Arc::new(crate::process::topic_router::TopicRouter::new()),
            1,
            cmd_rx,
        )
        .spawn_loop(shutdown);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("actor 終了")
            .expect("actor task panic せず");
        // detach spawn task の完了待ち
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let addr = LaneAddress::performer("proj", "chat-perf");
        let pool_read = pool.read().await;
        let info = pool_read
            .get(&addr)
            .expect("chat performer が登録されるはず");
        // doc 53 R1: 投影 field は退役 — honor の証明は挙動（下の pid/PtySlot assert）と
        // 読み手経路（root_act 直読）で行う。
        assert_eq!(
            pool_read.root_act(&addr),
            SessionAct::Chat,
            "boot 後も読み手（root_act 直読）が永続 chat act を見る"
        );
        assert_eq!(
            info.state,
            LaneState::Running,
            "chat lane は Running が正常形"
        );
        assert_eq!(info.pid, None, "chat lane は engine-less (PTY を立てない)");
        assert!(
            pool_read.subscribe_output(&addr, None).is_none(),
            "chat lane に PtySlot は存在しないはず"
        );
        drop(pool_read);
        // env は `state` guard の drop で復元される。
    }
}
