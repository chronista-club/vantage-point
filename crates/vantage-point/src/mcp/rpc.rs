//! MCP の接続と stream の寿命を分ける。送信後の回復は次の call にだけ適用する。
use super::*;
use unison::{ProtocolClient, UnisonChannel};

pub(super) struct Connection(pub Arc<ProtocolClient>);

impl Drop for Connection {
    fn drop(&mut self) {
        // ProtocolClient の drop だけでは accept task が接続を保持する。
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let client = self.0.clone();
            runtime.spawn(async move {
                let _ = client.disconnect().await;
            });
        }
    }
}

pub(super) struct Channel {
    pub channel: Arc<UnisonChannel>,
    connection: Arc<Connection>,
}

impl Channel {
    #[cfg(test)]
    pub async fn is_connected(&self) -> bool {
        self.connection.0.is_connected().await
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // この call 専用の stream を閉じる。cancel / timeout / handshake 失敗時も解放する。
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let channel = self.channel.clone();
            let connection = self.connection.clone();
            runtime.spawn(async move {
                let _ = channel.close().await;
                drop(connection);
            });
        }
    }
}

// loopback の通常往復より余裕を取り、古い接続の open_ack 待ちを有界にする。
const PREPARE_TIMEOUT: Duration = Duration::from_secs(1);
const PREPARE_ATTEMPTS: usize = 2;

#[derive(Default)]
pub(super) struct ConnectionCache {
    connection: Option<Arc<Connection>>,
}

impl ConnectionCache {
    pub async fn get(
        &mut self,
        addr: &str,
        name: &str,
        repo_path: &str,
        request_timeout: Duration,
    ) -> Result<Channel, McpError> {
        for attempt in 0..PREPARE_ATTEMPTS {
            // connect / open_ack / subscribe の全体を試行ごとの期限に収める。
            let result = tokio::time::timeout(
                PREPARE_TIMEOUT,
                self.open(addr, name, repo_path, request_timeout),
            )
            .await
            .unwrap_or_else(|_| {
                Err(McpError::internal_error(
                    format!("Unison {name} preparation timed out"),
                    None,
                ))
            });
            match result {
                Ok(channel) => return Ok(channel),
                Err(error) => {
                    self.connection = None;
                    if attempt + 1 == PREPARE_ATTEMPTS {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("最後の試行は結果を返す")
    }

    async fn open(
        &mut self,
        addr: &str,
        name: &str,
        repo_path: &str,
        request_timeout: Duration,
    ) -> Result<Channel, McpError> {
        if let Some(connection) = &self.connection
            && !connection.0.is_connected().await
        {
            self.connection = None;
        }
        if self.connection.is_none() {
            self.connection = Some(connect_quic(addr).await?);
        }
        let connection = self.connection.as_ref().unwrap().clone();
        // stream は call ごとに新造。open_ack を受けるまで業務 request は送らない。
        let channel = connection.0.open_channel(name).await.map_err(|error| {
            McpError::internal_error(format!("Unison {name} channel error: {error}"), None)
        })?;
        let channel = Channel {
            channel: Arc::new(channel.with_request_timeout(request_timeout)),
            connection,
        };
        if name == "repo-proxy" {
            let response = channel
                .channel
                .request::<serde_json::Value, serde_json::Value>(
                    "subscribe",
                    &serde_json::json!({"repo_path":repo_path}),
                )
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("repo-proxy handshake error: {e}"), None)
                })?;
            if let Some(error) = rpc_response_error(&response) {
                return Err(McpError::internal_error(
                    format!("repo-proxy handshake error: {error}"),
                    None,
                ));
            }
        }
        Ok(channel)
    }
}

pub(super) async fn request_once(
    channel: Channel,
    label: &str,
    method: &str,
    payload: &serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, McpError> {
    let error = match tokio::time::timeout(
        timeout,
        channel
            .channel
            .request::<serde_json::Value, serde_json::Value>(method, payload),
    )
    .await
    {
        Ok(Ok(response)) => return Ok(response),
        Ok(Err(error)) => format!("QUIC {label}.{method} failed: {error}"),
        Err(_) => format!("QUIC {label}.{method} timed out"),
    };
    // stream はこの call 専用で、終了時に閉じる。要求の成否は不明なので再送しない。
    Err(McpError::internal_error(error, None))
}
