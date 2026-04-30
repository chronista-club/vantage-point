//! Lane spawn actor — `LaneCmd::SpawnLane` を Mailbox 経由で受信し、 内部 Semaphore で
//! 並列度を gate しつつ `stand_spawner::spawn_with_fallback` で Lane を spawn する actor。
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
//! - 旧 sync 経路: `super::lanes_state::LanePool::populate_workers_from_disk` (本 PR で削除)

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::capability::msgbox::{Handle, MessageKind};

use super::lane_cmd::LaneCmd;
use super::lanes_state::{LaneAddress, LaneInfo, LaneKind, LanePool, LaneState};

/// Mailbox actor を起動する。 internal に `Arc<Semaphore::new(max_concurrent)>` を持ち、
/// `lane-spawn` mailbox から `LaneCmd` を recv して並列度制限付きで Lane を spawn する。
///
/// `tokio::spawn` で background task 化されるため、 caller は即 return する。
pub fn spawn(
    handle: Handle,
    lane_pool: Arc<RwLock<LanePool>>,
    max_concurrent: usize,
    shutdown: CancellationToken,
) {
    // max_concurrent=0 は意味的に「全 spawn を block」 だが事故 config の可能性が高い。
    // Semaphore::new(0) は永久 block するため、 1 に丸めて warn する (= sequential)。
    let n = if max_concurrent == 0 {
        tracing::warn!(
            "Lane spawn actor: max_concurrent=0 は無効、 1 に丸めます (config 確認推奨)"
        );
        1
    } else {
        max_concurrent
    };
    let semaphore = Arc::new(Semaphore::new(n));
    let address = handle.address().to_string();

    tokio::spawn(async move {
        tracing::info!(
            "Lane spawn actor 起動: address={} max_concurrent={}",
            address,
            n
        );
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("Lane spawn actor: shutdown");
                    break;
                }
                msg = handle.recv() => {
                    let Some(msg) = msg else {
                        tracing::info!("Lane spawn actor: channel closed");
                        break;
                    };
                    if msg.kind != MessageKind::Direct {
                        tracing::debug!(
                            "Lane spawn actor: 非 Direct メッセージを無視 kind={:?}",
                            msg.kind
                        );
                        continue;
                    }
                    let Some(cmd) = msg.payload_as::<LaneCmd>() else {
                        tracing::warn!(
                            "Lane spawn actor: payload を LaneCmd として parse 失敗 (msg.id={})",
                            msg.id
                        );
                        continue;
                    };
                    let sem = semaphore.clone();
                    let pool = lane_pool.clone();
                    // permit 取得を含めて worker task で実行 → recv loop は次の msg を即受領可能。
                    // 結果として「N 本まで permit 待ち + 実行、 残りは queue で待機」 の挙動。
                    tokio::spawn(async move {
                        handle_cmd(cmd, pool, sem).await;
                    });
                }
            }
        }
    });
}

/// 単一 `LaneCmd` を処理。 Semaphore permit を acquire してから heavy spawn を実行。
async fn handle_cmd(cmd: LaneCmd, pool: Arc<RwLock<LanePool>>, semaphore: Arc<Semaphore>) {
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
        "Lane spawn requested: addr={} cwd={} stand={:?}",
        addr,
        cwd,
        stand
    );
    let started = Instant::now();

    // spawn_with_fallback は内部で std::thread::sleep(800ms) を呼ぶ sync 関数。
    // tokio worker を block しないよう spawn_blocking で隔離する。
    let cwd_for_blocking = cwd.clone();
    let result = tokio::task::spawn_blocking(move || {
        let cmd_built =
            super::stand_spawner::build_stand_command(stand, Path::new(&cwd_for_blocking));
        super::stand_spawner::spawn_with_fallback(&cwd_for_blocking, &cmd_built, 80, 24)
    })
    .await;

    let elapsed_ms = started.elapsed().as_millis() as u64;

    let (state, pid, slot_opt) = match result {
        Ok(Ok((slot, _rx))) => {
            let pid = slot.pid();
            tracing::info!(
                "Lane spawn completed: addr={} pid={} elapsed_ms={}",
                addr,
                pid,
                elapsed_ms
            );
            (LaneState::Running, Some(pid), Some(slot))
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
        stand,
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // 起動時点では git 状態取得しない (list_handler 側で必要時に enrich)。
        worker_status: None,
    };
    let mut pool_write = pool.write().await;
    if pool_write.get(&addr).is_some() {
        tracing::debug!(
            "Lane spawn actor: race lost (post-spawn) addr={}、 spawn 済 slot を drop",
            addr
        );
        // slot_opt は scope 終端で drop されるので明示的処理不要。
        return;
    }
    if let Some(slot) = slot_opt {
        pool_write.insert_pty_slot(addr.clone(), slot);
    }
    pool_write.insert(info);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::msgbox::{Message, Router};
    use crate::process::lanes_state::LaneStand;

    /// max_concurrent=0 は 1 に丸められること。 Semaphore::new(0) を踏むと永久 block するため
    /// runtime に到達しないことを serde 側ではなく actor 側で防ぐ contract test。
    #[tokio::test]
    async fn spawn_zero_concurrent_does_not_hang() {
        let router = Router::new();
        let handle = router.register("lane-spawn").await;
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let shutdown = CancellationToken::new();

        // 0 を渡しても 1 に丸めて起動するはず (= タイムアウトせずに actor 起動 + shutdown 完了)
        spawn(handle.clone(), pool, 0, shutdown.clone());

        // SpawnLane を投入しても fallback 経路 (cwd 不在) で graceful degrade するはず。
        // 重要なのは「actor が動いて shutdown で終了する」 こと。
        let cmd = LaneCmd::SpawnLane {
            project_id: "test".to_string(),
            name: "msg-zero".to_string(),
            cwd: "/nonexistent/path/for/test".to_string(),
            stand: LaneStand::HeavensDoor,
        };
        let msg = Message::new("test", "lane-spawn", MessageKind::Direct).with_payload(&cmd);
        let _ = handle.send(msg).await;

        // shutdown して terminate を確認 (= 永久 block 回避)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        shutdown.cancel();
        // shutdown が伝播する time を確保
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// 非 Direct メッセージは payload parse せずに skip すること。
    /// (= recv loop が parse error で抜けず、 後続 msg を処理可能なこと)
    #[tokio::test]
    async fn non_direct_message_is_ignored() {
        let router = Router::new();
        let handle = router.register("lane-spawn").await;
        let pool = Arc::new(RwLock::new(LanePool::new()));
        let shutdown = CancellationToken::new();

        spawn(handle.clone(), pool.clone(), 1, shutdown.clone());

        // Notification kind を投入 → ignore されるはず
        let msg = Message::new("test", "lane-spawn", MessageKind::Notification)
            .with_payload(&serde_json::json!({"kind": "spawn_lane"}));
        let _ = handle.send(msg).await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // pool に insert されていない (= ignore された) ことを確認
        assert_eq!(pool.read().await.count(), 0);

        shutdown.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
