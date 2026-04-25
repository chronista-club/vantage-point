//! TheWorld daemon HTTP クライアント
//!
//! Mac の `TheWorldClient.swift` に相当する Rust 実装。
//! port 32000 の TheWorld と HTTP で対話する。
//!
//! ## URL 解決
//! 1. `VP_WORLD_URL` env var があれば優先 (例: `http://172.20.78.253:32000`)
//! 2. それ以外は `http://127.0.0.1:32000` (IPv4 loopback)
//!
//! **IPv6 `[::1]` は WSL2 → Windows の localhost 転送で通らない**ため
//! デフォルトは IPv4。WSL2 側で daemon を立ち上げて Windows の
//! vp-app から接続するケースを前提にしている。

use anyhow::Result;
use serde::Deserialize;

/// TheWorld の既定ポート
pub const DEFAULT_WORLD_PORT: u16 = 32000;

/// デフォルト URL 解決
///
/// `VP_WORLD_URL` env var → `http://127.0.0.1:32000`
fn default_base_url() -> String {
    std::env::var("VP_WORLD_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", DEFAULT_WORLD_PORT))
}

/// TheWorld daemon HTTP クライアント
pub struct TheWorldClient {
    base_url: String,
    client: reqwest::Client,
}

/// プロジェクト情報 (TheWorld `/api/world/projects` レスポンス要素)
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    projects: Vec<ProjectInfo>,
}

/// `/api/health` の主要 field のみを取り出した軽量レスポンス
///
/// vp-app の Activity widget で表示するため、TheWorld 側 `HealthResponse` の
/// stands / terminal_token / pid 等は無視。サーバ側の field 追加で壊れないよう
/// `#[serde(default)]` を付けている。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct WorldHealthInfo {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub started_at: String,
}

/// 稼働中 process 情報 (`/api/world/processes` レスポンス要素)
///
/// サーバ側 `RunningProcess` (capability/process_manager_capability.rs) の subset。
/// Activity widget で count にしか使わないので最小限。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RunningProcessInfo {
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub port: u16,
}

#[derive(Debug, Deserialize)]
struct ProcessesResponse {
    #[serde(default)]
    processes: Vec<RunningProcessInfo>,
}

impl TheWorldClient {
    /// ポート指定で IPv4 loopback に向ける
    pub fn new(port: u16) -> Self {
        Self::with_base_url(format!("http://127.0.0.1:{}", port))
    }

    /// 任意の base URL で作成 (env var override / テスト用)
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    /// プロジェクト一覧を取得
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>> {
        let url = format!("{}/api/world/projects", self.base_url);
        let resp: ProjectsResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.projects)
    }

    /// health check ping
    pub async fn ping(&self) -> Result<bool> {
        let url = format!("{}/api/health", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    /// `/api/health` の中身を取得 (Activity widget 用)
    pub async fn world_health(&self) -> Result<WorldHealthInfo> {
        let url = format!("{}/api/health", self.base_url);
        let info: WorldHealthInfo = self.client.get(&url).send().await?.json().await?;
        Ok(info)
    }

    /// 稼働中 process 一覧
    pub async fn list_processes(&self) -> Result<Vec<RunningProcessInfo>> {
        let url = format!("{}/api/world/processes", self.base_url);
        let resp: ProcessesResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.processes)
    }

    /// プロジェクトを追加 (POST /api/world/projects)
    ///
    /// サーバ側 `AddProjectRequest`: `{ name: String, path: String }`
    /// 成功時はサーバが追加した `ProjectInfo` を返す (本実装では破棄)。
    pub async fn add_project(&self, name: &str, path: &str) -> Result<()> {
        let url = format!("{}/api/world/projects", self.base_url);
        let body = serde_json::json!({ "name": name, "path": path });
        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("add_project HTTP {}: {}", status, text);
        }
        Ok(())
    }
}

impl Default for TheWorldClient {
    fn default() -> Self {
        Self::with_base_url(default_base_url())
    }
}
