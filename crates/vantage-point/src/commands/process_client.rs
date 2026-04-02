//! Process HTTP クライアント（CLI 用同期版）
//!
//! MCP の `http_post()` に対応する CLI 版。
//! Process の HTTP API を同期的に呼び出す。

use anyhow::{Result, bail};
use serde::Serialize;

use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};

/// Process HTTP クライアント（blocking）
pub struct ProcessClient {
    port: u16,
    client: reqwest::blocking::Client,
}

impl ProcessClient {
    /// target/port/cwd から Process を自動検出して接続
    pub fn connect(target: Option<&str>, port: Option<u16>, config: &Config) -> Result<Self> {
        let resolved_port = match port {
            Some(p) => p,
            None => resolve_port_from_target(target, config)?,
        };

        let client = reqwest::blocking::Client::new();

        // ヘルスチェックで Process 起動確認
        let health_url = format!("http://[::1]:{}/api/health", resolved_port);
        match client
            .get(&health_url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
        {
            Ok(resp) if resp.status().is_success() => {}
            _ => bail!(
                "Process が起動していません（port {}）。`vp sp start` で起動してください。",
                resolved_port
            ),
        }

        Ok(Self {
            port: resolved_port,
            client,
        })
    }

    /// JSON POST リクエストを Process に送信
    pub fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<serde_json::Value> {
        let url = format!("http://[::1]:{}{}", self.port, path);
        let resp = self
            .client
            .post(&url)
            .json(body)
            .timeout(std::time::Duration::from_secs(15))
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("Process returned HTTP {} ({}): {}", status, path, body);
        }

        let json: serde_json::Value = resp.json()?;
        Ok(json)
    }

    /// GET リクエストを Process に送信
    pub fn get(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("http://[::1]:{}{}", self.port, path);
        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send()?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("Process returned HTTP {} ({}): {}", status, path, body);
        }

        let json: serde_json::Value = resp.json()?;
        Ok(json)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// label または pane_id を受け取り、pane_id に解決する
    ///
    /// `%` で始まるならそのまま返す。それ以外は `/api/tmux/resolve-pane` で逆引き。
    pub fn resolve_pane_id(&self, query: &str) -> Result<String> {
        if query.starts_with('%') {
            return Ok(query.to_string());
        }
        // クエリ文字列のエンコード（スペースや特殊文字を安全に送信）
        let encoded: String = query
            .chars()
            .map(|c| match c {
                ' ' => "%20".to_string(),
                '%' => "%25".to_string(),
                '&' => "%26".to_string(),
                '=' => "%3D".to_string(),
                '#' => "%23".to_string(),
                _ => c.to_string(),
            })
            .collect();
        let resp = self.get(&format!("/api/tmux/resolve-pane?q={}", encoded))?;
        if let Some(pane_id) = resp.get("pane_id").and_then(|v| v.as_str()) {
            Ok(pane_id.to_string())
        } else {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("ペインが見つかりません");
            bail!("{}", err);
        }
    }

    /// pane_id の表示用フォーマットを取得（label があれば "label (pane_id)" 形式）
    pub fn format_pane_display(&self, pane_id: &str) -> String {
        let meta_resp = self.get(&format!(
            "/api/tmux/agent-meta?pane_id={}",
            pane_id.replace('%', "%25")
        ));
        if let Ok(resp) = meta_resp {
            if let Some(label) = resp.pointer("/meta/label").and_then(|v| v.as_str()) {
                return format!("{} ({})", label, pane_id);
            }
        }
        pane_id.to_string()
    }
}

/// target 引数からポートを解決
fn resolve_port_from_target(target: Option<&str>, config: &Config) -> Result<u16> {
    match resolve::resolve_target(target, config)? {
        ResolvedTarget::Running { port, .. } => Ok(port),
        ResolvedTarget::Configured { name, .. } => {
            bail!(
                "プロジェクト '{}' は登録済みですが起動していません。`vp sp start` で起動してください。",
                name
            )
        }
        ResolvedTarget::Cwd { .. } => {
            // resolve_target が running.json も config も見つけられなかった
            bail!("起動中の Process が見つかりません。`vp sp start` で起動してください。")
        }
    }
}
