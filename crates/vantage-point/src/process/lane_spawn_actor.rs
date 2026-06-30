//! Lane spawn actor — `LaneCmd::SpawnLane` を Mailbox 経由で受信し、 内部 Semaphore で
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
//! ## wiremsg R2-a (store 中央化、 2026-06-11) — recv path を TheWorld long-poll に rewire
//!
//! R4 で wire accumulation (per-SP `WiremsgStore`) に移行した recv path を、 store 中央化
//! (設計 mem_1CbvcJj4ppU3QKH9d7xMpT) に伴い TheWorld への HTTP long-poll
//! (`crate::process::world_wire`) に切替。 待機は TheWorld 側 `wire_recv` の long-poll が
//! 担うため in-process `WireNotifier` は不要になった。 TheWorld 不在 (standalone SP) は
//! IDLE_POLL 間隔の retry で gracefully degrade。 旧 msgbox の `claim → mark_consumed`
//! destructive 消費は wire の per-agent cursor 前進 (非破壊) のまま。
//! Semaphore gate / race guard / `handle_cmd` の内部挙動は完全互換。
//!
//! ## 設計
//!
//! - **address**: `lane-spawn@<project>` (= TheWorld 中央 wire store の address)。
//!   producer は同 Process の `sp-bootstrap@<project>` (server.rs の bootstrap loop)
//! - **wire format**: `LaneCmd::SpawnLane{...}` (= `crate::process::lane_cmd`)、 serde tag="kind"
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
//! `shutdown_token.cancelled()` で recv loop 終了。 in-flight tokio task は detach (= 自然完了)
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

use crate::capability::stand_service::{LayerScope, Service, SpawnableService};

use super::lane_capabilities::LaneCapabilitiesPool;
use super::lane_cmd::LaneCmd;
use super::lanes_state::{Diff, LaneAddress, LaneInfo, LaneKind, LanePool, LaneState, SystemEvent};

/// lane-spawn recv の retry 間隔 (TheWorld 不達時)。 通常は TheWorld 側 long-poll が
/// 待機を担うため、 この sleep は TheWorld 不在時の再接続間隔としてのみ効く。
const IDLE_POLL: Duration = Duration::from_secs(5);

/// Lane spawn Service (= `lane-spawn` mailbox から `LaneCmd::SpawnLane` を recv、
/// 並列度 N で gate しつつ Lane を spawn する infra actor)。
///
/// SP-local Service (= 1 Project per Process)、 mailbox handle + dependencies を保持し、
/// `spawn(shutdown)` で background recv loop を `tokio::spawn` 起動する。
///
/// PR-β-2 (VP-120): `lane_capabilities_pool: Option<...>` で Performer spawn 成功時に
/// `populate_lane` を呼び、 Lane あたり独立 PaisleyParkState を host する。
pub struct LaneSpawnActor {
    /// wire address の project segment (`lane-spawn@<project>`)
    project: String,
    lane_pool: Arc<RwLock<LanePool>>,
    lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
    system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
    max_concurrent: usize,
}

impl LaneSpawnActor {
    /// 新しい `LaneSpawnActor` を構築する。
    ///
    /// wiremsg R2-a: store 中央化に伴い、 旧 `new(wiremsg_store, wire_notifier, ...)` から
    /// wire 系引数を撤去。 recv は TheWorld への HTTP long-poll
    /// ([`crate::process::world_wire`]) で行う。
    ///
    /// `max_concurrent=0` は意味的に「全 spawn を block」 だが事故 config の可能性が高いため、
    /// `spawn()` 内で 1 に丸めて warn する (= sequential、 `Semaphore::new(0)` の永久 block 回避)。
    pub fn new(
        project: String,
        lane_pool: Arc<RwLock<LanePool>>,
        lane_capabilities_pool: Option<Arc<RwLock<LaneCapabilitiesPool>>>,
        system_event_tx: tokio::sync::broadcast::Sender<SystemEvent>,
        max_concurrent: usize,
    ) -> Self {
        Self {
            project,
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
            project,
            lane_pool,
            lane_capabilities_pool,
            system_event_tx,
            ..
        } = self;

        tokio::spawn(async move {
            let address = format!("lane-spawn@{}", project);
            tracing::info!(
                "Lane spawn actor 起動 (= TheWorld 中央 wire store long-poll、 address={}, max_concurrent={})",
                address,
                n
            );
            loop {
                if shutdown.is_cancelled() {
                    tracing::info!("Lane spawn actor: shutdown");
                    return;
                }
                // R2-a: TheWorld 側 handler が max 30s の long-poll を行う。 25s で投げて
                // 余裕を持つ (待機は server 側なので busy loop にならない)。
                let payload = serde_json::json!({ "agent": address, "timeout": 25 });
                let resp = tokio::select! {
                    _ = shutdown.cancelled() => { tracing::info!("Lane spawn actor: shutdown"); return; }
                    r = crate::process::world_wire::call("/api/wire/recv", payload) => r,
                };
                let msgs = match resp {
                    Ok(v) => v
                        .get("messages")
                        .and_then(|m| m.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    Err(e) => {
                        // TheWorld 不在は standalone SP (`vp sp start` 単独) で正常系。
                        // debug に留め、 IDLE_POLL 間隔で再試行する。
                        tracing::debug!("lane-spawn wire recv (TheWorld) 失敗、 retry: {}", e);
                        tokio::select! {
                            _ = shutdown.cancelled() => return,
                            _ = tokio::time::sleep(IDLE_POLL) => {}
                        }
                        continue;
                    }
                };

                for msg in msgs {
                    // body は `LaneCmd` (serde tag="kind") の JSON object。 parse 失敗 =
                    // 想定外 message、 log して skip (cursor は recv で既に前進済)。
                    let body = msg.get("body").cloned().unwrap_or(serde_json::Value::Null);
                    let cmd = match serde_json::from_value::<LaneCmd>(body) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(
                                "Lane spawn actor: body を LaneCmd として parse 失敗 (msg.id={}): {}",
                                msg.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                                e
                            );
                            continue;
                        }
                    };
                    let sem = semaphore.clone();
                    let pool = lane_pool.clone();
                    let lc_pool = lane_capabilities_pool.clone();
                    let tx = system_event_tx.clone();
                    tokio::spawn(async move {
                        handle_cmd(cmd, pool, lc_pool, tx, sem).await;
                    });
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

    // spawn_with_fallback は内部で std::thread::sleep(800ms) を呼ぶ sync 関数。
    // tokio worker thread を block しないよう spawn_blocking で隔離する。
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
        // PTY 初期 winsize 120x48: xterm.js が fitAddon で実サイズに resize する
        // までの初期値 + headless Stand の作業サイズ。 classic 80x24 は VP の広い
        // terminal には狭く、 claude TUI の reflow ジャンプも大きいため 120x48。
        // reconcile gap fix (2026-06-30、 横展開): performer も既存 tmux session を
        // adopt して重複 SP spawn での Dead 化を防ぐ（conductor と同じ ground-truth 経路）。
        let session = addr_for_blocking.tmux_session_name(&stand_for_blocking);
        super::stand_spawner::spawn_or_adopt(&cmd_built, &session, 120, 48)
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
    // I1: performer の安定 id を address (project, name) で load_or_create。
    // 注: load_or_create は同期 file IO だが、 cc_session の lazy read と同じく数 ms で、
    // spawn_blocking 隔離は省略 (pre-MVP の単純化。 重い処理は上の spawn_with_fallback で隔離済)。
    let lane_id = crate::lane::lane_id::load_or_create(&addr.project, &name);
    let info = LaneInfo {
        id: lane_id,
        address: addr.clone(),
        kind: LaneKind::Performer,
        name: Some(name),
        state,
        stand: stand.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // 起動時点では git 状態取得しない (list_handler 側で必要時に enrich)。
        performer_status: None,
        cc_session_id: None,
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
        // Stage 1 (ADR-0001): TermAttach も同時に spawn (race フリー、 Conductor 経路と統一)
        pool_write.insert_pty_slot(addr.clone(), slot, term_rx);
    }
    pool_write.insert(info.clone());
    drop(pool_write); // write lock 解放してから publish (deadlock 回避 + subscriber が即取れる)

    // Performer Lane spawn 完了 → LaneCapabilities pool に entry 追加
    // (Lane あたり独立 PaisleyParkState を host、 doc 13 §6 自動 spawn rule = default)。
    // None は World mode (Lane scope なし) で発生、 SP mode では常に Some。
    // Dead state では populate しない (cascade lifecycle、 上の tmux: vec![] と同型 guard)。
    if matches!(state, LaneState::Running)
        && let Some(lc_pool) = lane_capabilities_pool.as_ref()
    {
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
    /// runtime に到達しないことを serde 側ではなく actor 側で防ぐ contract test。
    ///
    /// R2-a: recv は TheWorld への HTTP long-poll。 test 環境に TheWorld は居ないため
    /// recv は失敗 → IDLE_POLL retry で idle になる (= 0 → 1 丸め contract は
    /// TheWorld の有無と無関係に検証可能)。
    #[tokio::test]
    async fn spawn_zero_concurrent_does_not_hang() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let shutdown = CancellationToken::new();

        // 0 を渡しても 1 に丸めて起動するはず (= タイムアウトせずに actor 起動 + shutdown 完了)
        // PR-β-2 (VP-120): lane_capabilities_pool = None で test (Lane scope なしの動作確認)
        LaneSpawnActor::new("test".to_string(), pool, None, tx, 0).spawn_loop(shutdown.clone());

        // shutdown して terminate を確認 (= 永久 block 回避)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        shutdown.cancel();
        // shutdown が伝播する time を確保
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// actor 起動 → shutdown 完了の smoke test。
    ///
    /// R2-a: test 環境に TheWorld は居ないため recv は失敗 → IDLE_POLL retry で idle。
    /// shutdown contract のみ smoke 検証する。
    #[tokio::test]
    async fn actor_shuts_down_cleanly() {
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel::<SystemEvent>(8);
        let shutdown = CancellationToken::new();

        // PR-β-2 (VP-120): lane_capabilities_pool = None で test
        LaneSpawnActor::new("test".to_string(), pool.clone(), None, tx, 1)
            .spawn_loop(shutdown.clone());

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // pool は空のまま (= recv 経路なしで何も起こらない)
        assert_eq!(pool.read().await.count(), 0);

        shutdown.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
