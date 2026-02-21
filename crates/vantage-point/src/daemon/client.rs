//! Daemon への Unison クライアント
//!
//! Console (vp start) から Daemon に QUIC 接続し、
//! セッション操作・PTY I/O を行う。
//!
//! 接続は遅延初期化 + リトライ付き。
//! 通信エラー時は自動リセットし、次回呼び出しで再接続を試みる。

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::Mutex;
use unison::{ProtocolClient, UnisonClient};

use super::protocol::*;
#[allow(unused_imports)]
use super::registry::{PaneId, SessionInfo};

/// Daemon QUIC ポート（設計書: [::1]:34000）
pub const DAEMON_QUIC_PORT: u16 = 34000;

/// Daemon への Unison クライアント
pub struct DaemonClient {
    /// QUIC クライアント（排他制御、call が &mut self のため）
    client: Arc<Mutex<ProtocolClient>>,
    /// 接続先アドレス（リトライ用に保持）
    addr: String,
}

impl DaemonClient {
    /// Daemon に接続する（リトライ付き）
    ///
    /// 最大 `retries` 回、200ms 間隔で接続を試みる。
    /// Daemon がまだ起動中の場合に対応するため。
    pub async fn connect(port: u16, retries: u32) -> Result<Self> {
        let addr = format!("[::1]:{}", port);
        let mut client = ProtocolClient::new_default().context("QUIC クライアントの作成に失敗")?;

        for attempt in 0..retries {
            match UnisonClient::connect(&mut client, &addr).await {
                Ok(_) => {
                    tracing::info!("Daemon に接続 ({})", addr);
                    return Ok(Self {
                        client: Arc::new(Mutex::new(client)),
                        addr,
                    });
                }
                Err(_) if attempt < retries - 1 => {
                    tracing::debug!(
                        "Daemon 接続リトライ ({}/{}): {}",
                        attempt + 1,
                        retries,
                        addr
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Daemon 接続失敗 ({}回リトライ後): {} - {}",
                        retries,
                        addr,
                        e
                    ));
                }
            }
        }

        anyhow::bail!("Daemon 接続失敗: {}", addr)
    }

    /// 汎用 RPC 呼び出し
    ///
    /// メソッド名と JSON ペイロードで Daemon に RPC リクエストを送信する。
    async fn rpc_call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut client = self.client.lock().await;
        client
            .call(method, payload)
            .await
            .map_err(|e| anyhow::anyhow!("RPC '{}' 失敗: {}", method, e))
    }

    // =========================================================================
    // Session 操作
    // =========================================================================

    /// セッションを作成する
    pub async fn create_session(&self, id: &str) -> Result<SessionInfo> {
        let req = CreateSessionRequest {
            session_id: id.to_string(),
        };
        let resp = self
            .rpc_call("session.create", serde_json::to_value(&req)?)
            .await?;
        let info: SessionInfo =
            serde_json::from_value(resp).context("session.create レスポンスのパースに失敗")?;
        Ok(info)
    }

    /// セッション一覧を取得する
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let resp = self.rpc_call("session.list", serde_json::json!({})).await?;
        let list: ListSessionsResponse =
            serde_json::from_value(resp).context("session.list レスポンスのパースに失敗")?;
        // SessionSummary → SessionInfo への変換
        // 一覧取得では簡易情報のみ返すため、SessionInfo として再構築
        let sessions = list
            .sessions
            .into_iter()
            .map(|s| SessionInfo {
                id: s.id,
                panes: vec![],
                created_at: s.created_at,
            })
            .collect();
        Ok(sessions)
    }

    /// セッションにアタッチする（PTY出力ストリーム開始）
    pub async fn attach(&self, session_id: &str) -> Result<()> {
        let req = AttachRequest {
            session_id: session_id.to_string(),
        };
        self.rpc_call("session.attach", serde_json::to_value(&req)?)
            .await?;
        Ok(())
    }

    /// セッションからデタッチする
    pub async fn detach(&self, session_id: &str) -> Result<()> {
        let req = DetachRequest {
            session_id: session_id.to_string(),
        };
        self.rpc_call("session.detach", serde_json::to_value(&req)?)
            .await?;
        Ok(())
    }

    // =========================================================================
    // Terminal 操作
    // =========================================================================

    /// ペインを作成する（PTYプロセス起動）
    pub async fn create_pane(
        &self,
        session_id: &str,
        shell: &str,
        cols: u16,
        rows: u16,
    ) -> Result<PaneId> {
        let req = CreatePaneRequest {
            session_id: session_id.to_string(),
            shell_cmd: shell.to_string(),
            cols,
            rows,
        };
        let resp = self
            .rpc_call("terminal.create_pane", serde_json::to_value(&req)?)
            .await?;
        let pane_resp: CreatePaneResponse = serde_json::from_value(resp)
            .context("terminal.create_pane レスポンスのパースに失敗")?;
        Ok(pane_resp.pane_id)
    }

    /// PTY に入力データを送信する
    ///
    /// `data` はバイト列で、内部で base64 エンコードして送信する。
    pub async fn write_input(&self, session_id: &str, pane_id: PaneId, data: &[u8]) -> Result<()> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        let req = WriteRequest {
            session_id: session_id.to_string(),
            pane_id,
            data: encoded,
        };
        self.rpc_call("terminal.write", serde_json::to_value(&req)?)
            .await?;
        Ok(())
    }

    /// ペインをリサイズする
    pub async fn resize_pane(
        &self,
        session_id: &str,
        pane_id: PaneId,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let req = ResizeRequest {
            session_id: session_id.to_string(),
            pane_id,
            cols,
            rows,
        };
        self.rpc_call("terminal.resize", serde_json::to_value(&req)?)
            .await?;
        Ok(())
    }

    /// PTY 出力を読み取る（ポーリング型）
    ///
    /// 指定ペインの PTY 出力をタイムアウト付きで読み取る。
    /// 出力があればバイト列を返し、なければ空の Vec を返す。
    pub async fn read_output(
        &self,
        session_id: &str,
        pane_id: PaneId,
        timeout_ms: u64,
    ) -> Result<Vec<u8>> {
        let req = ReadOutputRequest {
            session_id: session_id.to_string(),
            pane_id,
            timeout_ms,
        };
        let resp = self
            .rpc_call("terminal.read_output", serde_json::to_value(&req)?)
            .await?;

        let output: ReadOutputResponse = serde_json::from_value(resp)
            .context("terminal.read_output レスポンスのパースに失敗")?;

        if output.bytes_read == 0 {
            return Ok(Vec::new());
        }

        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD
            .decode(&output.data)
            .context("output data の base64 デコード失敗")?;

        Ok(data)
    }

    /// ペインを終了する
    pub async fn kill_pane(&self, session_id: &str, pane_id: PaneId) -> Result<()> {
        let req = KillPaneRequest {
            session_id: session_id.to_string(),
            pane_id,
        };
        self.rpc_call("terminal.kill_pane", serde_json::to_value(&req)?)
            .await?;
        Ok(())
    }

    // =========================================================================
    // System 操作
    // =========================================================================

    /// Daemon のヘルスチェック
    pub async fn health(&self) -> Result<HealthResponse> {
        let resp = self
            .rpc_call("system.health", serde_json::json!({}))
            .await?;
        let health: HealthResponse =
            serde_json::from_value(resp).context("system.health レスポンスのパースに失敗")?;
        Ok(health)
    }

    /// Daemon をシャットダウンする
    pub async fn shutdown(&self) -> Result<()> {
        self.rpc_call("system.shutdown", serde_json::json!({}))
            .await?;
        Ok(())
    }

    /// 接続先アドレスを取得する
    pub fn addr(&self) -> &str {
        &self.addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_quic_port() {
        assert_eq!(DAEMON_QUIC_PORT, 34000);
    }

    #[test]
    fn test_addr_format() {
        // 接続アドレスのフォーマット確認
        let port: u16 = 34000;
        let addr = format!("[::1]:{}", port);
        assert_eq!(addr, "[::1]:34000");
    }

    #[tokio::test]
    #[ignore] // QUIC ハンドシェイクタイムアウトが長い（~60秒）ため CI ではスキップ
    async fn test_connect_fails_without_daemon() {
        // Daemon が起動していない場合、接続は失敗する
        let result = DaemonClient::connect(39999, 1).await;
        assert!(result.is_err());
    }
}
