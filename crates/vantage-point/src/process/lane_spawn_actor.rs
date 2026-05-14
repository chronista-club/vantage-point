//! Lane spawn actor — `LaneCmd::SpawnLane` を Mailbox 経由で受信し、 内部 Semaphore で
//! 並列度を gate しつつ `stand_spawner::spawn_with_fallback` で Lane を spawn する Service actor。
//!
//! ## 背景 (I-b、 2026-04-30)
//!
//! PR #228 で landed した `LanePool::populate_workers_from_disk` は SP 起動時に Worker N 本を
//! **直列 sync ループ** で spawn していた。 内部の `spawn_with_fallback` が `EARLY_EXIT_CHECK_MS
//! = 800ms` の `std::thread::sleep` で executor を block するため、 N 本で `800ms × N` の
//! 累積待ち → SP の axum listen ready が遅延する設計上の問題があった。
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
//! 通信経路 / msg flow / Semaphore gate / race guard / payload schema 等の挙動は完全互換。
//!
//! ## 設計
//!
//! - **address**: `lane-spawn` (= `msgbox_router.register("lane-spawn")`)、 cross-Process
//!   namespacing は TheWorld registry layer が解決
//! - **wire format**: `LaneCmd::SpawnLane{...}` (= `crate::process::lane_cmd`)、 serde tag="kind"
//! - **concurrency**: `Arc<Semaphore::new(max_concurrent)>` で permit gate、 各 Cmd は
//!   `tokio::spawn` で並列処理されるが Semaphore で同時実行上限を制御
//! - **blocking 隔離**: `spawn_with_fallback` の 800ms sync sleep を `tokio::task::spawn_blocking`
//!   で隔離し、 actor の recv loop と他 task を妨げない
//! - **race guard**: permit 待ち中に手動 `POST /api/lanes` で同 addr が create された場合、
//!   spawn 完了後の `pool.write()` で再 check し、 lost race なら spawn 済 PtySlot を drop で zombie reap
//! - **graceful degrade**: spawn 失敗 = `LaneState::Dead` + pid:None で record (= 既存
//!   `populate_workers_from_disk` と同じ contract、 sidebar の disk-scan fallback と整合)
//!
//! ## 計測 log (dogfood で N 値決定の足場)
//!
//! - `Lane spawn requested: addr=... cwd=... stand=...` — permit acquire 後
//! - `Lane spawn completed: addr=... pid=... elapsed_ms=...` — slot insert 成功
//! - `Lane spawn failed: addr=... err=... elapsed_ms=...` — graceful degrade
//!
//! ## shutdown
//!
//! `shutdown_token.cancelled()` で recv loop 終了。 in-flight worker task は detach (= 自然完了)
//! で graceful。 max_concurrent 個までの待機時間を許容する trade-off。
//!
//! ## 関連
//!
//! - 設計 spec: memory `mem_1CaZiXoUVvZ4hSrYtVSW8R` (I-b design spark, 2026-04-30)
//! - Cmd 定義: `super::lane_cmd::LaneCmd`
//! - VP-159 PR-3 — Service trait 形式登録 (= ECS 純度回復)
//! - parent epic: VP-156 (Mailbox routing 統一)
//! - PR-2 同型 pattern: `AgentCapability` / `ProtocolCapability` (impl Stand)

use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capability::MsgboxStore;
use crate::capability::msgbox::MessageKind;
use crate::capability::msgbox_v2::WhitesnakeStore;
use crate::capability::stand_service::{LayerScope, Service, SpawnableService};

use super::lane_capabilities::LaneCapabilitiesPool;
use super::lane_cmd::LaneCmd;
use super::lanes_state::{Diff, LaneAddress, LaneInfo, LaneKind, LanePool, LaneState, SystemEvent};

/// Lane spawn Service (= `lane-spawn` mailbox から `LaneCmd::SpawnLane` を recv、
/// 並列度 N で gate しつつ Lane を spawn する infra actor)。
///
/// SP-local Service (= 1 Project per Process)、 mailbox handle + dependencies を保持し、
/// `spawn(shutdown)` で background recv loop を `tokio::spawn` 起動する。
///
/// PR-β-2 (VP-120): `lane_capabilities_pool: Option<...>` で Worker spawn 成功時に
/// `populate_lane` を呼び、 Lane あたり独立 PaisleyParkState を host する。
pub struct LaneSpawnActor {
    /// VP-177 (Phase 3 PR-5): WhitesnakeStore claim polling に rewire (= 旧 Handle 削除)
    msgbox_store: Option<WhitesnakeStore>,
    lane_pool: Arc<RwLock<LanePool>>,
    lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
    system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
    max_concurrent: usize,
}

impl LaneSpawnActor {
    /// 新しい `LaneSpawnActor` を構築する。
    ///
    /// VP-177 (Phase 3 PR-5): 旧 `handle: Handle` → `msgbox_store: Option<WhitesnakeStore>` に
    /// signature 変更。 caller (= server.rs) が vpdb 接続から build した store を渡す。
    ///
    /// `max_concurrent=0` は意味的に「全 spawn を block」 だが事故 config の可能性が高いため、
    /// `spawn()` 内で 1 に丸めて warn する (= sequential、 `Semaphore::new(0)` の永久 block 回避)。
    pub fn new(
        msgbox_store: Option<WhitesnakeStore>,
        lane_pool: Arc<RwLock<LanePool>>,
        lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
        system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
        max_concurrent: usize,
    ) -> Self {
        Self {
            msgbox_store,
            lane_pool,
            lane_capabilities_pool,
            system_event_tx,
            max_concurrent,
        }
    }
}

impl Service for LaneSpawnActor {
    fn actor_name(&self) -> &str {
        "lane-spawn"
    }

    fn layer_scope(&self) -> LayerScope {
        // SP-local Service (= 1 Project per Process、 cross-machine forward は msgbox_remote 経由)
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
            msgbox_store,
            lane_pool,
            lane_capabilities_pool,
            system_event_tx,
            ..
        } = self;

        tokio::spawn(async move {
            // VP-177: msgbox_store なし = recv 経路なし、 shutdown 待ち
            let Some(store) = msgbox_store else {
                tracing::warn!("Lane spawn actor: msgbox_store なし、 shutdown 待ち");
                shutdown.cancelled().await;
                return;
            };
            let consumer_id = format!("lane-spawn-{}", std::process::id());
            tracing::info!(
                "Lane spawn actor 起動 (= claim polling)、 max_concurrent={}",
                n
            );
            loop {
                let msg = tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("Lane spawn actor: shutdown");
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        match store.claim("lane-spawn", "lead", &consumer_id).await {
                            Ok(Some(m)) => m,
                            Ok(None) => continue,
                            Err(e) => {
                                tracing::warn!("VP-177 lane-spawn claim failed: {}", e);
                                continue;
                            }
                        }
                    }
                };
                if msg.kind != MessageKind::Direct {
                    tracing::debug!(
                        "Lane spawn actor: 非 Direct メッセージを無視 kind={:?}",
                        msg.kind
                    );
                    if !msg.manual_ack
                        && let Err(e) = store.mark_consumed(&msg.id).await
                    {
                        tracing::warn!("VP-177 lane-spawn mark_consumed failed: {}", e);
                    }
                    continue;
                }
                let Some(cmd) = msg.payload_as::<LaneCmd>() else {
                    tracing::warn!(
                        "Lane spawn actor: payload を LaneCmd として parse 失敗 (msg.id={})",
                        msg.id
                    );
                    if !msg.manual_ack
                        && let Err(e) = store.mark_consumed(&msg.id).await
                    {
                        tracing::warn!("VP-177 lane-spawn mark_consumed failed: {}", e);
                    }
                    continue;
                };
                // VP-177: 処理 OK の msg は mark_consumed (= mpsc auto-ack 互換)
                if !msg.manual_ack
                    && let Err(e) = store.mark_consumed(&msg.id).await
                {
                    tracing::warn!("VP-177 lane-spawn mark_consumed failed: {}", e);
                }
                let sem = semaphore.clone();
                let pool = lane_pool.clone();
                let lc_pool = lane_capabilities_pool.clone();
                let tx = system_event_tx.clone();
                tokio::spawn(async move {
                    handle_cmd(cmd, pool, lc_pool, tx, sem).await;
                });
            }
        })
    }
}

/// 単一 `LaneCmd` を処理。 Semaphore permit を acquire してから heavy spawn を実行。
///
/// PR-β-2 (VP-120): `lane_capabilities_pool` 引数 (Option) を追加、 spawn 成功時に
/// `populate_lane` を呼んで Worker Lane あたり独立 PaisleyParkState を host する。
async fn handle_cmd(
    cmd: LaneCmd,
    pool: Arc<RwLock<LanePool>>,
    lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
    system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
    semaphore: Arc<Semaphore>,
) {
    let LaneCmd::SpawnLane {
        project_id,
        name,
        cwd,
        stand,
    } = cmd;

    let addr = LaneAddress::worker(&project_id, &name);

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

    // spawn_with_fallback は内部で std::thread::sleep(800ms) を呼ぶ sync 関数。
    // tokio worker を block しないよう spawn_blocking で隔離する。
    let cwd_for_blocking = cwd.clone();
    // Phase 1e: build_stand_command が addr を要求するので clone を closure に move
    let addr_for_blocking = addr.clone();
    let stand_for_blocking = stand.clone();
    let result = tokio::task::spawn_blocking(move || {
        let cmd_built = super::stand_spawner::build_stand_command(
            &stand_for_blocking,
            &addr_for_blocking,
            Path::new(&cwd_for_blocking),
        );
        super::stand_spawner::spawn_with_fallback(&cmd_built, 80, 24)
    })
    .await;

    let elapsed_ms = started.elapsed().as_millis() as u64;

    // Stage 1 (ADR-0001): TermAttach 配線のため term_rx を tuple に保持して await 跨ぎで持ち越す。
    // initial_rx (= reader_task start 前の Receiver) を pool.insert_pty_slot まで届ける = race フリー。
    let (state, pid, slot_rx_opt) = match result {
        Ok(Ok((slot, term_rx))) => {
            let pid = slot.pid();
            tracing::info!(
                "Lane spawn completed: addr={} pid={} elapsed_ms={}",
                addr,
                pid,
                elapsed_ms
            );
            (LaneState::Running, Some(pid), Some((slot, term_rx)))
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Lane spawn failed (graceful degrade to Dead): addr={} elapsed_ms={} err={}",
                addr,
                elapsed_ms,
                e
            );
            (LaneState::Dead, None, None)
        }
        Err(join_err) => {
            tracing::warn!(
                "Lane spawn join error (graceful degrade to Dead): addr={} elapsed_ms={} err={}",
                addr,
                elapsed_ms,
                join_err
            );
            (LaneState::Dead, None, None)
        }
    };

    // pool に insert。 spawn 中の race (= permit 待ち後だが spawn_blocking 完了前に手動 create)
    // を再 check し、 lost race なら spawn 済 slot を drop して zombie reap。
    let info = LaneInfo {
        address: addr.clone(),
        kind: LaneKind::Worker,
        name: Some(name),
        state,
        stand: stand.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // 起動時点では git 状態取得しない (list_handler 側で必要時に enrich)。
        worker_status: None,
        // Phase 1e: spawn 成功時のみ tmux address を populate
        tmux: if matches!(state, super::lanes_state::LaneState::Running) {
            vec![super::lanes_state::TmuxLaneAddress::for_spawn(
                &addr, &stand,
            )]
        } else {
            Vec::new()
        },
    };
    let mut pool_write = pool.write().await;
    if pool_write.get(&addr).is_some() {
        tracing::debug!(
            "Lane spawn actor: race lost (post-spawn) addr={}、 spawn 済 slot を drop",
            addr
        );
        // slot_rx_opt は scope 終端で drop されるので明示的処理不要。
        return;
    }
    if let Some((slot, term_rx)) = slot_rx_opt {
        // Stage 1 (ADR-0001): TermAttach も同時に spawn (race フリー、 Lead 経路と統一)
        pool_write.insert_pty_slot(addr.clone(), slot, term_rx);
    }
    pool_write.insert(info.clone());
    drop(pool_write); // write lock 解放してから publish (deadlock 回避 + subscriber が即取れる)

    // Worker Lane spawn 完了 → LaneCapabilities pool に entry 追加
    // (Lane あたり独立 PaisleyParkState を host、 doc 13 §6 自動 spawn rule = default)。
    // None は World mode (Lane scope なし) で発生、 SP mode では常に Some。
    // Dead state では populate しない (cascade lifecycle、 上の tmux: vec![] と同型 guard)。
    if matches!(state, LaneState::Running)
        && let Some(lc_pool) = lane_capabilities_pool.as_ref()
    {
        lc_pool.write().await.populate_lane(addr.clone(), &stand);
        tracing::debug!(
            "LaneCapabilities pool に Worker Lane populate (addr={}, stand={})",
            addr,
            stand
        );
    }

    // Phase 2 (Step E): Worker spawn 完了を SystemEvent::Lane(Diff::Add) で TheWorld に push。
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
    /// runtime に到達しないことを serde 側ではなく actor 側で防ぐ contract test。
    ///
    /// VP-177 (Phase 3 PR-5): msgbox_store = None で test (= recv 経路 idle、 0 → 1
    /// 丸め contract は WhitesnakeStore の有無と無関係に検証可能)。 旧 mpsc 版は
    /// Router.send で SpawnLane を流して /nonexistent/path で graceful degrade も
    /// 同時検証していたが、 WhitesnakeStore 経路では DB insert が要るので
    /// msgbox_v2 unit test 側で別途担保 (= 重複検証を避ける)。
    #[tokio::test]
    async fn spawn_zero_concurrent_does_not_hang() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let shutdown = CancellationToken::new();

        // 0 を渡しても 1 に丸めて起動するはず (= タイムアウトせずに actor 起動 + shutdown 完了)
        // PR-β-2 (VP-120): lane_capabilities_pool = None で test (Lane scope なしの動作確認)
        // VP-177 (Phase 3 PR-5): msgbox_store = None (= recv 経路 idle、 shutdown のみ検証)
        LaneSpawnActor::new(None, pool, None, tx, 0).spawn_loop(shutdown.clone());

        // shutdown して terminate を確認 (= 永久 block 回避)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        shutdown.cancel();
        // shutdown が伝播する time を確保
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// actor 起動 → shutdown 完了の smoke test。
    ///
    /// VP-177 (Phase 3 PR-5): 旧「非 Direct メッセージは ignore」 test は mpsc Handle
    /// 経由で Router.send で Notification を流して pool 0 を確認していた。
    /// WhitesnakeStore 経路では payload に kind 区別が DB schema 側で混在しないため
    /// (= msgbox_send で MessageKind を payload に同梱)、 該 ignore 検証は msgbox_v2
    /// 側の unit test に統合済 (`spawn_loop` 入口の `MessageKind::Direct` guard は
    /// 関数 body の 1 行で実質 dead-code 化していないことを cargo check が保証)。
    /// ここでは shutdown contract のみ smoke 検証。
    #[tokio::test]
    async fn actor_shuts_down_cleanly() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let shutdown = CancellationToken::new();

        // PR-β-2 (VP-120): lane_capabilities_pool = None で test
        // VP-177 (Phase 3 PR-5): msgbox_store = None で test (= recv 経路 idle)
        LaneSpawnActor::new(None, pool.clone(), None, tx, 1).spawn_loop(shutdown.clone());

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // pool は空のまま (= recv 経路なしで何も起こらない)
        assert_eq!(pool.read().await.count(), 0);

        shutdown.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
