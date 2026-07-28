//! daemon control plane クライアント (Unison `daemon-control` / `registry`)
//!
//! doc 45 段 3: vp-app が抱えていた REST client (`client::DaemonRpcClient`) のうち、
//! repos / processes / lanes を触る面をここへ移した。vp-app は既に Unison を
//! 主要な transport として使っている (lanes / canvas / terminal / device / wire / repo-proxy)
//! ので、control plane だけ HTTP に残す理由が無い — Unison 側には KDL schema と drift
//! 検出があり、HTTP 側には無い (doc 45 §1)。
//!
//! 残った HTTP は `/api/health` 1 本だけで、これは doc 45 §2 の設計判断による
//! (health は「他が壊れている時に動いてほしい」probe なので、意図的に鈍い外殻として
//! HTTP のまま置く)。
//!
//! ## 1 RPC = 1 stream
//!
//! daemon 側の daemon-control handler は **1 stream につき逐次** (recv → handle → send を
//! 直列に回す) なので、長い RPC (`repos/restart` は内部に grace sleep + 起動確認が入る) と
//! 5s 周期の poll を同じ stream に相乗りさせると、poll が restart の完了まで待たされる
//! (head-of-line blocking)。call ごとに stream を開いて閉じることで、旧 HTTP の
//! 「1 リクエスト = 1 独立した往復」という性質をそのまま持ち込む。
//!
//! 接続自体は `SharedDaemonConn` の 1 本 (F1b、doc 27 §3.4.4) を共有するので、
//! 増えるのは QUIC stream だけ。stream open は同一 connection 上の 1 往復で、
//! 毎回 connect し直していた旧 `daemon_repo_request` より安い。
//!
//! ⚠️ stream は必ず `close()` する。drop 任せにすると recv task と QUIC stream が残り、
//! MAX_STREAMS 枯渇に効いてくる (`run_lanes_session` が踏んだのと同じ罠)。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::client::{RepoInfo, RunningRepo};

/// 1 RPC の上限。旧 reqwest client の 10s timeout をそのまま引き継ぐ。
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// `repos/restart` だけの上限。daemon 側で stop → grace sleep → start → 起動確認と
/// 繋ぐので他の RPC より桁が違う。旧 HTTP client では 10s を超えると reqwest が
/// 先に諦めて「失敗したのに再起動は進む」という嘘のエラーになっていた。
const RESTART_TIMEOUT: Duration = Duration::from_secs(60);

/// 共有 QUIC connection 上に control 面を張る client。
///
/// `SharedDaemonConn::control()` から得る。connection の再接続は共有 manager が
/// 一手に持っているので、本 struct は「今生きている client で 1 往復する」だけを担う。
#[derive(Clone)]
pub struct DaemonControl {
    client: Arc<unison::ProtocolClient>,
}

impl DaemonControl {
    /// 確立済みの共有 connection から control client を作る。
    pub fn new(client: Arc<unison::ProtocolClient>) -> Self {
        Self { client }
    }

    /// 任意 channel への 1 往復 (stream open → request → close)。
    async fn call_on(
        &self,
        channel_name: &str,
        method: &str,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let channel = tokio::time::timeout(timeout, self.client.open_channel(channel_name))
            .await
            .map_err(|_| anyhow!("{channel_name} channel open: timeout"))?
            .map_err(|e| anyhow!("{channel_name} channel open: {e}"))?;
        // request の結果に関わらず stream は閉じる (早期 return で漏らさない)。
        let result = tokio::time::timeout(
            timeout,
            channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
        )
        .await;
        let _ = channel.close().await;

        let resp = result
            .map_err(|_| anyhow!("{channel_name}.{method}: timeout"))?
            .map_err(|e| anyhow!("{channel_name}.{method}: {e}"))?;
        if let Some(err) = rpc_error(&resp) {
            bail!("{channel_name}.{method}: {err}");
        }
        Ok(resp)
    }

    /// `daemon-control` channel への 1 往復 (既定 timeout)。
    async fn control(&self, method: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
        self.call_on("daemon-control", method, payload, RPC_TIMEOUT)
            .await
    }

    // =====================================================================
    // repos — 旧 `/api/daemon/repos*` / `/api/daemon/processes/*`
    // =====================================================================

    /// 登録 repo 一覧 (旧 `GET /api/daemon/repos`)。
    pub async fn list_repos(&self) -> Result<Vec<RepoInfo>> {
        let resp = self.control("repos/list", serde_json::json!({})).await?;
        decode_repos(resp)
    }

    /// 稼働中 repo の snapshot (旧 `GET /api/daemon/processes`)。
    ///
    /// 面は `registry` channel。daemon は `running_repos` map を HTTP route と
    /// **同じ Arc** で共有しているので、どちらから読んでも同じ答えになる
    /// (parity テスト: `daemon/server.rs` の `registry_list_matches_http`)。
    pub async fn list_processes(&self) -> Result<Vec<RunningRepo>> {
        let resp = self
            .call_on("registry", "list", serde_json::json!({}), RPC_TIMEOUT)
            .await?;
        decode_processes(resp)
    }

    /// repo を登録する (旧 `POST /api/daemon/repos`)。
    pub async fn add_repo(&self, name: &str, path: &str) -> Result<()> {
        self.control(
            "repos/add",
            serde_json::json!({ "name": name, "path": path }),
        )
        .await?;
        Ok(())
    }

    /// repo を起動する (旧 `POST /api/daemon/processes/{name}/start`)。
    ///
    /// doc 44 P1 (fold-in) 後は子プロセス spawn ではなく daemon の registry への登録。
    /// 既に居れば daemon 側で no-op になる。
    pub async fn start_process(&self, repo_name: &str) -> Result<()> {
        self.control("repos/start", serde_json::json!({ "name": repo_name }))
            .await?;
        Ok(())
    }

    /// repo を再起動する (旧 `POST /api/daemon/processes/{name}/restart`)。
    pub async fn restart_process(&self, repo_name: &str) -> Result<()> {
        self.call_on(
            "daemon-control",
            "repos/restart",
            serde_json::json!({ "name": repo_name }),
            RESTART_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    /// repo を停止する (旧 `POST /api/daemon/processes/{name}/stop`)。
    ///
    /// repo は registered のまま (`enabled` 不変) — 稼働だけ落とす。
    pub async fn stop_process(&self, repo_name: &str) -> Result<()> {
        self.control("repos/stop", serde_json::json!({ "name": repo_name }))
            .await?;
        Ok(())
    }

    /// hub federation 接続の即時張り直しを要求する (auth login/logout 後の credential 反映)。
    ///
    /// daemon 側は常駐ループへ Notify を積むだけの fire-and-forget — 数秒後の `/api/health`
    /// に新しい `hub_auth` が現れる。hub 未設定 daemon でも成功が返る (no-op)。
    pub async fn hub_reconnect(&self) -> Result<()> {
        self.control("hub/reconnect", serde_json::json!({})).await?;
        Ok(())
    }

    /// repo を登録解除する (旧 `POST /api/daemon/repos/remove`)。
    ///
    /// daemon の `remove_repo` は稼働中だとエラーを返すので、caller は先に
    /// `stop_process` を呼ぶこと (repo ディレクトリ自体は削除しない)。
    pub async fn remove_repo(&self, path: &str) -> Result<()> {
        self.control("repos/remove", serde_json::json!({ "path": path }))
            .await?;
        Ok(())
    }

    /// 並び順を daemon に永続化する (旧 `POST /api/daemon/repos/reorder`)。
    pub async fn reorder_repos(&self, paths: Vec<String>) -> Result<()> {
        self.control("repos/reorder", serde_json::json!({ "paths": paths }))
            .await?;
        Ok(())
    }

    // =====================================================================
    // lanes — 旧 `/api/daemon/lanes*`
    // =====================================================================

    /// active lane (presence、Model Q) を daemon canonical に設定する
    /// (旧 `POST /api/daemon/lanes/active`)。
    pub async fn set_active_lane(&self, path: String, address: String) -> Result<()> {
        self.control(
            "lanes/set_active",
            serde_json::json!({ "path": path, "address": address }),
        )
        .await?;
        Ok(())
    }

    /// performer lane を作る (旧 `POST /api/daemon/lanes`、doc 24 §10 Phase 2 B-create)。
    ///
    /// `branch` / `agent` を省くと daemon 側が default を導出する
    /// (HTTP route と同じ `resolve_create_lane_args` を共有)。
    pub async fn create_performer_lane(
        &self,
        repo_path: &str,
        name: &str,
        branch: Option<&str>,
        agent: Option<&str>,
    ) -> Result<()> {
        let mut payload = serde_json::json!({ "path": repo_path, "name": name });
        for (key, value) in [("branch", branch), ("agent", agent)] {
            if let Some(value) = value {
                payload[key] = serde_json::Value::String(value.to_string());
            }
        }
        self.control("lanes/create", payload).await?;
        Ok(())
    }
}

/// Unison の error 慣習 (VP-163): 専用 error frame が無いので、daemon は失敗を
/// **成功 frame の `{"error": ...}`** で返す。transport 成功 = 処理成功ではないため、
/// ここで拾わないと未知 method や validation 失敗が silent success になる
/// (`daemon_repo_request` が同じ理由で同じ処理をしている)。
pub fn rpc_error(resp: &serde_json::Value) -> Option<String> {
    resp.get("error").map(|e| match e.as_str() {
        Some(s) => s.to_string(),
        // 文字列以外 (object / array) で返る余地も残す — 握り潰すと原因が消える。
        None => e.to_string(),
    })
}

/// `daemon-control.repos/list` の応答 → `RepoInfo` 一覧。
///
/// ⚠️ 旧 HTTP `GET /api/daemon/repos` は `{"repos": [...]}` で包んでいたが、
/// Unison 版は **裸の配列**を返す (`handle_daemon_control` が `to_value(&list)` する)。
/// 中身の要素は同じ `RepoInfo` なので、差はこの包み 1 枚だけ
/// (テスト: `repos_list_decodes_same_as_http_shape`)。
pub fn decode_repos(resp: serde_json::Value) -> Result<Vec<RepoInfo>> {
    serde_json::from_value(resp).context("repos/list レスポンスのパースに失敗")
}

/// `registry.list` の応答 (`{"processes": [...]}`) → `RunningRepo` 一覧。
///
/// 包みの形は旧 HTTP `GET /api/daemon/processes` と同じ。
pub fn decode_processes(resp: serde_json::Value) -> Result<Vec<RunningRepo>> {
    let processes = resp
        .get("processes")
        .cloned()
        .ok_or_else(|| anyhow!("registry.list に processes field が無い"))?;
    serde_json::from_value(processes).context("registry.list レスポンスのパースに失敗")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 HTTP `GET /api/daemon/repos` の wire shape (`{"repos": [...]}`)。
    ///
    /// 本 struct は client.rs から**消えた**ものを test に残したもの。段 3 の移行が
    /// 正しい (= 新面が旧面と同じ答えを出す) ことを、両方の shape を decode して
    /// 突き合わせることで固定する。
    #[derive(serde::Deserialize)]
    struct LegacyReposResponse {
        repos: Vec<RepoInfo>,
    }

    /// 旧 HTTP `GET /api/daemon/processes` の wire shape。
    #[derive(serde::Deserialize)]
    struct LegacyProcessesResponse {
        #[serde(default)]
        processes: Vec<RunningRepo>,
    }

    fn sample_repos() -> serde_json::Value {
        serde_json::json!([
            {
                "name": "vp",
                "path": "/repos/vp",
                "process_status": "running",
                "active_lane": "vp:lane:conductor",
            },
            { "name": "nexus", "path": "/repos/nexus" },
        ])
    }

    /// 新面 (裸配列) と旧面 (`{repos:[...]}`) が同じ `RepoInfo` 一覧に落ちる。
    ///
    /// 差は包み 1 枚だけ、という主張をここで固定する。要素の形が片方だけ変われば
    /// (例: `process_status` alias の取りこぼし) このテストが落ちる。
    #[test]
    fn repos_list_decodes_same_as_http_shape() {
        let unison = sample_repos();
        let http = serde_json::json!({ "repos": sample_repos() });

        let via_unison = decode_repos(unison).expect("unison decode");
        let via_http: LegacyReposResponse = serde_json::from_value(http).expect("http decode");

        assert_eq!(via_unison.len(), via_http.repos.len());
        for (u, h) in via_unison.iter().zip(via_http.repos.iter()) {
            assert_eq!(u.name, h.name);
            assert_eq!(u.path, h.path);
            assert_eq!(u.state, h.state, "process_status alias が両面で効くこと");
            assert_eq!(u.active_lane, h.active_lane);
        }
        // 中身が本当に入っているか (空 Vec 同士の一致で通してしまわない)。
        assert_eq!(via_unison[0].name, "vp");
        assert_eq!(
            via_unison[0].state,
            crate::client::RepoStatus::Running,
            "process_status が state に載ること"
        );
    }

    /// `registry.list` と旧 HTTP `GET /api/daemon/processes` は包みまで同形。
    #[test]
    fn processes_list_decodes_same_as_http_shape() {
        let wire = serde_json::json!({
            "processes": [
                { "repo_name": "vp", "port": 33000, "pid": 1234, "repo_path": "/repos/vp" },
            ]
        });

        let via_unison = decode_processes(wire.clone()).expect("unison decode");
        let via_http: LegacyProcessesResponse = serde_json::from_value(wire).expect("http decode");

        assert_eq!(via_unison.len(), 1);
        assert_eq!(via_unison.len(), via_http.processes.len());
        assert_eq!(via_unison[0].repo_name, via_http.processes[0].repo_name);
        assert_eq!(via_unison[0].port, via_http.processes[0].port);
    }

    /// daemon が空 map を返すケース (repo 未起動) でも空 Vec に落ちる。
    #[test]
    fn processes_list_accepts_empty() {
        let wire = serde_json::json!({ "processes": [] });
        assert!(decode_processes(wire).expect("decode").is_empty());
    }

    /// `{"error": ...}` を含む成功 frame は Err 扱いにする (VP-163)。
    /// ここを拾わないと「成功ログが出るのに何も起きない」silent success になる。
    #[test]
    fn rpc_error_detects_error_frame() {
        assert_eq!(
            rpc_error(&serde_json::json!({ "error": "path is required" })).as_deref(),
            Some("path is required")
        );
        assert!(rpc_error(&serde_json::json!({ "status": "ok" })).is_none());
        // repos/list は裸配列を返す — error field を持ち得ないので None。
        assert!(rpc_error(&serde_json::json!([{ "name": "vp" }])).is_none());
    }
}
