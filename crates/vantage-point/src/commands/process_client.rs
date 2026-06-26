//! Process クライアント（CLI 用同期版）
//!
//! - `ProcessClient`: HTTP API の同期呼び出し（MCP の `http_post()` 相当）。
//! - `send_process_message`: Unison(QUIC) "process" チャネルへの ProcessMessage 送信
//!   （MCP の `process_call`/`quic_call` 相当、sync CLI 用。PR1a で switch_lane が移行）。

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

    /// label または pane_id を受け取り、(pane_id, 表示名) を返す
    ///
    /// `%` で始まるならそのまま pane_id とし、resolve API で meta を取得して表示名を生成。
    /// label の場合は逆引きして pane_id + meta を一括取得（HTTP 1回のみ）。
    pub fn resolve_pane(&self, query: &str) -> Result<(String, String)> {
        if query.starts_with('%') {
            // pane_id → resolve API で meta も取得
            let encoded = query.replace('%', "%25");
            let display = match self.get(&format!("/api/tmux/resolve-pane?q={}", encoded)) {
                Ok(resp) => {
                    if let Some(label) = resp.pointer("/meta/label").and_then(|v| v.as_str()) {
                        format!("{} ({})", label, query)
                    } else {
                        query.to_string()
                    }
                }
                Err(_) => query.to_string(),
            };
            return Ok((query.to_string(), display));
        }
        // label → resolve API で逆引き
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
            let label = resp.pointer("/meta/label").and_then(|v| v.as_str());
            let display = match label {
                Some(l) => format!("{} ({})", l, pane_id),
                None => pane_id.to_string(),
            };
            Ok((pane_id.to_string(), display))
        } else {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("ペインが見つかりません");
            bail!("{}", err);
        }
    }
}

/// CLI 用: local SP の Unison(QUIC) "process" チャネルに ProcessMessage を 1 つ送る同期 helper。
///
/// MCP の `process_call`(mcp.rs) の CLI 同期版。sync CLI から呼べるよう内部で tokio runtime を
/// 建てて block_on する。`method` は `unison_server.rs:536-541` の dispatch arm 名
/// （"switch_lane" / "show" / "clear" 等）、`msg` はそのまま JSON 化されて payload になる。
/// caller は method と `ProcessMessage` variant が dispatch arm と一致することを保証する。
///
/// PR1a で switch_lane を HTTP `/api/show` から Unison に移行する入口。
/// PR1b (ROTO routing) 等、CLI から QUIC で ProcessMessage を投げる経路で再利用する。
pub fn send_process_message(
    port: u16,
    method: &str,
    msg: &crate::protocol::ProcessMessage,
) -> Result<()> {
    // QUIC(rustls) は CryptoProvider の install が前提（install 済みなら no-op）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let payload = serde_json::to_value(msg)?;
    // QUIC_PORT_OFFSET = 0 のため SP の HTTP port と同一 UDP port で QUIC が listen する。
    let addr = format!("[::1]:{}", port);

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!("tokio runtime build failed: {}", e))?;
    rt.block_on(async move {
        // connect+open+request 全体を 10s で bound（旧 HTTP の reqwest timeout 10s と等価)。
        // unison default request timeout は 30s、 connect の QUIC handshake は無 bound なので、
        // SP が wedge した場合に CLI が無言で長時間刺さるのを防ぐ。
        let work = async {
            let transport = unison::network::quic::QuicClient::builder()
                .trust_anchors(unison::network::TrustAnchors::SkipVerification)
                .build()
                .map_err(|e| anyhow::anyhow!("QUIC client build failed: {}", e))?;
            let client = unison::ProtocolClient::new(transport);
            client
                .connect(&addr)
                .await
                .map_err(|e| anyhow::anyhow!("QUIC connect {} 失敗: {}", addr, e))?;
            let channel = client
                .open_channel("process")
                .await
                .map_err(|e| anyhow::anyhow!("open process channel 失敗: {}", e))?;
            let resp: serde_json::Value = channel
                .request::<serde_json::Value, serde_json::Value>(method, &payload)
                .await
                .map_err(|e| anyhow::anyhow!("QUIC process.{} 失敗: {}", method, e))?;
            // unison は専用 error frame を持たず、 server Err は成功フレームに {"error":..} を
            // 詰めて返す（mcp.rs quic_call_with_timeout と同じ扱い）。素通しせず Err に変換する。
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                bail!("SP error (process.{}): {}", method, err);
            }
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), work).await {
            Ok(result) => result,
            Err(_) => bail!(
                "SP ({}) QUIC process.{} が 10s 以内に応答しませんでした",
                addr,
                method
            ),
        }
    })
}

/// F6 (doc 27 §3.4.5/§6): World :32000 の "process-proxy" channel 経由で SP process method を
/// ask する同期 helper。 旧来の SP 直結 (`send_process_message` の port 直結) を World 経由の
/// reverse-route に移す CLI 用入口。 World が project_path から path_key を正規化して当該 SP の
/// "control" channel を逆引きし、 dispatch_process_method へ forward する (SP 直結 HTTP/QUIC を撤去)。
/// 戻り値は SP の応答 JSON (unison error frame `{"error":..}` は Err に変換)。
pub fn world_process_request_blocking(
    world_port: u16,
    project_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    // QUIC(rustls) は CryptoProvider の install が前提（install 済みなら no-op）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let addr = format!("[::1]:{}", world_port);
    let project_path = project_path.to_string();
    let method = method.to_string();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!("tokio runtime build failed: {}", e))?;
    rt.block_on(async move {
        // connect → handshake → ask 全体を 35s で bound (delete は tmux kill 等の orchestration を
        // 含むため `send_process_message` の 10s では不足。 旧 MCP/CLI reqwest の 30s + handshake 往復)。
        let work = async {
            let transport = unison::network::quic::QuicClient::builder()
                .trust_anchors(unison::network::TrustAnchors::SkipVerification)
                .build()
                .map_err(|e| anyhow::anyhow!("QUIC client build failed: {}", e))?;
            let client = unison::ProtocolClient::new(transport);
            client
                .connect(&addr)
                .await
                .map_err(|e| anyhow::anyhow!("QUIC connect {} 失敗: {}", addr, e))?;
            let channel = client
                .open_channel("process-proxy")
                .await
                .map_err(|e| anyhow::anyhow!("open process-proxy channel 失敗: {}", e))?;
            // handshake: project_path を渡す (World が path_key に正規化して control channel を逆引き)。
            channel
                .request::<serde_json::Value, serde_json::Value>(
                    "subscribe",
                    &serde_json::json!({ "project_path": project_path }),
                )
                .await
                .map_err(|e| anyhow::anyhow!("process-proxy handshake 失敗: {}", e))?;
            let resp: serde_json::Value = channel
                .request::<serde_json::Value, serde_json::Value>(&method, &payload)
                .await
                .map_err(|e| anyhow::anyhow!("process-proxy {} 失敗: {}", method, e))?;
            // unison は専用 error frame を持たず、 server Err を成功フレームに {"error":..} で
            // 詰めて返す（send_process_message / mcp.rs quic_call と同じ扱い）。 Err に変換する。
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                bail!("World process-proxy error ({}): {}", method, err);
            }
            Ok::<serde_json::Value, anyhow::Error>(resp)
        };
        match tokio::time::timeout(std::time::Duration::from_secs(35), work).await {
            Ok(result) => result,
            Err(_) => bail!(
                "World ({}) process-proxy.{} が 35s 以内に応答しませんでした",
                addr,
                method
            ),
        }
    })
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

/// L0 portless: target → project_path（World process-proxy handshake の安定 identifier）。
///
/// `resolve_port_from_target` の path 版。 SP port を使わず、 全 ResolvedTarget variant から
/// project path を取り出す（Running=project_dir / Configured=path / Cwd=path）。 SP が未起動でも
/// path は決まるので、 旧 HTTP 経路の「起動してないと使えない」制約が消える。
pub fn resolve_project_path_from_target(target: Option<&str>, config: &Config) -> Result<String> {
    Ok(match resolve::resolve_target(target, config)? {
        ResolvedTarget::Running { project_dir, .. } => project_dir,
        ResolvedTarget::Configured { path, .. } => path,
        ResolvedTarget::Cwd { path } => path,
    })
}
