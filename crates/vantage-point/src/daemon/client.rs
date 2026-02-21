//! Daemon への Unison チャネルクライアント
//!
//! Console (vp start) から Daemon に QUIC 接続し、
//! 3つの永続チャネル（session / terminal / system）を通じて
//! セッション操作・PTY I/O を行う。
//!
//! 接続時にチャネルをオープンし、以降は各チャネルの
//! request() メソッドでリクエスト/レスポンスを行う。

use anyhow::{Context, Result};
use unison::{ProtocolClient, UnisonChannel, UnisonClient};

use super::protocol::*;
#[allow(unused_imports)]
use super::registry::{PaneId, SessionInfo};

/// Daemon QUIC ポート（設計書: [::1]:34000）
pub const DAEMON_QUIC_PORT: u16 = 34000;

/// Daemon への Unison チャネルクライアント
///
/// 3つの永続チャネルを保持し、用途別にリクエストをルーティングする:
/// - session_ch: セッション作成・一覧・アタッチ・デタッチ
/// - terminal_ch: ペイン作成・入出力・リサイズ・終了
/// - system_ch: ヘルスチェック・シャットダウン
pub struct DaemonClient {
    /// Session チャネル（セッション作成・一覧・アタッチ・デタッチ）
    session_ch: UnisonChannel,
    /// Terminal チャネル（ペイン作成・入出力・リサイズ・終了）
    terminal_ch: UnisonChannel,
    /// System チャネル（ヘルスチェック・シャットダウン）
    system_ch: UnisonChannel,
    /// 接続先アドレス（表示・デバッグ用に保持）
    addr: String,
}

impl DaemonClient {
    /// Daemon に接続し、3つのチャネルをオープンする（リトライ付き）
    ///
    /// 最大 `retries` 回、200ms 間隔で接続を試みる。
    /// 接続成功後、session / terminal / system チャネルをオープンする。
    pub async fn connect(port: u16, retries: u32) -> Result<Self> {
        let addr = format!("[::1]:{}", port);
        let mut client = ProtocolClient::new_default().context("QUIC クライアントの作成に失敗")?;

        for attempt in 0..retries {
            match UnisonClient::connect(&mut client, &addr).await {
                Ok(_) => {
                    tracing::info!("Daemon に接続 ({})", addr);

                    // チャネルをオープン
                    let session_ch = client
                        .open_channel("session")
                        .await
                        .map_err(|e| anyhow::anyhow!("session チャネルオープン失敗: {}", e))?;
                    let terminal_ch = client
                        .open_channel("terminal")
                        .await
                        .map_err(|e| anyhow::anyhow!("terminal チャネルオープン失敗: {}", e))?;
                    let system_ch = client
                        .open_channel("system")
                        .await
                        .map_err(|e| anyhow::anyhow!("system チャネルオープン失敗: {}", e))?;

                    return Ok(Self {
                        session_ch,
                        terminal_ch,
                        system_ch,
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

    // =========================================================================
    // Session 操作
    // =========================================================================

    /// セッションを作成する
    pub async fn create_session(&self, id: &str) -> Result<SessionInfo> {
        let payload = serde_json::to_value(&CreateSessionRequest {
            session_id: id.to_string(),
        })?;
        let resp = self
            .session_ch
            .request("create", payload)
            .await
            .map_err(|e| anyhow::anyhow!("session.create 失敗: {}", e))?;
        serde_json::from_value(resp).context("session.create レスポンスのパースに失敗")
    }

    /// セッション一覧を取得する
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let resp = self
            .session_ch
            .request("list", serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("session.list 失敗: {}", e))?;
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
        let payload = serde_json::to_value(&AttachRequest {
            session_id: session_id.to_string(),
        })?;
        self.session_ch
            .request("attach", payload)
            .await
            .map_err(|e| anyhow::anyhow!("session.attach 失敗: {}", e))?;
        Ok(())
    }

    /// セッションからデタッチする
    pub async fn detach(&self, session_id: &str) -> Result<()> {
        let payload = serde_json::to_value(&DetachRequest {
            session_id: session_id.to_string(),
        })?;
        self.session_ch
            .request("detach", payload)
            .await
            .map_err(|e| anyhow::anyhow!("session.detach 失敗: {}", e))?;
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
        let payload = serde_json::to_value(&CreatePaneRequest {
            session_id: session_id.to_string(),
            shell_cmd: shell.to_string(),
            cols,
            rows,
        })?;
        let resp = self
            .terminal_ch
            .request("create_pane", payload)
            .await
            .map_err(|e| anyhow::anyhow!("terminal.create_pane 失敗: {}", e))?;
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
        let payload = serde_json::to_value(&WriteRequest {
            session_id: session_id.to_string(),
            pane_id,
            data: encoded,
        })?;
        self.terminal_ch
            .request("write", payload)
            .await
            .map_err(|e| anyhow::anyhow!("terminal.write 失敗: {}", e))?;
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
        let payload = serde_json::to_value(&ResizeRequest {
            session_id: session_id.to_string(),
            pane_id,
            cols,
            rows,
        })?;
        self.terminal_ch
            .request("resize", payload)
            .await
            .map_err(|e| anyhow::anyhow!("terminal.resize 失敗: {}", e))?;
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
        let payload = serde_json::to_value(&ReadOutputRequest {
            session_id: session_id.to_string(),
            pane_id,
            timeout_ms,
        })?;
        let resp = self
            .terminal_ch
            .request("read_output", payload)
            .await
            .map_err(|e| anyhow::anyhow!("terminal.read_output 失敗: {}", e))?;

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
        let payload = serde_json::to_value(&KillPaneRequest {
            session_id: session_id.to_string(),
            pane_id,
        })?;
        self.terminal_ch
            .request("kill_pane", payload)
            .await
            .map_err(|e| anyhow::anyhow!("terminal.kill_pane 失敗: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // System 操作
    // =========================================================================

    /// Daemon のヘルスチェック
    pub async fn health(&self) -> Result<HealthResponse> {
        let resp = self
            .system_ch
            .request("health", serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("system.health 失敗: {}", e))?;
        let health: HealthResponse =
            serde_json::from_value(resp).context("system.health レスポンスのパースに失敗")?;
        Ok(health)
    }

    /// Daemon をシャットダウンする
    pub async fn shutdown(&self) -> Result<()> {
        self.system_ch
            .request("shutdown", serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("system.shutdown 失敗: {}", e))?;
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
