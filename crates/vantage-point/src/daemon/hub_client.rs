//! chronista-hub への Unison client — VP の実 world を hub registry に register / discover する。
//!
//! ## 責務分担（prior art: mem_1CaVeTysipdgVHoxwxUcPj / mem_1Cc1dA79VZu586fjqafiBS）
//! - **SSOT**: hub への register は **TheWorld 経由のみ**。個別 SP / performer は hub と直接話さない。
//! - **opt-in**: hub 依存は env `CHRONISTA_HUB_ADDR` 未設定なら全 skip（= machine-local 動作）。
//! - **degradation**: hub down でも world は machine-local で動き続ける（federation 機能だけ失う）。
//!
//! ## hub 側の契約（chronista-hub v0.1.0, `hub_protocol.kdl`、変更不可）
//! - Unison surface addr: env `CHRONISTA_HUB_UNISON_ADDR`、default `[::1]:7879`（QUIC/UDP）
//! - channel `worlds`:
//!   - `Register {handle, name}` → `{handle, registered_at}`
//!   - `Discover {}` → `{worlds: [{handle, name, registered_at}]}`
//!
//! ## federation L2 追従（ADR-020 D2/D3、wld_id namespace + endpoint）
//! - VP は `Register` に `wld_id`（位置独立 routing key `wld_xxx`）と `endpoints`（direct 到達
//!   候補 `["[GUA]:port"]`、IPv6 GUA 優先・tailnet 非依存・relay floor）を **additive** で載せる。
//! - hub S2（registry endpoint field）未実装の現状 hub はこの 2 field を無視するが、 protocol は
//!   additive なので非破壊。S2 landed 後に hub が `wld_id → endpoint(s)` を index し、 `Discover`
//!   が両者を carry する（[`WorldEntry::wld_id`] / [`WorldEntry::endpoints`] が受け皿）。
//!
//! ## cert 検証（trust anchors、2026-06-28〜）
//! - 公開 hub `hub.chronista.club:12879` は **実 CA cert（Let's Encrypt、ISRG Root chain）** で
//!   稼働 → VP は cert 配布も pin も不要、`TrustAnchors::System`（webpki-roots Mozilla bundle）で
//!   公的検証する。cert は90日ごと無人 rotate されるが System trust なので VP は無変更。
//!   ⚠️ System trust は SNI↔SAN を照合するので **必ず hostname で dial**（生 IP 不可）。
//! - loopback dev hub（self-signed）は `SkipVerification`。振り分けは [`hub_trust_anchors`]。
//! - VP は既に Unison native（daemon QUIC server / WorldControlClient）。rustls の
//!   CryptoProvider は VP 既存経路（aws_lc_rs）で install 済みのため、ここでの再 install は不要。

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use unison::ProtocolClient;
use unison::network::channel::UnisonChannel;
use unison::network::{ClientConnectionEvent, ClientConnectionEventReceiver, NetworkError};

/// hub Unison surface の addr を読む env var。未設定/空なら hub federation は opt-out。
pub const HUB_ADDR_ENV: &str = "CHRONISTA_HUB_ADDR";

/// hub registry に登録された 1 world の entry（`worlds.Discover` の戻り要素）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorldEntry {
    /// home-World の位置独立 routing key `wld_xxx`（ADR-020 D2）。hub S2 (registry endpoint
    /// field) 未実装の現状は空文字で返り得るため `#[serde(default)]`。S2 landed 後は
    /// Discover が `wld_id → endpoint` を carry する（前方互換のためここで先に受け皿を持つ）。
    #[serde(default)]
    pub wld_id: String,
    /// この world の direct 到達 endpoint 候補 (`["[GUA]:port", ..]`、ADR-020 D3-a)。dialer が
    /// 順に QUIC direct を試し、全滅で hub relay に落ちる。hub S2 未実装の現状は空配列で返り得る
    /// ため `#[serde(default)]`（Discover が endpoints を返すのは S2 landed 後）。
    #[serde(default)]
    pub endpoints: Vec<String>,
    pub handle: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub registered_at: String,
}

/// env から hub addr を取得する（= opt-in 判定）。未設定 or 空白のみなら `None`（federation skip）。
pub fn hub_addr() -> Option<String> {
    std::env::var(HUB_ADDR_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// この world の handle（hub registry の一意キー）を解決する。
///
/// 優先順位（prior art の `@host = VP machine` と整合）:
/// 1. 明示 override（呼び出し側が handle を指定した場合）
/// 2. OS hostname（`hostname` crate）
/// 3. fallback `"vp-world"`
pub fn resolve_handle(override_handle: Option<&str>) -> String {
    if let Some(h) = override_handle.map(str::trim).filter(|s| !s.is_empty()) {
        return h.to_string();
    }
    hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vp-world".to_string())
}

/// hub addr の host 部が loopback（localhost / 127.0.0.1 / `[::1]`）か判定する。
///
/// `host:port` から host を取り出し、 IP なら `is_loopback()`、 文字列なら `localhost` を loopback
/// とみなす。hub addr は常に port 付きなので素朴な rsplit で十分（hostname に `:` は無い）。
fn is_loopback_hub(addr: &str) -> bool {
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// hub 接続に使う trust anchors を addr から選ぶ。
///
/// - **公開 hub**（hostname、 非 loopback）= 実 CA cert（Let's Encrypt、 ISRG Root chain）で
///   稼働 → `TrustAnchors::System`（webpki-roots Mozilla bundle、 ISRG 含む）で公的検証。
///   ⚠️ System trust は SNI↔SAN を照合するので **必ず hostname で dial**（生 IP は不可）。
/// - **loopback dev hub**（self-signed）= `SkipVerification`（dev/test の loopback 限定）。
fn hub_trust_anchors(addr: &str) -> unison::network::TrustAnchors {
    if is_loopback_hub(addr) {
        unison::network::TrustAnchors::SkipVerification
    } else {
        unison::network::TrustAnchors::System
    }
}

/// hub の `worlds` channel に接続済みの client。
///
/// `ProtocolClient` で QUIC 接続を張り、`worlds` channel（register/discover 用）を保持する。
///
/// ## なぜ `client` 本体も保持するのか（relay 対応で変更、ADR-020 §S4）
/// register/discover だけなら `open_channel` の戻り channel が接続を内部保持するため `client` は
/// drop してよかった。だが **relay の dialer / target inbound は `client` 本体を必要とする**:
/// - dialer: [`HubClient::dial_relay`] が `client.open_channel("relay")` を呼ぶ。
/// - target inbound: [`HubClient::connect_with_inbound`] が **connect 前**に
///   `client.register_server_channel("relay", ..)` で handler を仕込み、hub が push する
///   server-initiated stream を受ける。この accept loop は connection（= `client`）生存中だけ
///   稼働するので、`client` を drop すると relay 受信が止まる。→ struct で保持し続ける。
pub struct HubClient {
    client: ProtocolClient,
    ch: UnisonChannel,
}

impl HubClient {
    /// hub Unison surface に接続し `worlds` channel を open する（リトライ付き）。
    ///
    /// `retries` 回まで接続を試み、全失敗なら最後のエラーを返す。caller（register 経路）は
    /// このエラーを warn ログに落として machine-local 動作を継続する（degradation）。
    ///
    /// relay の dialer（[`HubClient::dial_relay`]）はこの経路で接続した client でも使える。
    /// relay の **target inbound**（受信）が必要なら [`HubClient::connect_with_inbound`] を使う
    /// （server-initiated stream の handler を connect 前に登録する必要があるため）。
    pub async fn connect(addr: &str, retries: u32) -> Result<Self> {
        let client = build_hub_client(addr)?;
        let ch = connect_and_open_worlds(&client, addr, retries).await?;
        Ok(Self { client, ch })
    }

    /// relay target inbound（受信）対応で接続する（ADR-020 §S4 = universal floor）。
    ///
    /// **connect 前**に `relay` server-channel handler を登録してから接続する。direct 全滅で
    /// 別 world が hub 経由 relay を張ってきたとき、hub は `open_server_stream("relay")` で
    /// この world へ server-initiated reliable stream を push する。handler はその raw stream を
    /// 直読し、先頭 frame = `open{from}`（送信元 wld_id）・以降 = forward された data frame として
    /// drain し、データフレーム毎に `on_msg` を呼ぶ。
    ///
    /// 返った [`HubClient`] を **保持し続ける限り**受信し続ける（drop = connection 断 = 受信停止）。
    pub async fn connect_with_inbound<F, Fut>(addr: &str, retries: u32, on_msg: F) -> Result<Self>
    where
        F: Fn(RelayInbound) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let client = build_hub_client(addr)?;

        // relay handler を connect 前に登録（server-initiated stream の受け皿）。
        let on_msg = std::sync::Arc::new(on_msg);
        client
            .register_server_channel("relay", move |stream| {
                let on_msg = on_msg.clone();
                async move {
                    // 先頭 frame = open{from}（送信元 wld_id）。欠落/早期切断なら受信終了。
                    let from = match stream.recv_frame().await {
                        Ok(m) => m
                            .payload_as_value()
                            .ok()
                            .and_then(|v| v.get("from").and_then(Value::as_str).map(str::to_string))
                            .unwrap_or_default(),
                        Err(_) => return Ok::<(), NetworkError>(()),
                    };
                    // 以降 = forward された data frame。stream 終端（source close）まで drain。
                    while let Ok(msg) = stream.recv_frame().await {
                        let payload = msg.payload_as_value().unwrap_or(Value::Null);
                        on_msg(RelayInbound {
                            from: from.clone(),
                            payload,
                        })
                        .await;
                    }
                    Ok(())
                }
            })
            .await;

        let ch = connect_and_open_worlds(&client, addr, retries).await?;
        Ok(Self { client, ch })
    }

    /// relay dialer（source）— hub 経由で `to_wld_id` への片方向 stream を確立する（ADR-020 §S4）。
    ///
    /// `registry.lookup(wld_id).endpoints` への QUIC direct が全滅したときの **fallback floor**。
    /// `relay` channel を開いて宛先宣言 `{to, from}` を送り、hub の status を待つ:
    /// - `established` → [`RelayDial`] を返す（以降 [`RelayDial::send`] で data を片方向送信）。
    /// - `offline` → target 不在。Err（送り手 home-World の reconcile 対象 = D3-c）。
    /// - `error` / その他 → Err。
    pub async fn dial_relay(&self, to_wld_id: &str, from_wld_id: &str) -> Result<RelayDial> {
        let ch = self
            .client
            .open_channel("relay")
            .await
            .map_err(|e| anyhow::anyhow!("relay チャネル open 失敗: {}", e))?;
        // 宛先宣言 {to, from}。hub は payload だけ読む（method 名は無視するため任意ラベル）。
        ch.send_event("open", &json!({ "to": to_wld_id, "from": from_wld_id }))
            .await
            .map_err(|e| anyhow::anyhow!("relay open 宣言の送信に失敗: {}", e))?;
        // hub の status frame（Event {status, detail}）を待つ。
        let status = ch
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("relay status 受信に失敗: {}", e))?;
        let v = status
            .payload_as_value()
            .map_err(|e| anyhow::anyhow!("relay status の parse に失敗: {}", e))?;
        let detail = v.get("detail").and_then(Value::as_str).unwrap_or_default();
        match v.get("status").and_then(Value::as_str) {
            Some("established") => Ok(RelayDial {
                ch,
                to: to_wld_id.to_string(),
            }),
            Some("offline") => {
                anyhow::bail!("relay target offline（reconcile 対象 D3-c）: {}", to_wld_id)
            }
            other => anyhow::bail!(
                "relay 確立失敗: status={:?} detail={} to={}",
                other,
                detail,
                to_wld_id
            ),
        }
    }

    /// この world を hub registry に register する（`worlds.Register`）。
    ///
    /// - `wld_id` = home-World の位置独立 routing key（ADR-020 D2）。
    /// - `endpoints` = direct 到達 endpoint 候補（`["[GUA]:port", ..]`、D3-a。空配列 = direct
    ///   候補なしで relay floor に委ねる）。
    ///
    /// hub は S2 (registry endpoint field) 未実装の現状この 2 field を無視するが、 additive
    /// なので非破壊で先に送っておく（両側が揃った時点で hub が `wld_id → endpoint` を index する）。
    pub async fn register(
        &self,
        wld_id: &str,
        endpoints: &[String],
        handle: &str,
        name: &str,
    ) -> Result<WorldEntry> {
        let resp: serde_json::Value = self
            .ch
            .request(
                "Register",
                &json!({ "wld_id": wld_id, "endpoints": endpoints, "handle": handle, "name": name }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("worlds.Register 失敗: {}", e))?;
        serde_json::from_value(resp).context("Register レスポンスのパースに失敗")
    }

    /// hub registry に居る world 一覧を取得する（`worlds.Discover`）。
    pub async fn discover(&self) -> Result<Vec<WorldEntry>> {
        let resp: serde_json::Value = self
            .ch
            .request("Discover", &json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("worlds.Discover 失敗: {}", e))?;
        let worlds = resp.get("worlds").cloned().unwrap_or_else(|| json!([]));
        serde_json::from_value(worlds).context("Discover レスポンスのパースに失敗")
    }

    /// connection lifecycle event（Connected / Disconnected）を subscribe する。
    ///
    /// 常駐セッション（[`run_hub_federation`]）が `Disconnected` を待って再接続するために使う。
    /// club-unison は自動 reconnect しない（caller 責務）ので、この event が再接続のトリガ。
    pub fn subscribe_connection_events(&self) -> ClientConnectionEventReceiver {
        self.client.subscribe_connection_events()
    }

    /// hub への QUIC 接続が生きているか。`Disconnected` event を取りこぼした場合の health poll
    /// backstop（[`run_hub_federation`] が定期確認する）。
    pub async fn is_connected(&self) -> bool {
        self.client.is_connected().await
    }
}

/// hub 接続用の QUIC `ProtocolClient` を組む（trust anchors は addr で振り分け）。
///
/// 公開 hub は実 CA cert（Let's Encrypt）で稼働 → System trust で検証。loopback dev hub
/// (self-signed) は SkipVerification。addr の host 部で振り分ける（[`hub_trust_anchors`]）。
fn build_hub_client(addr: &str) -> Result<ProtocolClient> {
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(hub_trust_anchors(addr))
        .build()
        .context("hub QUIC クライアントの作成に失敗")?;
    Ok(ProtocolClient::new(transport))
}

/// `client` で hub に接続し（`retries` 回リトライ）`worlds` channel を open する。
///
/// register/discover/relay-dialer 共通の接続確立。relay target inbound 用の handler 登録は
/// **connect より前**に済ませておく必要があるため、この関数の呼び出し前に行う（[`HubClient::
/// connect_with_inbound`] 参照）。
async fn connect_and_open_worlds(
    client: &ProtocolClient,
    addr: &str,
    retries: u32,
) -> Result<UnisonChannel> {
    let attempts = retries.max(1);
    let mut last_err: Option<String> = None;
    for attempt in 0..attempts {
        match client.connect(addr).await {
            Ok(_) => {
                return client
                    .open_channel("worlds")
                    .await
                    .map_err(|e| anyhow::anyhow!("worlds チャネル open 失敗: {}", e));
            }
            Err(e) => {
                last_err = Some(e.to_string());
                if attempt + 1 < attempts {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
            }
        }
    }
    anyhow::bail!(
        "hub 接続失敗 ({} 回リトライ後): {} - {}",
        attempts,
        addr,
        last_err.unwrap_or_default()
    )
}

/// relay で受信した 1 データフレーム（target inbound 側、[`HubClient::connect_with_inbound`]）。
///
/// hub は payload を opaque に dumb forward する（中身を覗かない = D5）ので、`payload` の意味は
/// 送り手 world と受け手 world のアプリ層が決める。`from` = 送信元 wld_id。
#[derive(Debug, Clone)]
pub struct RelayInbound {
    /// 送信元 home-World の wld_id（hub の `open{from}` 宣言由来）。
    pub from: String,
    /// forward された data frame の payload（opaque JSON、欠落時は `Value::Null`）。
    pub payload: Value,
}

/// 確立済みの relay 片方向 stream（source 側、[`HubClient::dial_relay`] の戻り）。
///
/// hub が source→target を dumb forward する。`send` で data frame を片方向に送る。B→A の
/// 応答は B 側が別 relay を張ることで創発する（D5「片方向 tell の交換」）。
pub struct RelayDial {
    ch: UnisonChannel,
    to: String,
}

impl RelayDial {
    /// data frame を target へ片方向送信する（hub が opaque に forward）。
    pub async fn send(&self, payload: &Value) -> Result<()> {
        self.ch
            .send_event("data", payload)
            .await
            .map_err(|e| anyhow::anyhow!("relay data 送信に失敗 (to={}): {}", self.to, e))
    }

    /// 宛先 wld_id。
    pub fn target(&self) -> &str {
        &self.to
    }
}

/// hub federation の常駐セッション（ADR-020 §S4 = universal floor の target inbound）。
///
/// ## なぜ常駐か（使い捨て register からの昇格）
/// 旧実装は起動時に `connect → register → drop` する**使い捨て**だった（存在告知のみ）。だが
/// relay の **target inbound**（別 world が hub 経由で送ってくる relay の受信）は、server-initiated
/// stream を受ける accept loop が **connection 生存中だけ**動くため、接続を張りっぱなしにする
/// 必要がある。この関数はその常駐ループ:
/// 1. [`HubClient::connect_with_inbound`] で relay 受信 handler を仕込んで接続
/// 2. [`HubClient::register`] で hub registry に存在告知（hub が `wld_id → ctx` を index する）
/// 3. `Disconnected` を検知したら backoff して再接続（hub 再起動 / 回線瞬断からの自律復帰）
///
/// 受信した relay は現状 **ログのみ**（到達実証）。VP の messaging（`vp wire`）への routing は
/// 次アーク。`shutdown` cancel でループを抜ける。hub 未設定時はこの関数自体を呼ばない（caller 側
/// で opt-in 判定）。
pub async fn run_hub_federation(
    addr: String,
    wld_id: String,
    endpoints: Vec<String>,
    handle: String,
    name: String,
    shutdown: CancellationToken,
) {
    // 再接続 backoff（hub 再起動を待つ間 busy loop にしない）。
    const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);
    // Disconnected event を取りこぼした場合の health poll 間隔（backstop）。
    const HEALTH_POLL: Duration = Duration::from_secs(30);

    while !shutdown.is_cancelled() {
        match HubClient::connect_with_inbound(&addr, 5, |inbound: RelayInbound| async move {
            // 行き先（VP wire 等）は次アーク。今は「本番 world が relay を受信できる」到達実証として
            // ログのみ。hub は payload を opaque forward するので中身の解釈はしない。
            tracing::info!(
                from = %inbound.from,
                payload = %inbound.payload,
                "chronista-hub federation relay 受信（inbound、現状ログのみ）"
            );
        })
        .await
        {
            Ok(client) => {
                match client.register(&wld_id, &endpoints, &handle, &name).await {
                    Ok(entry) => tracing::info!(
                        "chronista-hub 常駐 register 成功: wld_id={} endpoints={:?} handle={} registered_at={}",
                        wld_id,
                        endpoints,
                        entry.handle,
                        entry.registered_at
                    ),
                    Err(e) => tracing::warn!(
                        "chronista-hub register 失敗（接続は維持、再接続で再試行）: {}",
                        e
                    ),
                }
                // 切断 or shutdown まで待機（この間 relay 受信 handler は background で稼働）。
                // Disconnected event を主トリガに、取りこぼし対策で is_connected の health poll を併用。
                let mut events = client.subscribe_connection_events();
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        ev = events.recv_skip_lagged() => match ev {
                            Ok(ClientConnectionEvent::Connected { .. }) => continue,
                            Ok(ClientConnectionEvent::Disconnected { reason }) => {
                                tracing::warn!("chronista-hub connection 切断（再接続する）: {reason}");
                                break;
                            }
                            // sender drop = connection 消滅 → 再接続へ。
                            Err(_) => break,
                        },
                        _ = tokio::time::sleep(HEALTH_POLL) => {
                            if !client.is_connected().await {
                                tracing::warn!("chronista-hub connection dead（health poll 検知、再接続する）");
                                break;
                            }
                        }
                    }
                }
                // client drop → connection close。
            }
            Err(e) => tracing::warn!(
                "chronista-hub 接続失敗（{}秒後に再試行、machine-local で継続）: {} (addr={})",
                RECONNECT_BACKOFF.as_secs(),
                e,
                addr
            ),
        }

        // backoff（shutdown で即中断可能）。
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(RECONNECT_BACKOFF) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_addr_env_name() {
        // env var 名は hub エコシステム命名と揃える（hub 側は CHRONISTA_HUB_UNISON_ADDR）。
        assert_eq!(HUB_ADDR_ENV, "CHRONISTA_HUB_ADDR");
    }

    #[test]
    fn loopback_hub_detection() {
        // loopback dev hub = SkipVerification、 公開 hostname = System trust の振り分け根拠。
        assert!(is_loopback_hub("127.0.0.1:7879"));
        assert!(is_loopback_hub("[::1]:7879"));
        assert!(is_loopback_hub("localhost:7879"));
        // 公開 hub は hostname dial（System trust）。生 IP も loopback でないが SAN 照合のため非推奨。
        assert!(!is_loopback_hub("hub.chronista.club:12879"));
        assert!(!is_loopback_hub("163.43.117.17:12879"));
    }

    #[test]
    fn resolve_handle_prefers_override() {
        assert_eq!(resolve_handle(Some("mito-mac")), "mito-mac");
        // 空白のみは無効として扱い、hostname fallback に落とす（空にはならない）。
        assert_ne!(resolve_handle(Some("   ")), "   ");
        assert!(!resolve_handle(None).is_empty());
    }

    #[test]
    fn world_entry_parses_partial() {
        // name / registered_at が欠けても default で埋まる。
        let e: WorldEntry = serde_json::from_value(json!({ "handle": "world-a" })).unwrap();
        assert_eq!(e.handle, "world-a");
        assert_eq!(e.name, "");
        assert_eq!(e.registered_at, "");
    }
}
