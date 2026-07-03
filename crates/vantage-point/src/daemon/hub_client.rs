//! chronista-hub への Unison client — VP の実 world を hub registry に register / discover する。
//!
//! ## 責務分担（prior art: mem_1CaVeTysipdgVHoxwxUcPj / mem_1Cc1dA79VZu586fjqafiBS）
//! - **SSOT**: hub への register は **TheWorld 経由のみ**。個別 SP / performer は hub と直接話さない。
//! - **opt-in**: hub addr（env `CHRONISTA_HUB_ADDR` > config.kdl `hub-addr`、[`hub_addr()`] が解決）
//!   未設定なら全 skip（= machine-local 動作）。常設運用は config.kdl 側（launchd daemon は env を持たない）。
//! - **degradation**: hub down でも world は machine-local で動き続ける（federation 機能だけ失う）。
//!
//! ## connection-level auth（ADR-020 §S3、credential = Creo ID user-jwt）
//! - 接続時に [`hub_credential`]（= `~/.vp/credentials.json` の access_token）が有れば
//!   `connect_with_credential` で提示、無ければ credential なしで接続（graceful degrade）。
//! - credential 提示は全 hub 経路（常駐 register / federate_wire_send / federate_discover_lanes /
//!   hub-discover RPC）で共通 —— 接続確立が [`connect_and_open_worlds`] の 1 点に集約されているため。
//! - hub は現状 permissive = **observe mode**（credential を verify してログるが未認証も通す）。
//!   VP のこの提示は非破壊で、hub が required に反転した後は未ログイン connection が gate される。
//!   contract 確定の経緯は wire thread 019f28c9（hub↔VP coordination、2026-07-04）。
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

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use unison::ProtocolClient;
use unison::network::channel::UnisonChannel;
use unison::network::{ClientConnectionEvent, ClientConnectionEventReceiver, NetworkError};

/// hub federation の接続状態（vp-app の world status 表示用）。
///
/// [`run_hub_federation`] が遷移ごとに更新し、`/api/health` handler が読んで vp-app に返す。
/// 表示は「world online の近くに `Hub ● connected`」のシンプルなインジケータ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubFederationState {
    /// hub addr 未設定（env `CHRONISTA_HUB_ADDR` / config.kdl `hub-addr` とも無し =
    /// machine-local 動作、federation off）。
    Disabled,
    /// 接続試行中 or 再接続 backoff 待ち（まだ register 未確立）。
    Connecting,
    /// hub に接続・register 済み（relay target inbound 受信可能）。
    Connected,
    /// 切断検知（再接続ループが回っている）。
    Disconnected,
}

impl HubFederationState {
    /// `/api/health` の `hub` フィールド値（vp-app が描画に使う）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Disconnected,
            _ => Self::Disabled,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Connecting => 1,
            Self::Connected => 2,
            Self::Disconnected => 3,
        }
    }
}

/// [`HubFederationState`] の共有ハンドル。`run_hub_federation`（writer）と `/api/health`
/// handler（reader）が共有する。状態は単一の enum 値なので `AtomicU8` で lock-free に持つ。
#[derive(Clone, Default)]
pub struct HubFederationStatus(Arc<AtomicU8>);

impl HubFederationStatus {
    /// 初期状態 = `Disabled`（hub 未設定相当）。
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(
            HubFederationState::Disabled.to_u8(),
        )))
    }

    /// 状態を更新する（writer = `run_hub_federation`）。
    pub fn set(&self, state: HubFederationState) {
        self.0.store(state.to_u8(), Ordering::Relaxed);
    }

    /// 現在の状態を読む（reader = `/api/health` handler）。
    pub fn get(&self) -> HubFederationState {
        HubFederationState::from_u8(self.0.load(Ordering::Relaxed))
    }
}

/// hub Unison surface の addr を読む env var（dev override）。config.kdl `hub-addr` より優先。
/// env / config とも未設定なら hub federation は opt-out（解決は [`hub_addr()`]）。
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

/// hub addr を解決する（= federation opt-in 判定の SSOT）。優先順位: env > config.kdl。
///
/// - env `CHRONISTA_HUB_ADDR` — dev override / 一時切替用（従来経路、互換維持）
/// - config.kdl `hub-addr` — 常設運用の SSOT。launchd (LaunchAgent) 起動の daemon は
///   shell env を持たないため、brew / 常駐運用では config 側でしか federation を
///   常時 ON にできない（TERM/PATH/LANG と同じ launchd env 問題の構造的回避）
///
/// どちらも未設定 or 空白のみなら `None`（federation skip = machine-local 動作）。
/// config 読み込み失敗（parse error 等）は env のみで判定する（fail-open で従来動作に degrade）。
pub fn hub_addr() -> Option<String> {
    let env_val = std::env::var(HUB_ADDR_ENV).ok();
    let config_val = crate::config::Config::load().ok().and_then(|c| c.hub_addr);
    resolve_hub_addr(env_val, config_val)
}

/// 優先順位の純関数（env > config）。空白のみは「未設定」として次の候補に fall through する。
fn resolve_hub_addr(env_val: Option<String>, config_val: Option<String>) -> Option<String> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    clean(env_val).or_else(|| clean(config_val))
}

/// hub connection-level auth（ADR-020 §S3）に提示する credential を解決する。
///
/// credential = **raw Creo ID user-jwt（UTF-8 bytes）**（hub 契約、thread 019f28c9 で確定）。
/// hub 側は `String::from_utf8(cred)` → `verify_user_token` → principal を立てる。
///
/// 実体は `vp auth login` が保存する `~/.vp/credentials.json` の `access_token`。未ログイン
/// （file 不在 / 空 token）なら `None` を返し、caller は **credential なしで接続**する
/// （graceful degrade）。現状 hub は permissive = observe mode なので、credential 不在でも
/// federation は動く（verify されないだけ）。hub が required に反転した後は、未ログインだと
/// hub 側で connection が gate される（その時点で `vp auth login` を促すのが正しい挙動）。
///
/// token 期限切れは**ここでは検査しない**（verify は hub 側の責務）。期限切れ token を送れば
/// hub の verify が失敗するが、observe 下では非破壊（ログに verify 失敗が出るだけ）。
fn hub_credential() -> Option<Vec<u8>> {
    match crate::commands::auth::read_credentials() {
        Ok(creds) => credential_from_creds(creds),
        // file 読み込み失敗（壊れた file 等）は credential なし扱い。federation を止めるより
        // observe/permissive で繋ぐ方が degrade として穏当。
        Err(e) => {
            tracing::debug!(
                "hub credential 読み込み失敗（credential なしで接続）: {}",
                e
            );
            None
        }
    }
}

/// credentials から credential bytes を作る純関数（I/O と分離、env 非依存でテスト可能）。
///
/// access_token が有れば raw UTF-8 bytes を返す。file 不在（`None`）or 空白のみ token は
/// `None`（未ログイン相当 = credential なしで接続に degrade）。
fn credential_from_creds(creds: Option<crate::commands::auth::Credentials>) -> Option<Vec<u8>> {
    let token = creds?.access_token;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.as_bytes().to_vec())
    }
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
///
/// ## connection-level auth（ADR-020 §S3）
/// [`hub_credential`] が Creo ID user-jwt を返せば `connect_with_credential`（connect 直後に
/// `unison.auth` で credential を 1 回提示）で接続する。未ログイン等で credential が無ければ
/// 従来どおり `connect`（credential なし）に degrade する。hub は permissive = observe の間
/// どちらでも繋がり、required 反転後は credential 無しの connection が gate される。
///
/// credential 付き接続が**失敗**した場合は原因不問（verifier 拒否 / hub の auth 未対応 /
/// 網断）で warn を出して credential なしに降格し再試行する — 無効 token や旧 hub 相手でも
/// federation が全落ちしない（permissive では従来同等に繋がる。網断なら降格後も同じ理由で
/// 失敗するだけで、次回呼び出し時に credential は再評価される）。
///
/// **auth channel は他 channel より先**という club-unison の制約は `connect_with_credential`
/// が内部で満たす（connect → auth → 本メソッドが worlds を open）。
async fn connect_and_open_worlds(
    client: &ProtocolClient,
    addr: &str,
    retries: u32,
) -> Result<UnisonChannel> {
    let mut credential = hub_credential();
    let attempts = retries.max(1);
    let mut last_err: Option<String> = None;
    for attempt in 0..attempts {
        // credential があれば connection-level auth 付きで、無ければ従来どおり接続する。
        let connected = match &credential {
            Some(cred) => client.connect_with_credential(addr, cred).await,
            None => client.connect(addr).await,
        };
        match connected {
            Ok(_) => {
                return client
                    .open_channel("worlds")
                    .await
                    .map_err(|e| anyhow::anyhow!("worlds チャネル open 失敗: {}", e));
            }
            Err(e) => {
                // credential 付き接続の失敗は**原因を問わず** credential なしに降格して以降の
                // 試行を続ける。失敗モードは複数あり（verifier 拒否 = 期限切れ / audience 不一致、
                // hub が auth 未対応 = `unison.auth` channel 不在の旧 hub / dev hub、等）、error
                // 文言での選別は club-unison の内部文言に依存して取りこぼす（moody-blues レビューで
                // channel 不在経路の取りこぼしを指摘）。credential なし接続は既存の安全パス
                //（permissive なら従来同等）なので無条件降格で悪化しない — 純粋な網断なら降格後も
                // 同じ理由で失敗するだけで、次回の `connect_and_open_worlds` 呼び出し時には
                // [`hub_credential`] が再評価されるため恒久降格にもならない。
                if credential.take().is_some() {
                    tracing::warn!(
                        "hub への credential 付き接続に失敗 — credential なしに降格して再試行します\
                         （verifier 拒否 = token 期限切れ / audience 不一致、または hub が auth \
                         未対応の可能性。`vp auth login` での再ログインも検討）: {}",
                        e
                    );
                }
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

/// 既存接続 `client` で `target_wld` へ relay を張り envelope を 1 本送る（dial + send の共通単位）。
async fn relay_send_on(
    client: &HubClient,
    target_wld: &str,
    from_label: &str,
    envelope: &Value,
) -> Result<()> {
    let dial = client
        .dial_relay(target_wld, from_label)
        .await
        .with_context(|| format!("relay 確立に失敗（to={target_wld}）"))?;
    dial.send(envelope)
        .await
        .context("relay envelope 送信に失敗")?;
    Ok(())
}

/// `discover` で `handle` → `wld_id` を引く（不在 / wld_id 空はエラー）。曖昧性回避のため
/// federation 宛先は handle 明示で受け、ここで routing key（wld_id）に解決する。
async fn discover_wld_by_handle(client: &HubClient, handle: &str) -> Result<String> {
    let worlds = client.discover().await.context("discover に失敗")?;
    let target = worlds.iter().find(|w| w.handle == handle).ok_or_else(|| {
        anyhow::anyhow!(
            "world '{}' が hub registry に居ない（discover {} 件）",
            handle,
            worlds.len()
        )
    })?;
    if target.wld_id.is_empty() {
        anyhow::bail!(
            "world '{}' の wld_id が空（hub が wld_id を index していない）",
            handle
        );
    }
    Ok(target.wld_id.clone())
}

/// 指定 wld_id へ短命接続で relay envelope を送る（handle 解決なし）。宛先 wld_id が既知のケース
/// （discovery の `lanes-reply` 返信等）で使う low-level send。
pub async fn relay_send_to_wld(
    hub_addr: &str,
    target_wld: &str,
    from_label: &str,
    envelope: &Value,
) -> Result<()> {
    let client = HubClient::connect(hub_addr, 3)
        .await
        .context("relay-send: hub 接続に失敗")?;
    relay_send_on(&client, target_wld, from_label, envelope).await
}

/// 遠方 world の lane へ wire envelope を relay 経由で送る（flow ③ = federation 送信、daemon-side）。
///
/// SSOT 原則（hub と話すのは TheWorld のみ）に従い、CLI ではなく **TheWorld の wire channel handler**
/// （`handle_wire_channel` の `wire/federate` method）から呼ぶ。MVP は短命接続:
/// connect → `discover` で `target_world_handle` → `wld_id` 解決 → `dial_relay` → send。受信 world が
/// `dispatch_wire("send")` でローカル配送する。`from_label` は relay の `open{from}` に載るが受信側は
/// envelope の `from` を使うため cosmetic。永続接続の再利用は後の最適化。
pub async fn federate_wire_send(
    hub_addr: &str,
    target_world_handle: &str,
    from_label: &str,
    envelope: &Value,
) -> Result<()> {
    let client = HubClient::connect(hub_addr, 3)
        .await
        .context("federate-send: hub 接続に失敗")?;
    let target_wld = discover_wld_by_handle(&client, target_world_handle)
        .await
        .context("federate-send")?;
    relay_send_on(&client, &target_wld, from_label, envelope)
        .await
        .context("federate-send")
}

/// 遠方 world の lane 一覧を問い合わせる（flow discovery = step 2、daemon-side）。
///
/// 片方向 relay の上に request-response を作る（「双方向は片方向 relay × 2 で創発」）:
/// 1. discovery 用の**一時 wld_id**（`wld_disco-<nonce>`）で別接続を temp-register（永続接続の
///    registration を clobber しないため）。同接続の inbound handler に応答受け皿を仕込む。
/// 2. `lanes-query`（`{request_id, reply_to: 一時 wld_id}`）を target に relay。
/// 3. target が lane を集めて `lanes-reply` を一時 wld_id へ relay で返す。
/// 4. 同接続の handler が `request_id` 一致で受けて oneshot を解決 → lane 配列を返す。
///
/// 宛先を知らないときの「在庫確認」。返りは lane subset（address/kind/name/state）の配列。
pub async fn federate_discover_lanes(
    hub_addr: &str,
    target_world_handle: &str,
) -> Result<Vec<Value>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let temp_wld = format!("wld_disco-{}", uuid::Uuid::new_v4().simple());

    // 応答受け皿: 同接続の inbound handler が request_id 一致の lanes-reply で take + resolve。
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
    let slot = Arc::new(std::sync::Mutex::new(Some(tx)));
    let want = request_id.clone();
    let on_reply = move |inbound: RelayInbound| {
        let slot = slot.clone();
        let want = want.clone();
        async move {
            let p = &inbound.payload;
            let is_reply = p.get("kind").and_then(Value::as_str) == Some("lanes-reply")
                && p.get("request_id").and_then(Value::as_str) == Some(want.as_str());
            if !is_reply {
                return;
            }
            // request_id 一致の lanes-reply を 1 回だけ take して oneshot を解決。
            if let Some(tx) = slot.lock().ok().and_then(|mut g| g.take()) {
                let _ = tx.send(p.get("lanes").cloned().unwrap_or(Value::Null));
            }
        }
    };

    let client = HubClient::connect_with_inbound(hub_addr, 3, on_reply)
        .await
        .context("discover-lanes: hub 接続に失敗")?;
    client
        .register(&temp_wld, &[], "vp-disco", "VP discovery (transient)")
        .await
        .context("discover-lanes: 一時 register に失敗")?;

    let target_wld = discover_wld_by_handle(&client, target_world_handle)
        .await
        .context("discover-lanes")?;
    let query = json!({
        "kind": "lanes-query",
        "request_id": request_id,
        "reply_to": temp_wld,
    });
    relay_send_on(&client, &target_wld, &temp_wld, &query)
        .await
        .context("discover-lanes: query 送信に失敗")?;

    // 応答待ち（timeout）。connection は関数 scope 終了で drop → 一時 register は hub から除去（D1）。
    let lanes = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .map_err(|_| {
            anyhow::anyhow!("discover-lanes: 応答 timeout（target が lanes-reply を返さない）")
        })?
        .map_err(|_| anyhow::anyhow!("discover-lanes: 応答 channel が閉じた"))?;
    Ok(lanes.as_array().cloned().unwrap_or_default())
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
/// 受信した relay frame は `on_relay`（caller 注入）に渡す。transport の lifecycle（接続 / 再接続 /
/// register）と **配送ポリシー**（relay → VP wire への routing 等）を分離するため、配送先は
/// run_world 側で AppState（wire store）を capture したクロージャとして渡す。`on_relay` は再接続
/// ごとに再登録するため `Clone` を要求する。`shutdown` cancel でループを抜ける。hub 未設定時は
/// この関数自体を呼ばない（caller 側で opt-in 判定）。
///
/// 接続状態は `status`（[`HubFederationStatus`]）に遷移ごとに反映し、`/api/health` 経由で vp-app
/// が world status 横の `Hub ● connected` インジケータとして表示する。
// federation session の固有パラメータ（identity 3 つ + status/shutdown/handler）。いずれも独立に
// 必要で struct 化しても可読性が上がらないため allow（caller は run_world の 1 箇所のみ）。
#[allow(clippy::too_many_arguments)]
pub async fn run_hub_federation<F, Fut>(
    addr: String,
    wld_id: String,
    endpoints: Vec<String>,
    handle: String,
    name: String,
    status: HubFederationStatus,
    shutdown: CancellationToken,
    on_relay: F,
) where
    F: Fn(RelayInbound) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    // 再接続 backoff（hub 再起動を待つ間 busy loop にしない）。
    const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);
    // Disconnected event を取りこぼした場合の health poll 間隔（backstop）。
    const HEALTH_POLL: Duration = Duration::from_secs(30);

    while !shutdown.is_cancelled() {
        status.set(HubFederationState::Connecting);
        // 再接続ごとに handler を再登録するため clone（connect_with_inbound は on_msg を move する）。
        match HubClient::connect_with_inbound(&addr, 5, on_relay.clone()).await {
            Ok(client) => {
                match client.register(&wld_id, &endpoints, &handle, &name).await {
                    Ok(entry) => {
                        status.set(HubFederationState::Connected);
                        tracing::info!(
                            "chronista-hub 常駐 register 成功: wld_id={} endpoints={:?} handle={} registered_at={}",
                            wld_id,
                            endpoints,
                            entry.handle,
                            entry.registered_at
                        );
                    }
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
                // 切断検知 → Disconnected を反映（次 iteration 冒頭で Connecting に戻る）。
                status.set(HubFederationState::Disconnected);
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
    fn resolve_hub_addr_precedence() {
        let s = |v: &str| Some(v.to_string());
        // env > config（両方あれば env が勝つ = dev override）
        assert_eq!(resolve_hub_addr(s("env:1"), s("cfg:1")), s("env:1"));
        // env 不在 → config に fall through（launchd 常設運用の本線）
        assert_eq!(resolve_hub_addr(None, s("cfg:1")), s("cfg:1"));
        // 空白のみの env は「未設定」扱いで config へ
        assert_eq!(resolve_hub_addr(s("   "), s("cfg:1")), s("cfg:1"));
        // 両方なし = federation off
        assert_eq!(resolve_hub_addr(None, None), None);
        // 前後空白は trim（config 手編集の揺れ吸収）
        assert_eq!(
            resolve_hub_addr(s(" hub.example:7879 "), None),
            s("hub.example:7879")
        );
        assert_eq!(
            resolve_hub_addr(None, s(" hub.example:7879 ")),
            s("hub.example:7879")
        );
    }

    /// テスト用の Credentials を access_token だけ埋めて作る。
    fn creds_with_token(token: &str) -> crate::commands::auth::Credentials {
        crate::commands::auth::Credentials {
            access_token: token.to_string(),
            token_type: None,
            expires_at: None,
            refresh_token: None,
            scope: None,
        }
    }

    #[test]
    fn credential_from_creds_maps_token_to_bytes() {
        // access_token 有り → raw UTF-8 bytes を credential として返す。
        assert_eq!(
            credential_from_creds(Some(creds_with_token("jwt.abc.def"))),
            Some(b"jwt.abc.def".to_vec())
        );
    }

    #[test]
    fn credential_from_creds_none_when_absent_or_blank() {
        // file 不在（未ログイン）→ None（credential なしで接続に degrade）。
        assert_eq!(credential_from_creds(None), None);
        // 空白のみ token → None（空 credential を送らない）。
        assert_eq!(credential_from_creds(Some(creds_with_token("   "))), None);
        // 空文字も同様。
        assert_eq!(credential_from_creds(Some(creds_with_token(""))), None);
    }

    #[test]
    fn credential_from_creds_trims_surrounding_whitespace() {
        // 前後空白は trim（file 手編集の揺れ吸収、JWT 本体だけを送る）。
        assert_eq!(
            credential_from_creds(Some(creds_with_token("  jwt.abc.def  "))),
            Some(b"jwt.abc.def".to_vec())
        );
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

    /// flow ③+⑤ e2e: 別 world が relay で送ってきた wire envelope が、受信 world のローカル中央
    /// wire store に inject され、宛先 lane が `wire_recv` で拾えることを実証する（hub 起動前提）。
    /// `dispatch_wire` が pub(crate) なので integration test ではなく内部 test に置く。
    ///
    /// 手動実行:
    /// ```bash
    /// CHRONISTA_HUB_ADDR='[::1]:7879' cargo test -p vantage-point --lib \
    ///   daemon::hub_client::tests::relay_to_wire_delivery -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "実 chronista-hub の起動が前提（CHRONISTA_HUB_ADDR）"]
    async fn relay_to_wire_delivery() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let addr = hub_addr().expect("CHRONISTA_HUB_ADDR を設定して hub を起動して実行すること");

        // 受信 world のローカル中央 wire store（run_world の AppState 相当を mem db で再現）。
        let db = crate::db::VpDb::connect_mem().await.expect("connect_mem");
        db.define_schema().await.expect("define_schema");
        let store = crate::capability::WiremsgStore::new(std::sync::Arc::new(db.inner().clone()))
            .await
            .expect("WiremsgStore::new");
        let notifier = crate::capability::WireNotifier::new();
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());

        // 受信 world: relay inbound → dispatch_wire("send") でローカル store に inject（run_world と同形）。
        let target_wld = "wld_wire-target";
        let on_relay = {
            let store = store.clone();
            let notifier = notifier.clone();
            let notify = notify.clone();
            move |inbound: RelayInbound| {
                let store = store.clone();
                let notifier = notifier.clone();
                let notify = notify.clone();
                async move {
                    let _ = crate::process::routes::wire::dispatch_wire(
                        &store,
                        &notifier,
                        &notify,
                        "send",
                        inbound.payload,
                    )
                    .await;
                }
            }
        };
        let receiver = HubClient::connect_with_inbound(&addr, 5, on_relay)
            .await
            .expect("receiver: connect_with_inbound");
        receiver
            .register(target_wld, &[], "vp-wire-target", "VP wire target")
            .await
            .expect("receiver: register");

        // 送信 world: relay を張って wire envelope（{from, to, body}）を送る。to は受信 world 内部の
        // logical address（agent@nostos/main）。送信側は相手の port/PID を知らない。
        // production の federate_wire_send を直接叩く（connect → discover で handle→wld_id →
        // dial_relay → send）。宛先 world は handle "vp-wire-target" で明示（曖昧性回避）。
        let envelope = json!({
            "from": "agent@vp-wire-source/proj/main",
            "to": ["agent@nostos/main"],
            "body": { "kind": "event", "text": "遠方 world からの wire" }
        });
        federate_wire_send(&addr, "vp-wire-target", "vp-wire-source", &envelope)
            .await
            .expect("federate_wire_send");

        // 受信 world の store に届くまで poll（relay → handler → dispatch_wire は非同期）。
        let mut delivered = None;
        for _ in 0..50 {
            let recvd = crate::process::routes::wire::dispatch_wire(
                &store,
                &notifier,
                &notify,
                "recv",
                json!({ "agent": "agent@nostos/main", "timeout": 0 }),
            )
            .await
            .expect("wire recv");
            if recvd.get("count").and_then(|c| c.as_i64()).unwrap_or(0) > 0 {
                delivered = Some(recvd);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let recvd = delivered.expect("relay→wire 配送されなかった（タイムアウト）");
        assert_eq!(recvd["count"], 1, "1 件配送されるはず: {recvd}");

        println!(
            "✅ federate_wire_send e2e OK: vp-wire-source → relay → {target_wld}(vp-wire-target) の agent@nostos/main inbox に配送"
        );
    }

    /// flow step 2 e2e: 遠方 world の lane 一覧を `federate_discover_lanes` で問い合わせ、relay 上の
    /// request-response（lanes-query → lanes-reply）で lane 配列を受け取ることを実証する（hub 前提）。
    /// target 側の lanes-query handler は run_world の on_relay と同形（ここでは固定 lane を返す）。
    ///
    /// 手動実行:
    /// ```bash
    /// CHRONISTA_HUB_ADDR='[::1]:7879' cargo test -p vantage-point --lib \
    ///   daemon::hub_client::tests::discover_lanes_roundtrip -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "実 chronista-hub の起動が前提（CHRONISTA_HUB_ADDR）"]
    async fn discover_lanes_roundtrip() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let addr = hub_addr().expect("CHRONISTA_HUB_ADDR を設定して hub を起動して実行すること");

        // target world: lanes-query を受けたら固定 lane 2 件を lanes-reply で返す（run_world の
        // on_relay と同形の routing。実機では lane_registry を flatten する）。
        let target_wld = "wld_disco-target";
        let addr_for_target = addr.clone();
        let on_query = move |inbound: RelayInbound| {
            let hub_addr = addr_for_target.clone();
            async move {
                if inbound.payload.get("kind").and_then(Value::as_str) != Some("lanes-query") {
                    return;
                }
                let Some(reply_to) = inbound.payload.get("reply_to").and_then(Value::as_str) else {
                    return;
                };
                let request_id = inbound
                    .payload
                    .get("request_id")
                    .cloned()
                    .unwrap_or(Value::Null);
                let reply = json!({
                    "kind": "lanes-reply",
                    "request_id": request_id,
                    "lanes": [
                        { "address": "agent@nostos", "kind": "conductor", "state": "running" },
                        { "address": "agent@nostos/wing-a", "kind": "performer", "state": "running" },
                    ],
                });
                let from = resolve_handle(None);
                let _ = relay_send_to_wld(&hub_addr, reply_to, &from, &reply).await;
            }
        };
        let target = HubClient::connect_with_inbound(&addr, 5, on_query)
            .await
            .expect("target: connect_with_inbound");
        target
            .register(target_wld, &[], "vp-disco-target", "VP discovery target")
            .await
            .expect("target: register");

        // source: federate_discover_lanes（一時 wld_id で temp-register → query → 同接続で reply 受信）。
        let lanes = federate_discover_lanes(&addr, "vp-disco-target")
            .await
            .expect("federate_discover_lanes");

        assert_eq!(lanes.len(), 2, "lane 2 件返るはず: {lanes:?}");
        assert_eq!(lanes[0]["address"], "agent@nostos");
        assert_eq!(lanes[1]["address"], "agent@nostos/wing-a");

        println!(
            "✅ discover-lanes e2e OK: vp-disco-target の lane {} 件を取得",
            lanes.len()
        );
    }
}
