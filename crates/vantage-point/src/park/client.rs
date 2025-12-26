//! WorldClient - The World との HTTP 通信クライアント
//!
//! Paisley Park から The World への API 呼び出しを提供

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// 登録レスポンス
#[derive(Debug, Deserialize)]
pub struct RegisterResponse {
    pub park_id: String,
    pub session_token: String,
}

/// ACK レスポンス
#[derive(Debug, Deserialize)]
pub struct AckResponse {
    pub ack: bool,
}

/// World クライアント
#[derive(Clone)]
pub struct WorldClient {
    base_url: String,
    client: reqwest::Client,
}

impl WorldClient {
    /// 新しいクライアントを作成
    pub fn new(base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            base_url: base_url.to_string(),
            client,
        }
    }

    /// Paisley Park を The World に登録
    pub async fn register(
        &self,
        project_id: &str,
        project_path: &str,
        port: u16,
    ) -> Result<RegisterResponse> {
        #[derive(Serialize)]
        struct Request {
            project_id: String,
            project_path: String,
            port: u16,
        }

        let url = format!("{}/api/parks/register", self.base_url);
        let req = Request {
            project_id: project_id.to_string(),
            project_path: project_path.to_string(),
            port,
        };

        let response = self.client.post(&url).json(&req).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Registration failed: {}",
                response.status()
            ));
        }

        let data = response.json::<RegisterResponse>().await?;
        Ok(data)
    }

    /// The World から解除
    pub async fn unregister(&self, park_id: &str, reason: Option<&str>) -> Result<bool> {
        #[derive(Serialize)]
        struct Request {
            park_id: String,
            reason: Option<String>,
        }

        let url = format!("{}/api/parks/unregister", self.base_url);
        let req = Request {
            park_id: park_id.to_string(),
            reason: reason.map(|s| s.to_string()),
        };

        let response = self.client.post(&url).json(&req).send().await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let data = response.json::<AckResponse>().await?;
        Ok(data.ack)
    }

    /// ハートビートを送信
    pub async fn heartbeat(&self, park_id: &str, status: &str) -> Result<bool> {
        #[derive(Serialize)]
        struct Request {
            park_id: String,
            status: String,
        }

        let url = format!("{}/api/parks/heartbeat", self.base_url);
        let req = Request {
            park_id: park_id.to_string(),
            status: status.to_string(),
        };

        let response = self.client.post(&url).json(&req).send().await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let data = response.json::<AckResponse>().await?;
        Ok(data.ack)
    }

    /// ヘルスチェック
    pub async fn health(&self) -> Result<bool> {
        let url = format!("{}/health", self.base_url);
        let response = self.client.get(&url).send().await?;
        Ok(response.status().is_success())
    }

    /// View にコンテンツを表示
    pub async fn show(
        &self,
        pane_id: &str,
        content_type: &str,
        content: &str,
        append: bool,
    ) -> Result<bool> {
        #[derive(Serialize)]
        struct Request {
            pane_id: String,
            content_type: String,
            content: String,
            append: bool,
        }

        let url = format!("{}/api/view/show", self.base_url);
        let req = Request {
            pane_id: pane_id.to_string(),
            content_type: content_type.to_string(),
            content: content.to_string(),
            append,
        };

        let response = self.client.post(&url).json(&req).send().await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let data = response.json::<AckResponse>().await?;
        Ok(data.ack)
    }

    /// View のペインをクリア
    pub async fn clear(&self, pane_id: &str) -> Result<bool> {
        #[derive(Serialize)]
        struct Request {
            pane_id: String,
        }

        let url = format!("{}/api/view/clear", self.base_url);
        let req = Request {
            pane_id: pane_id.to_string(),
        };

        let response = self.client.post(&url).json(&req).send().await?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let data = response.json::<AckResponse>().await?;
        Ok(data.ack)
    }
}
