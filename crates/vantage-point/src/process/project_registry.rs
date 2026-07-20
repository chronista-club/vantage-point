//! World が抱える per-project 実行状態の registry（doc 44 P1 = SP fold-in）。
//!
//! # 位置付け
//!
//! 旧構成では project 1 件 = SP プロセス 1 本で、World は QUIC registry / control /
//! canvas-ingest の 3 channel を張って外から操作していた。fold-in はこのプロセス境界を
//! 取り払い、project を **World プロセス内の `Arc<AppState>` 1 個**に降格させる。
//! 本 registry がその map の実体で、旧 `running_processes` + `control_channels` の
//! 役割を 1 つにまとめて引き継ぐ。
//!
//! doc 44 D2 が言う「project は認知境界に退化する」はここで達成される — project は
//! もはやプロセスでも actor でもなく、**この HashMap のエントリ**でしかない。
//!
//! # なぜ AppState を 1 枚に畳まないのか
//!
//! `LanePool` は key が `LaneAddress { project, kind, name }` なので原理的には全 project
//! 分を 1 枚に merge できる。ただしそれをやると `AppState.project_dir` の単一前提が壊れ、
//! `dispatch_process_method` の signature 変更 → 約 50 箇所のテスト改修に波及する。
//! 一方 fold-in の目的（World↔SP 配管の撤去・`vp sp` 退役・spine 三段→二段）は
//! **プロセス境界を消すだけで達成される**ため、1 枚化は分離して doc 44 P2
//! （`LaneAddress` フラット化）の担当とした。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::state::AppState;
use crate::protocol::DebugMode;

/// project 1 件分の実行状態と、その停止スイッチ。
pub(crate) struct ProjectRuntime {
    pub state: Arc<AppState>,
    /// cancel すると当該 project の spawn 済 task 群（lane monitor / snapshot publish 等）が止まる。
    pub shutdown: CancellationToken,
}

/// path_key → [`ProjectRuntime`] の map。
///
/// key は [`normalize_path_key`](crate::capability::normalize_path_key) 正規化済パス
/// （旧 `running_processes` / `lane_registry` / `control_channels` と同じ規約）。
#[derive(Default)]
pub(crate) struct ProjectRuntimes {
    inner: RwLock<HashMap<String, ProjectRuntime>>,
    /// World の lane 集約 view（`ProcessManagerCapability::lane_registry` と同一 Arc）。
    ///
    /// doc 44 P1 (fold-in): 旧構成では SP が QUIC "lanes" channel で World へ register
    /// snapshot を push し、この view を最新化していた。SP が消えた今、更新役は
    /// project 自身の publish task が引き継ぐ（[`start`] が本 Arc を渡す）。
    /// `None` は「World 以外の文脈」= test / SP 単体起動で、その場合 view は存在しない。
    world_lanes: Option<super::server::WorldLaneView>,
    /// shutdown 開始後に新規登録を受け付けないための門。
    ///
    /// [`shutdown_all`](Self::shutdown_all) は map を drain して停止するが、drain の**後**に
    /// 進行中の [`start`](Self::start) が insert を完了させると、その project の spawn 済 task と
    /// SurrealDB handle が「World stopped」ログの後も生き残る（= プロセスが終了できない）。
    ///
    /// 起動の入口は複数あり、いずれも World の shutdown 手続きの射程外で走る:
    ///   - `autostart_enabled_projects`（spawn した JoinHandle を保持していない）
    ///   - `projects/start` RPC（unison が接続ごとに独立 task で handler を回すため、
    ///     accept loop を abort しても既存接続の in-flight handler には波及しない）
    ///
    /// task の abort では解決できない: `start_project` の await 途中で future を drop すると、
    /// その時点で既に spawn 済みの内部 task が孤児として残り、防ぎたい状態そのものになる。
    /// よって「進行中の起動は完走させ、登録直前に自己回収させる」設計を取る。
    closing: AtomicBool,
}

impl ProjectRuntimes {
    pub fn new() -> Self {
        Self::default()
    }

    /// World の lane 集約 view を結線した registry を作る。
    ///
    /// World bootstrap 専用。これを渡さないと project を起こしても World の view が
    /// 更新されず、`vp ps` / sidebar / `/api/world/lanes` が boot 時の db 値で固まる。
    pub fn with_lane_view(world_lanes: super::server::WorldLaneView) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            world_lanes: Some(world_lanes),
            closing: AtomicBool::new(false),
        }
    }

    /// project を in-process で起動して登録する。
    ///
    /// 既に起動済みなら **no-op で `Ok(false)`**（旧 `start_process` の dedup 相当。
    /// プロセスが無くなったので「重複 spawn」は map への二重 insert として自然に防げる）。
    /// 新規起動できたら `Ok(true)`。
    ///
    /// shutdown 開始後は `Err` を返す（[`closing`](Self::closing) 参照）。`Ok(false)` に
    /// しないのは、caller の `start_process` が「false = 既に起動済み」と解釈して
    /// `running_processes` / presence に **動いていない project を登録してしまう**ため。
    pub async fn start(&self, project_dir: &str, debug_mode: DebugMode) -> Result<bool> {
        let key = crate::capability::normalize_path_key(std::path::Path::new(project_dir));

        if self.closing.load(Ordering::Acquire) {
            anyhow::bail!("World が shutdown 中のため project を起動しない (key={key})");
        }

        // 二重起動の早期棄却。 起動には時間がかかるので、まず read lock だけで判定する。
        if self.inner.read().await.contains_key(&key) {
            return Ok(false);
        }

        let shutdown = CancellationToken::new();
        let cap_config = super::capabilities::CapabilityConfig {
            project_dir: project_dir.to_string(),
        };
        // port はもう bind されない（SP-portless の遺産）。fold-in で概念ごと消えるため 0 を渡す。
        let state = super::server::start_project(
            0,
            debug_mode,
            cap_config,
            shutdown.clone(),
            self.world_lanes.clone(),
        )
        .await?;

        // 起動中に別 caller が同 project を起こしていた場合はこちらを捨てる（後勝ちにしない）。
        let mut guard = self.inner.write().await;
        if guard.contains_key(&key) {
            drop(guard);
            shutdown.cancel();
            super::server::shutdown_project(&state).await;
            return Ok(false);
        }
        // 起動している間に shutdown が始まっていたら、ここで自己回収する。
        // `shutdown_all` は closing を立ててから drain するので、この recheck を write lock 内で
        // 行うことで「drain の後に insert が滑り込む」窓が閉じる（漏れると当該 project の
        // task と db handle が残り、プロセスが終了できなくなる）。
        if self.closing.load(Ordering::Acquire) {
            drop(guard);
            shutdown.cancel();
            super::server::shutdown_project(&state).await;
            anyhow::bail!("起動中に World の shutdown が始まったため巻き戻した (key={key})");
        }
        guard.insert(key, ProjectRuntime { state, shutdown });
        Ok(true)
    }

    /// project を停止して登録解除する。停止対象が居なければ `false`。
    pub async fn stop(&self, path_key: &str) -> bool {
        let Some(rt) = self.inner.write().await.remove(path_key) else {
            return false;
        };
        rt.shutdown.cancel();
        super::server::shutdown_project(&rt.state).await;
        true
    }

    /// 登録済 project を**すべて**停止する（World の graceful shutdown 用）。戻り値は停止数。
    ///
    /// doc 44 P1 (fold-in): 旧構成では project = 別プロセス (SP) だったため、daemon が
    /// 落ちても project は生き残るのが**設計上の正**だった（`vp daemon stop` の gentle 挙動）。
    /// project が World 内の `Arc<AppState>` になった今、この前提は反転する — project は
    /// World の tokio task と SurrealDB handle でしかないので、**World が畳まなければ
    /// 誰も畳まない**。
    ///
    /// これを怠ると World は listener を閉じて「停止」を名乗るのにプロセスが終了できず、
    /// project db の LOCK を握り続ける。次に起動した daemon はその LOCK を見て
    /// 「重複 spawn」と誤検出し、**全 project の起動に失敗する**（実機で観測済み）。
    pub async fn shutdown_all(&self) -> usize {
        // drain より先に受付を閉じる。この順序が要点で、逆にすると「drain 済みの map へ
        // 進行中の start が insert を完了させる」窓が残る（= 畳んだはずの project が生き残る）。
        self.closing.store(true, Ordering::Release);
        // 先に map を空にしてから停止する（停止の await 中に別 caller が同じ project を
        // 掴んで二重に畳むのを防ぐ。`stop()` の remove-then-shutdown と同じ順序）。
        let drained: Vec<(String, ProjectRuntime)> = {
            let mut guard = self.inner.write().await;
            guard.drain().collect()
        };
        let count = drained.len();
        for (key, rt) in drained {
            rt.shutdown.cancel();
            super::server::shutdown_project(&rt.state).await;
            tracing::info!("project 停止 (key={})", key);
        }
        count
    }

    /// 登録済 project の `AppState` を引く。
    pub async fn get(&self, path_key: &str) -> Option<Arc<AppState>> {
        self.inner
            .read()
            .await
            .get(path_key)
            .map(|r| r.state.clone())
    }

    /// 当該 project の [`dispatch_process_method`] を **直接呼ぶ**。
    ///
    /// 旧 `forward_to_sp_control`（World → QUIC control channel → SP）の後継。
    /// 戻り値の形（成功は生の JSON、失敗は `{"error": ...}`）は caller が
    /// `send_response` でそのまま client に relay するため互換に保つ。
    ///
    /// [`dispatch_process_method`]: super::unison_server::dispatch_process_method
    pub async fn dispatch(
        &self,
        path_key: &str,
        method: &str,
        payload: &serde_json::Value,
    ) -> serde_json::Value {
        let Some(state) = self.get(path_key).await else {
            return serde_json::json!({
                "error": format!("project 未起動 (key={})", path_key)
            });
        };
        match super::unison_server::dispatch_process_method(&state, method, payload.clone()).await {
            Ok(v) => v,
            Err(e) => serde_json::json!({
                "error": format!("project dispatch 失敗 (key={}): {}", path_key, e)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_on_unknown_project_reports_not_started() {
        let runtimes = ProjectRuntimes::new();
        let res = runtimes
            .dispatch("/no/such/project", "lanes_list", &serde_json::json!({}))
            .await;
        let err = res
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            err.contains("project 未起動"),
            "未起動 project は error JSON を返すべき: {res}"
        );
    }

    #[tokio::test]
    async fn stop_on_unknown_project_is_false() {
        let runtimes = ProjectRuntimes::new();
        assert!(!runtimes.stop("/no/such/project").await);
    }

    #[tokio::test]
    async fn empty_registry_resolves_nothing() {
        let runtimes = ProjectRuntimes::new();
        assert!(runtimes.get("/anything").await.is_none());
    }

    /// shutdown 後の `start` は登録せず Err を返す。
    ///
    /// 回帰固定: ここが `Ok(false)` だと caller の `start_process` が「既に起動済み」と
    /// 解釈して running_processes / presence に動いていない project を載せる。また
    /// `shutdown_all` の drain 後に insert が滑り込むと、その project の task と db handle が
    /// 残ってプロセスが終了できなくなる（World shutdown の 82 分ハングの再現経路）。
    #[tokio::test]
    async fn start_after_shutdown_is_rejected() {
        let runtimes = ProjectRuntimes::new();
        assert_eq!(runtimes.shutdown_all().await, 0);

        let err = runtimes
            .start("/tmp/proj-after-shutdown", DebugMode::None)
            .await
            .expect_err("shutdown 後の start は Err であること");
        assert!(
            err.to_string().contains("shutdown"),
            "shutdown 中である旨が伝わるエラーであること: {err}"
        );
        assert!(
            runtimes.get("/tmp/proj-after-shutdown").await.is_none(),
            "拒否した project は登録されないこと"
        );
    }
}
