//! TheWorld control plane クライアント (Unison `world-control` / `registry`)
//!
//! doc 45 段 3: vp-app が抱えていた REST client (`client::TheWorldClient`) のうち、
//! projects / processes / lanes を触る面をここへ移した。vp-app は既に Unison を
//! 主要な transport として使っている (lanes / canvas / terminal / device / wire / process-proxy)
//! ので、control plane だけ HTTP に残す理由が無い — Unison 側には KDL schema と drift
//! 検出があり、HTTP 側には無い (doc 45 §1)。
//!
//! 残った HTTP は `/api/health` 1 本だけで、これは doc 45 §2 の設計判断による
//! (health は「他が壊れている時に動いてほしい」probe なので、意図的に鈍い外殻として
//! HTTP のまま置く)。
//!
//! ## 1 RPC = 1 stream
//!
//! World 側の world-control handler は **1 stream につき逐次** (recv → handle → send を
//! 直列に回す) なので、長い RPC (`projects/restart` は内部に grace sleep + 起動確認が入る) と
//! 5s 周期の poll を同じ stream に相乗りさせると、poll が restart の完了まで待たされる
//! (head-of-line blocking)。call ごとに stream を開いて閉じることで、旧 HTTP の
//! 「1 リクエスト = 1 独立した往復」という性質をそのまま持ち込む。
//!
//! 接続自体は `SharedWorldConn` の 1 本 (F1b、doc 27 §3.4.4) を共有するので、
//! 増えるのは QUIC stream だけ。stream open は同一 connection 上の 1 往復で、
//! 毎回 connect し直していた旧 `world_process_request` より安い。
//!
//! ⚠️ stream は必ず `close()` する。drop 任せにすると recv task と QUIC stream が残り、
//! MAX_STREAMS 枯渇に効いてくる (`run_lanes_session` が踏んだのと同じ罠)。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::client::{ProjectInfo, RunningProcess};

/// 1 RPC の上限。旧 reqwest client の 10s timeout をそのまま引き継ぐ。
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// `projects/restart` だけの上限。World 側で stop → grace sleep → start → 起動確認と
/// 繋ぐので他の RPC より桁が違う。旧 HTTP client では 10s を超えると reqwest が
/// 先に諦めて「失敗したのに再起動は進む」という嘘のエラーになっていた。
const RESTART_TIMEOUT: Duration = Duration::from_secs(60);

/// 共有 QUIC connection 上に control 面を張る client。
///
/// `SharedWorldConn::control()` から得る。connection の再接続は共有 manager が
/// 一手に持っているので、本 struct は「今生きている client で 1 往復する」だけを担う。
#[derive(Clone)]
pub struct WorldControl {
    client: Arc<unison::ProtocolClient>,
}

impl WorldControl {
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

    /// `world-control` channel への 1 往復 (既定 timeout)。
    async fn control(&self, method: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
        self.call_on("world-control", method, payload, RPC_TIMEOUT)
            .await
    }

    // =====================================================================
    // projects — 旧 `/api/world/projects*` / `/api/world/processes/*`
    // =====================================================================

    /// 登録 project 一覧 (旧 `GET /api/world/projects`)。
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>> {
        let resp = self.control("projects/list", serde_json::json!({})).await?;
        decode_projects(resp)
    }

    /// 稼働中 project の snapshot (旧 `GET /api/world/processes`)。
    ///
    /// 面は `registry` channel。World は `running_processes` map を HTTP route と
    /// **同じ Arc** で共有しているので、どちらから読んでも同じ答えになる
    /// (parity テスト: `daemon/server.rs` の `registry_list_matches_http`)。
    pub async fn list_processes(&self) -> Result<Vec<RunningProcess>> {
        let resp = self
            .call_on("registry", "list", serde_json::json!({}), RPC_TIMEOUT)
            .await?;
        decode_processes(resp)
    }

    /// project を登録する (旧 `POST /api/world/projects`)。
    pub async fn add_project(&self, name: &str, path: &str) -> Result<()> {
        self.control(
            "projects/add",
            serde_json::json!({ "name": name, "path": path }),
        )
        .await?;
        Ok(())
    }

    /// project を起動する (旧 `POST /api/world/processes/{name}/start`)。
    ///
    /// doc 44 P1 (fold-in) 後は子プロセス spawn ではなく World の registry への登録。
    /// 既に居れば World 側で no-op になる。
    pub async fn start_process(&self, project_name: &str) -> Result<()> {
        self.control(
            "projects/start",
            serde_json::json!({ "name": project_name }),
        )
        .await?;
        Ok(())
    }

    /// project を再起動する (旧 `POST /api/world/processes/{name}/restart`)。
    pub async fn restart_process(&self, project_name: &str) -> Result<()> {
        self.call_on(
            "world-control",
            "projects/restart",
            serde_json::json!({ "name": project_name }),
            RESTART_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    /// project を停止する (旧 `POST /api/world/processes/{name}/stop`)。
    ///
    /// project は registered のまま (`enabled` 不変) — 稼働だけ落とす。
    pub async fn stop_process(&self, project_name: &str) -> Result<()> {
        self.control("projects/stop", serde_json::json!({ "name": project_name }))
            .await?;
        Ok(())
    }

    /// project を登録解除する (旧 `POST /api/world/projects/remove`)。
    ///
    /// World の `remove_project` は稼働中だとエラーを返すので、caller は先に
    /// `stop_process` を呼ぶこと (repo ディレクトリ自体は削除しない)。
    pub async fn remove_project(&self, path: &str) -> Result<()> {
        self.control("projects/remove", serde_json::json!({ "path": path }))
            .await?;
        Ok(())
    }

    /// 並び順を daemon に永続化する (旧 `POST /api/world/projects/reorder`)。
    pub async fn reorder_projects(&self, paths: Vec<String>) -> Result<()> {
        self.control("projects/reorder", serde_json::json!({ "paths": paths }))
            .await?;
        Ok(())
    }

    // =====================================================================
    // lanes — 旧 `/api/world/lanes*`
    // =====================================================================

    /// active lane (presence、Model Q) を daemon canonical に設定する
    /// (旧 `POST /api/world/lanes/active`)。
    pub async fn set_active_lane(&self, path: String, address: String) -> Result<()> {
        self.control(
            "lanes/set_active",
            serde_json::json!({ "path": path, "address": address }),
        )
        .await?;
        Ok(())
    }

    /// performer lane を作る (旧 `POST /api/world/lanes`、doc 24 §10 Phase 2 B-create)。
    ///
    /// `branch` / `stand` を省くと World 側が default を導出する
    /// (HTTP route と同じ `resolve_create_lane_args` を共有)。
    pub async fn create_performer_lane(
        &self,
        project_path: &str,
        name: &str,
        branch: Option<&str>,
        stand: Option<&str>,
    ) -> Result<()> {
        let mut payload = serde_json::json!({ "path": project_path, "name": name });
        for (key, value) in [("branch", branch), ("stand", stand)] {
            if let Some(value) = value {
                payload[key] = serde_json::Value::String(value.to_string());
            }
        }
        self.control("lanes/create", payload).await?;
        Ok(())
    }
}

/// Unison の error 慣習 (VP-163): 専用 error frame が無いので、World は失敗を
/// **成功 frame の `{"error": ...}`** で返す。transport 成功 = 処理成功ではないため、
/// ここで拾わないと未知 method や validation 失敗が silent success になる
/// (`world_process_request` が同じ理由で同じ処理をしている)。
pub fn rpc_error(resp: &serde_json::Value) -> Option<String> {
    resp.get("error").map(|e| match e.as_str() {
        Some(s) => s.to_string(),
        // 文字列以外 (object / array) で返る余地も残す — 握り潰すと原因が消える。
        None => e.to_string(),
    })
}

/// `world-control.projects/list` の応答 → `ProjectInfo` 一覧。
///
/// ⚠️ 旧 HTTP `GET /api/world/projects` は `{"projects": [...]}` で包んでいたが、
/// Unison 版は **裸の配列**を返す (`handle_world_control` が `to_value(&list)` する)。
/// 中身の要素は同じ `ProjectInfo` なので、差はこの包み 1 枚だけ
/// (テスト: `projects_list_decodes_same_as_http_shape`)。
pub fn decode_projects(resp: serde_json::Value) -> Result<Vec<ProjectInfo>> {
    serde_json::from_value(resp).context("projects/list レスポンスのパースに失敗")
}

/// `registry.list` の応答 (`{"processes": [...]}`) → `RunningProcess` 一覧。
///
/// 包みの形は旧 HTTP `GET /api/world/processes` と同じ。
pub fn decode_processes(resp: serde_json::Value) -> Result<Vec<RunningProcess>> {
    let processes = resp
        .get("processes")
        .cloned()
        .ok_or_else(|| anyhow!("registry.list に processes field が無い"))?;
    serde_json::from_value(processes).context("registry.list レスポンスのパースに失敗")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 HTTP `GET /api/world/projects` の wire shape (`{"projects": [...]}`)。
    ///
    /// 本 struct は client.rs から**消えた**ものを test に残したもの。段 3 の移行が
    /// 正しい (= 新面が旧面と同じ答えを出す) ことを、両方の shape を decode して
    /// 突き合わせることで固定する。
    #[derive(serde::Deserialize)]
    struct LegacyProjectsResponse {
        projects: Vec<ProjectInfo>,
    }

    /// 旧 HTTP `GET /api/world/processes` の wire shape。
    #[derive(serde::Deserialize)]
    struct LegacyProcessesResponse {
        #[serde(default)]
        processes: Vec<RunningProcess>,
    }

    fn sample_projects() -> serde_json::Value {
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

    /// 新面 (裸配列) と旧面 (`{projects:[...]}`) が同じ `ProjectInfo` 一覧に落ちる。
    ///
    /// 差は包み 1 枚だけ、という主張をここで固定する。要素の形が片方だけ変われば
    /// (例: `process_status` alias の取りこぼし) このテストが落ちる。
    #[test]
    fn projects_list_decodes_same_as_http_shape() {
        let unison = sample_projects();
        let http = serde_json::json!({ "projects": sample_projects() });

        let via_unison = decode_projects(unison).expect("unison decode");
        let via_http: LegacyProjectsResponse = serde_json::from_value(http).expect("http decode");

        assert_eq!(via_unison.len(), via_http.projects.len());
        for (u, h) in via_unison.iter().zip(via_http.projects.iter()) {
            assert_eq!(u.name, h.name);
            assert_eq!(u.path, h.path);
            assert_eq!(u.state, h.state, "process_status alias が両面で効くこと");
            assert_eq!(u.active_lane, h.active_lane);
        }
        // 中身が本当に入っているか (空 Vec 同士の一致で通してしまわない)。
        assert_eq!(via_unison[0].name, "vp");
        assert_eq!(
            via_unison[0].state,
            crate::client::ProcessStatus::Running,
            "process_status が state に載ること"
        );
    }

    /// `registry.list` と旧 HTTP `GET /api/world/processes` は包みまで同形。
    #[test]
    fn processes_list_decodes_same_as_http_shape() {
        let wire = serde_json::json!({
            "processes": [
                { "project_name": "vp", "port": 33000, "pid": 1234, "project_path": "/repos/vp" },
            ]
        });

        let via_unison = decode_processes(wire.clone()).expect("unison decode");
        let via_http: LegacyProcessesResponse = serde_json::from_value(wire).expect("http decode");

        assert_eq!(via_unison.len(), 1);
        assert_eq!(via_unison.len(), via_http.processes.len());
        assert_eq!(
            via_unison[0].project_name,
            via_http.processes[0].project_name
        );
        assert_eq!(via_unison[0].port, via_http.processes[0].port);
    }

    /// World が空 map を返すケース (project 未起動) でも空 Vec に落ちる。
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
        // projects/list は裸配列を返す — error field を持ち得ないので None。
        assert!(rpc_error(&serde_json::json!([{ "name": "vp" }])).is_none());
    }
}
