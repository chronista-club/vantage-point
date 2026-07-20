//! Daemon への Unison チャネルクライアント
//!
//! Daemon (TheWorld :32000) に QUIC 接続し、world-process / events channel を通じて
//! Process lifecycle の取得・event log の emit/query を行う。
//!
//! 接続時に channel を best-effort open し、以降は各 channel の
//! request() メソッドでリクエスト/レスポンスを行う。

use anyhow::{Context, Result};
use unison::network::MessageType;
use unison::{ProtocolClient, UnisonChannel};

use super::protocol::*;

/// TheWorld QUIC ポート（Daemon 統合: [::1]:32000）。
///
/// VP_PROFILE 分離: brew=32000 / dev=32100。 world port と同一 (`vp_paths::default_world_port()`)。
pub fn daemon_quic_port() -> u16 {
    crate::cli::world_port()
}

/// Daemon への Unison チャネルクライアント
///
/// world-process / events channel を保持し、用途別にリクエストをルーティングする:
/// - world_process_ch: VP-154 PR-2 — Process lifecycle (list / subscribe)
/// - events_ch: L2 (doc 27 §5-3) — event log (emit / query)
pub struct DaemonClient {
    /// VP-154 PR-2: world-process チャネル（Process snapshot / lifecycle stream）
    /// open 失敗時は None (= TheWorld が古い binary で channel 不在のとき silent fallback)。
    world_process_ch: Option<UnisonChannel>,
    /// L2 (doc 27 §5-3): events チャネル（event log の emit / query）。
    /// best-effort open、古い daemon で不在なら None（呼び出し時 error）。
    events_ch: Option<UnisonChannel>,
}

impl DaemonClient {
    /// Daemon に接続し、world-process / events チャネルを best-effort open する（リトライ付き）
    ///
    /// 最大 `retries` 回、200ms 間隔で接続を試みる。
    pub async fn connect(port: u16, retries: u32) -> Result<Self> {
        let addr = format!("[::1]:{}", port);
        // VP-184: Builder API 移行 (dev default を明示、 PR-3 で mesh keypair に差し替え)。
        let transport = unison::network::quic::QuicClient::builder()
            .trust_anchors(unison::network::TrustAnchors::SkipVerification)
            .build()
            .context("QUIC クライアントの作成に失敗")?;
        let client = ProtocolClient::new(transport);

        for attempt in 0..retries {
            match client.connect(&addr).await {
                Ok(_) => {
                    tracing::info!("Daemon に接続 ({})", addr);

                    // VP-154 PR-2: world-process は best-effort オープン (= 古い daemon で
                    // channel 不在なら None で fallback、 list/subscribe 呼び出し時 error 化)。
                    let world_process_ch = match client.open_channel("world-process").await {
                        Ok(ch) => Some(ch),
                        Err(e) => {
                            tracing::debug!(
                                "world-process チャネル不在 (古い daemon バイナリ?): {}",
                                e
                            );
                            None
                        }
                    };

                    // L2: events も best-effort open（古い daemon は None で fallback）。
                    let events_ch = match client.open_channel("events").await {
                        Ok(ch) => Some(ch),
                        Err(e) => {
                            tracing::debug!("events チャネル不在 (古い daemon バイナリ?): {}", e);
                            None
                        }
                    };

                    return Ok(Self {
                        world_process_ch,
                        events_ch,
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
    // VP-154 PR-2: world-process 操作
    // =========================================================================

    /// world-process.list — 現在の Process snapshot を取得する。
    ///
    /// 古い daemon バイナリで channel 不在なら error。 caller は `vp daemon stop && start`
    /// でバイナリ更新を促すか、 `/api/health` の stands field に fallback する。
    pub async fn world_processes_list(&self) -> Result<Vec<ProcessSnapshot>> {
        let ch = self.world_process_ch.as_ref().ok_or_else(|| {
            anyhow::anyhow!("world-process チャネル不在 (= daemon バイナリが古い、 PR-2 未反映)")
        })?;
        let resp = ch
            .request::<serde_json::Value, serde_json::Value>("list", &serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("world-process.list 失敗: {}", e))?;
        let processes: Vec<ProcessSnapshot> = serde_json::from_value(resp["processes"].clone())
            .context("world-process.list レスポンスの processes パースに失敗")?;
        Ok(processes)
    }

    /// L2 (doc 27 §5-3): event log に 1 件 emit し、払い出された seq を返す。
    pub async fn events_emit(
        &self,
        kind: &str,
        source: Option<&str>,
        data: serde_json::Value,
    ) -> Result<u64> {
        let ch = self.events_ch.as_ref().ok_or_else(|| {
            anyhow::anyhow!("events チャネル不在 (= daemon バイナリが古い、 event log 未対応)")
        })?;
        let payload = serde_json::json!({ "kind": kind, "source": source, "data": data });
        let resp = ch
            .request::<serde_json::Value, serde_json::Value>("emit", &payload)
            .await
            .map_err(|e| anyhow::anyhow!("events.emit 失敗: {}", e))?;
        resp["seq"]
            .as_u64()
            .context("events.emit レスポンスに seq なし")
    }

    /// L2 (doc 27 §5-3): event log を `since` cursor 以降で query する（古い順、最大 limit 件、0=全件）。
    pub async fn events_query(
        &self,
        since: u64,
        limit: usize,
    ) -> Result<Vec<super::event_log::LoggedEvent>> {
        let ch = self.events_ch.as_ref().ok_or_else(|| {
            anyhow::anyhow!("events チャネル不在 (= daemon バイナリが古い、 event log 未対応)")
        })?;
        let payload = serde_json::json!({ "since": since, "limit": limit });
        let resp = ch
            .request::<serde_json::Value, serde_json::Value>("query", &payload)
            .await
            .map_err(|e| anyhow::anyhow!("events.query 失敗: {}", e))?;
        serde_json::from_value(resp["events"].clone())
            .context("events.query レスポンスの events パースに失敗")
    }

    /// world-process.subscribe — 現在の subscriber stream を確立し、 lifecycle event を
    /// `recv()` で取り出すための reference を返す。
    ///
    /// caller は返された channel reference に対して `recv().await` を loop で呼んで
    /// `ProcessLifecycleEvent` を取得する。 channel 不在は error。
    /// subscribe ack は内部で 1 回 request() で確認、 失敗時は error。
    pub async fn world_processes_subscribe(&self) -> Result<&UnisonChannel> {
        let ch = self.world_process_ch.as_ref().ok_or_else(|| {
            anyhow::anyhow!("world-process チャネル不在 (= daemon バイナリが古い、 PR-2 未反映)")
        })?;
        ch.request::<serde_json::Value, serde_json::Value>("subscribe", &serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("world-process.subscribe 失敗: {}", e))?;
        Ok(ch)
    }

    /// subscribe stream から次の lifecycle event を取得する helper。
    ///
    /// `recv()` は internal の Event queue から ProtocolMessage を取り出し、
    /// method = "event" で来た payload を `ProcessLifecycleEvent` に decode する。
    /// 想定外の method (= 別 event 種別) は warn + skip する design (= 将来 拡張可能性)。
    pub async fn world_processes_recv_event(ch: &UnisonChannel) -> Result<ProcessLifecycleEvent> {
        loop {
            let msg = ch
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("world-process recv 失敗: {}", e))?;
            if msg.msg_type != MessageType::Event {
                tracing::debug!("world-process 想定外 msg_type を skip: {:?}", msg.msg_type);
                continue;
            }
            if msg.method != "event" {
                tracing::warn!("world-process 想定外 event method を skip: {}", msg.method);
                continue;
            }
            let payload = msg
                .payload_as_value()
                .map_err(|e| anyhow::anyhow!("event payload パース失敗: {}", e))?;
            let event: ProcessLifecycleEvent =
                serde_json::from_value(payload).context("ProcessLifecycleEvent decode に失敗")?;
            return Ok(event);
        }
    }
}

/// World daemon の "world-control" channel だけを open する軽量クライアント。
///
/// control plane 一元化: projects は World 権威 (db/world) なので、 CLI は SP を経由せず
/// World daemon に直接 Unison RPC する。 `DaemonClient` は world-process / events を open するが、
/// one-shot CLI の projects 操作はそれらが不要なので world-control のみ open する。
pub struct WorldControlClient {
    ch: UnisonChannel,
    #[allow(dead_code)]
    addr: String,
}

impl WorldControlClient {
    /// World daemon に接続し world-control channel を open する（リトライ付き）。
    pub async fn connect(port: u16, retries: u32) -> Result<Self> {
        let addr = format!("[::1]:{}", port);
        let transport = unison::network::quic::QuicClient::builder()
            .trust_anchors(unison::network::TrustAnchors::SkipVerification)
            .build()
            .context("QUIC クライアントの作成に失敗")?;
        let client = ProtocolClient::new(transport);

        for attempt in 0..retries {
            match client.connect(&addr).await {
                Ok(_) => {
                    let ch = client.open_channel("world-control").await.map_err(|e| {
                        anyhow::anyhow!("world-control チャネルオープン失敗: {}", e)
                    })?;
                    return Ok(Self { ch, addr });
                }
                Err(_) if attempt < retries - 1 => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "World daemon 接続失敗 ({}回リトライ後): {} - {}",
                        retries,
                        addr,
                        e
                    ));
                }
            }
        }
        anyhow::bail!("World daemon 接続失敗: {}", addr)
    }

    /// world-control method を呼ぶ。 Unison の error 慣習 (VP-163) に従い、 success frame の
    /// `{"error": ...}` を `Err` 化する (= Unison は専用 error frame を持たない)。
    async fn call(&self, method: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .ch
            .request::<serde_json::Value, serde_json::Value>(method, &payload)
            .await
            .map_err(|e| anyhow::anyhow!("world-control.{} 失敗: {}", method, e))?;
        if let Some(err) = resp.get("error").and_then(|e| e.as_str()) {
            anyhow::bail!("{}", err);
        }
        Ok(resp)
    }

    /// 登録 project 一覧 (ProjectInfo の JSON 配列、 ord = sidebar 並び順)。
    pub async fn projects_list(&self) -> Result<Vec<serde_json::Value>> {
        let resp = self.call("projects/list", serde_json::json!({})).await?;
        serde_json::from_value(resp).context("projects/list レスポンスのパースに失敗")
    }

    /// project を追加 (追加された ProjectInfo の JSON を返す)。
    pub async fn projects_add(&self, name: &str, path: &str) -> Result<serde_json::Value> {
        self.call(
            "projects/add",
            serde_json::json!({ "name": name, "path": path }),
        )
        .await
    }

    /// project を削除 (path で特定)。
    pub async fn projects_remove(&self, path: &str) -> Result<()> {
        self.call("projects/remove", serde_json::json!({ "path": path }))
            .await?;
        Ok(())
    }

    /// project 名を変更。
    pub async fn projects_rename(&self, path: &str, name: &str) -> Result<()> {
        self.call(
            "projects/rename",
            serde_json::json!({ "path": path, "name": name }),
        )
        .await?;
        Ok(())
    }

    /// project の SP 自動起動 enabled を設定。
    pub async fn projects_set_enabled(&self, path: &str, enabled: bool) -> Result<()> {
        self.call(
            "projects/set_enabled",
            serde_json::json!({ "path": path, "enabled": enabled }),
        )
        .await?;
        Ok(())
    }

    /// 並び順を変更 (path を順に列挙)。
    pub async fn projects_reorder(&self, paths: &[String]) -> Result<()> {
        self.call("projects/reorder", serde_json::json!({ "paths": paths }))
            .await?;
        Ok(())
    }

    /// project を起動する (旧 `vp sp start` の後継)。
    ///
    /// doc 44 P1 (fold-in): project は World プロセス内の `Arc<AppState>` なので、
    /// 「起動」は子プロセス spawn ではなく World の registry への登録を意味する。
    /// 既に起動済みなら World 側で no-op になる (二重起動は map のキー一意性が防ぐ)。
    pub async fn projects_start(&self, name: &str) -> Result<serde_json::Value> {
        self.call("projects/start", serde_json::json!({ "name": name }))
            .await
    }

    /// project を停止する (旧 `vp sp stop` の後継)。
    pub async fn projects_stop(&self, name: &str) -> Result<()> {
        self.call("projects/stop", serde_json::json!({ "name": name }))
            .await?;
        Ok(())
    }

    /// chronista-hub registry の world 一覧を取得する（TheWorld 経由で `worlds.Discover` を proxy）。
    ///
    /// SSOT 原則: CLI は hub に直接接続せず、TheWorld の `hub/discover` RPC を叩く。
    /// `CHRONISTA_HUB_ADDR` 未設定時は World 側が federation 無効エラーを返す。
    pub async fn hub_discover(&self) -> Result<Vec<serde_json::Value>> {
        let resp = self.call("hub/discover", serde_json::json!({})).await?;
        // 新 daemon は `{ "worlds": [...] }` (channel 慣習 + vp-daemon.kdl の returns と一致)、
        // 旧 daemon は bare array を返す。binary 更新 ↔ daemon 再起動の skew を両対応で吸収。
        let worlds = match resp.get("worlds") {
            Some(w) => w.clone(),
            None => resp,
        };
        serde_json::from_value(worlds).context("hub/discover レスポンスのパースに失敗")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_quic_port() {
        // profile 準拠 (brew=32000 / dev=32100)。 SSOT と一致することを検証する
        // (32000 固定 assert だと VP_PROFILE=dev 環境の cargo test で偽陽性に落ちる)。
        assert_eq!(daemon_quic_port(), vp_paths::default_world_port());
    }

    #[test]
    fn test_addr_format() {
        // 接続アドレスのフォーマット確認
        let port: u16 = 32000;
        let addr = format!("[::1]:{}", port);
        assert_eq!(addr, "[::1]:32000");
    }

    #[tokio::test]
    #[ignore] // QUIC ハンドシェイクタイムアウトが長い（~60秒）ため CI ではスキップ
    async fn test_connect_fails_without_daemon() {
        // Daemon が起動していない場合、接続は失敗する
        let result = DaemonClient::connect(39999, 1).await;
        assert!(result.is_err());
    }
}
