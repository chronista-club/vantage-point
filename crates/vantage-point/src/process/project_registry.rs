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

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::state::AppState;
use crate::protocol::DebugMode;

/// project 1 件分の実行状態と、その停止スイッチ。
// 未結線（doc 44 P1 fold-in の建材）: 次 commit で World の `start_process` /
// `forward_to_sp_control` を本 registry に載せ替えると同時に本 allow を外す。
// CI は `clippy -D warnings` なので、それまでの間だけ明示的に黙らせる。
#[allow(dead_code)]
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
#[allow(dead_code)] // 未結線（上記 `ProjectRuntime` と同じ理由）
pub(crate) struct ProjectRuntimes {
    inner: RwLock<HashMap<String, ProjectRuntime>>,
}

#[allow(dead_code)] // 未結線（上記 `ProjectRuntime` と同じ理由）
impl ProjectRuntimes {
    pub fn new() -> Self {
        Self::default()
    }

    /// project を in-process で起動して登録する。
    ///
    /// 既に起動済みなら **no-op で `Ok(false)`**（旧 `start_process` の dedup 相当。
    /// プロセスが無くなったので「重複 spawn」は map への二重 insert として自然に防げる）。
    /// 新規起動できたら `Ok(true)`。
    pub async fn start(&self, project_dir: &str, debug_mode: DebugMode) -> Result<bool> {
        let key = crate::capability::normalize_path_key(std::path::Path::new(project_dir));

        // 二重起動の早期棄却。 起動には時間がかかるので、まず read lock だけで判定する。
        if self.inner.read().await.contains_key(&key) {
            return Ok(false);
        }

        let shutdown = CancellationToken::new();
        let cap_config = super::capabilities::CapabilityConfig {
            project_dir: project_dir.to_string(),
        };
        // port はもう bind されない（SP-portless の遺産）。fold-in で概念ごと消えるため 0 を渡す。
        let state =
            super::server::start_project(0, debug_mode, cap_config, shutdown.clone()).await?;

        // 起動中に別 caller が同 project を起こしていた場合はこちらを捨てる（後勝ちにしない）。
        let mut guard = self.inner.write().await;
        if guard.contains_key(&key) {
            drop(guard);
            shutdown.cancel();
            super::server::shutdown_project(&state).await;
            return Ok(false);
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

    /// 登録済 project の `AppState` を引く。
    pub async fn get(&self, path_key: &str) -> Option<Arc<AppState>> {
        self.inner
            .read()
            .await
            .get(path_key)
            .map(|r| r.state.clone())
    }

    /// 稼働中 project の path_key 一覧。
    pub async fn keys(&self) -> Vec<String> {
        self.inner.read().await.keys().cloned().collect()
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
    async fn empty_registry_has_no_keys() {
        let runtimes = ProjectRuntimes::new();
        assert!(runtimes.keys().await.is_empty());
        assert!(runtimes.get("/anything").await.is_none());
    }
}
