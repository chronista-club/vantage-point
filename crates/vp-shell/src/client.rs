//! TheWorld daemon HTTP クライアント
//!
//! Mac の `TheWorldClient.swift` に相当する Rust 実装。
//! port 32000 の TheWorld と HTTP で対話する。
//!
//! Phase W1: projects 一覧取得のみ実装。Phase W2 以降で WebSocket push channel 追加。

use anyhow::Result;
use serde::Deserialize;

/// TheWorld の既定ポート (launch 時に変更可)
pub const DEFAULT_WORLD_PORT: u16 = 32000;

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

impl TheWorldClient {
    pub fn new(port: u16) -> Self {
        Self {
            base_url: format!("http://[::1]:{}", port),
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
}

impl Default for TheWorldClient {
    fn default() -> Self {
        Self::new(DEFAULT_WORLD_PORT)
    }
}
