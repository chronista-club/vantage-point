//! chronista-hub への Unison client — VP の実 world を hub registry に register / discover する。
//!
//! ## 責務分担（prior art: mem_1CaVeTysipdgVHoxwxUcPj / mem_1Cc1dA79VZu586fjqafiBS）
//! - **SSOT**: hub への register は **TheWorld 経由のみ**。個別 SP / performer は hub と直接話さない。
//! - **opt-in**: hub addr（env `CHRONISTA_HUB_ADDR` > config.kdl `hub-addr`、[`hub_addr()`] が解決）
//!   未設定なら全 skip（= machine-local 動作）。常設運用は config.kdl 側（launchd daemon は env を持たない）。
//! - **degradation**: hub down でも world は machine-local で動き続ける（federation 機能だけ失う）。
//!
//! ## connection-level auth（ADR-020 §S3、credential = Creo ID user-jwt）
//! - 接続時に [`hub_credential`]（= `~/.vp/credentials.json` の access_token、期限が近ければ
//!   refresh_token で proactive に巻き直す）が有れば `connect_with_credential` で提示、無ければ
//!   credential なしで接続（graceful degrade）。
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

/// hub registry に居る available worlds の cache（`/api/health` の `hub_worlds` field の SSOT）。
///
/// writer = [`run_hub_federation`]（接続直後 + 定期 discover で更新、切断で clear）、
/// reader = `/api/health` handler。中身は [`available_worlds`] 適用済（自 world 除外・
/// handle dedup 済）の list。読み書きとも await を跨がない短い critical section なので
/// `std::sync::RwLock` で足りる（[`HubFederationStatus`] の AtomicU8 は enum 1 値だから
/// 成立する手で、可変長 list には使えない）。
#[derive(Clone, Default)]
pub struct HubWorldsCache(Arc<std::sync::RwLock<Vec<WorldEntry>>>);

impl HubWorldsCache {
    /// 初期状態 = 空（hub 未接続相当）。
    pub fn new() -> Self {
        Self::default()
    }

    /// discover 結果を反映する（writer = [`run_hub_federation`]）。
    pub fn set(&self, worlds: Vec<WorldEntry>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = worlds;
    }

    /// hub 切断時に空へ戻す（stale list を「available」と見せない）。
    pub fn clear(&self) {
        self.set(Vec::new());
    }

    /// 現在の available worlds（reader = `/api/health` handler）。
    pub fn get(&self) -> Vec<WorldEntry> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
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
    /// この world の常駐接続が hub で今生きているか（hub protocol v0.6.0 の additive field、
    /// relay registry snapshot 由来）。false = registry には居るが relay は offline を返す
    /// （stale entry / 切断中）。旧 hub は field を返さないため `#[serde(default)]` で false。
    #[serde(default)]
    pub connected: bool,
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

/// hub credential の refresh を先回りさせる skew（= 失効の何秒前から refresh/再接続するか）。
///
/// connect 経路の [`hub_credential`]（reactive refresh）と常駐ループの proactive 再接続
/// （[`run_hub_federation`]）の**両方が同じ値**を使う。両者が同一だからこそ「proactive 再接続 →
/// その connect で refresh 発火」が噛み合う（別値だと再接続しても token がまだ skew 外で refresh
/// されず、同じ deadline で即再接続する storm になる）。旧値 300s は失効直前すぎたため、日和見
/// reconnect が無い安定接続でも余裕を持って巻き直せるよう 30 分に広げた。
const HUB_REFRESH_SKEW_SECS: u64 = 30 * 60;

/// proactive 再接続までの残り時間を計算する（純関数 = data/calc、I/O 非依存でテスト可能）。
///
/// token 失効の `skew_secs` 前に 1 度だけ再接続し、その connect で [`hub_credential`] の
/// `credentials_refreshed_if_needed` が refresh_token で巻き直すよう促すための deadline。
/// - `expires_at` が `None`（期限不明）→ `None`（arm しない = reactive refresh に委ねる）。
/// - 失効まで skew より長い余裕がある → `Some(残り Duration)`。
/// - 既に skew 内（refresh 失敗で expiry が進まなかった場合を含む）→ `None`（arm しない）。
///   これにより「再接続 → refresh 失敗 → 即再々接続」の storm を構造的に防ぐ。
fn proactive_reconnect_delay(
    expires_at: Option<u64>,
    now: u64,
    skew_secs: u64,
) -> Option<Duration> {
    let deadline = expires_at?.checked_sub(skew_secs)?; // 再接続すべき絶対時刻（unix secs）
    let remaining = deadline.checked_sub(now)?; // 現在からの残り秒（deadline が過去なら None）
    (remaining > 0).then(|| Duration::from_secs(remaining))
}

/// 現在の unix epoch 秒（action = clock 読み）。
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 稼働中 daemon が保持する credential の失効時刻（unix secs、action = file 読み）。
///
/// proactive 再接続 deadline の計算に使う。file 不在 / parse 失敗 / expires_at 欠落は `None`
/// （= proactive を arm せず reactive refresh に委ねる、穏当な degrade）。
///
/// refresh が失敗して expires_at が進まなかった場合、次の connect 時点で既に skew 内なので
/// [`proactive_reconnect_delay`] が `None` を返し、proactive は**再武装されない** — 以降の
/// refresh 機会は次の自然な切断 / 再接続（reactive 経路）に委ねられる。
fn hub_credential_expires_at() -> Option<u64> {
    crate::commands::auth::read_credentials()
        .ok()
        .flatten()
        .and_then(|c| c.expires_at)
}

/// `Some(deadline)` ならその時刻に発火、`None` なら永久 pending（`select!` の他 arm に委ねる）。
///
/// proactive 再接続 timer を「期限不明なら無効」にするための optional future ラッパ。
async fn sleep_until_optional(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending::<()>().await,
    }
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
/// token が期限に近ければ **refresh_token で proactive に巻き直してから** credential を返す。
/// refresh の可否・fail-safe 挙動は [`crate::commands::auth::credentials_refreshed_if_needed`]
/// 参照。24h token が required hub 接続中に切れて federation が止まるのを防ぐ（利便性改善）。skew は
/// [`HUB_REFRESH_SKEW_SECS`]（proactive 再接続と共有 = 「再接続 → その connect で refresh」を噛み合わせる）。
async fn hub_credential() -> Option<Vec<u8>> {
    match crate::commands::auth::credentials_refreshed_if_needed(HUB_REFRESH_SKEW_SECS).await {
        Ok(creds) => credential_from_creds(creds),
        // file 読み込み / refresh 構築失敗（壊れた file 等）は credential なし扱い。federation を
        // 止めるより observe/permissive で繋ぐ方が degrade として穏当。
        Err(e) => {
            tracing::debug!(
                "hub credential 読み込み失敗（credential なしで接続）: {}",
                e
            );
            None
        }
    }
}

/// hub の応答 payload が in-band error（handler の `Err` を `{"error": "..."}` で返す形。
/// chronista-hub `unison_server.rs` の `unwrap_or_else(|e| json!({ "error": ... }))`）なら、その
/// message を返す（純関数）。
///
/// FEDERATION_AUTH=required 下で credential なし（token 失効 / 未ログイン）の Register / Discover は
/// **protocol error でなく通常 Response の payload** で `{"error": "authentication required (scope
/// '...')"}` を返す。素朴な型変換だと「レスポンスのパースに失敗」に潰れて（Discover は "worlds"
/// キー欠落で無言の空配列に潰れて）診断不能になるため、呼び出し側はこれを見て明示的な auth
/// エラーへ浮かせる。
fn hub_reply_error(resp: &Value) -> Option<&str> {
    resp.get("error").and_then(Value::as_str)
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
        // hub は auth 拒否等の handler Err を通常 Response の `{"error": ...}` で返す（in-band）。
        // WorldEntry 化の前に判定し、FEDERATION_AUTH=required 下の失効/未ログインを「parse 失敗」で
        // 潰さず明示エラーに浮かせる（2026-07-11 の federation 途絶で診断を数手遠回りさせた元凶）。
        if let Some(err) = hub_reply_error(&resp) {
            anyhow::bail!(
                "hub が Register を拒否しました（FEDERATION_AUTH=required 下では token 失効 / 未ログイン\
                 が主因 — `vp auth login` で再ログインを検討）: {err}"
            );
        }
        serde_json::from_value::<WorldEntry>(resp.clone())
            .with_context(|| format!("Register レスポンスのパースに失敗（応答: {resp}）"))
    }

    /// hub registry に居る world 一覧を取得する（`worlds.Discover`）。
    pub async fn discover(&self) -> Result<Vec<WorldEntry>> {
        let resp: serde_json::Value = self
            .ch
            .request("Discover", &json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("worlds.Discover 失敗: {}", e))?;
        // Register と同様、in-band の `{"error": ...}` 拒否を判定する。Discover は "worlds" キー欠落を
        // 空配列に潰すため、判定しないと auth 拒否が「discover 0 件」に化けて無言で誤誘導する。
        if let Some(err) = hub_reply_error(&resp) {
            anyhow::bail!(
                "hub が Discover を拒否しました（FEDERATION_AUTH=required 下では token 失効 / 未ログイン\
                 が主因 — `vp auth login` で再ログインを検討）: {err}"
            );
        }
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
    let mut credential = hub_credential().await;
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

/// `discover` で `handle` → 採用 entry を引く（不在 / wld_id 空はエラー）。曖昧性回避のため
/// federation 宛先は handle 明示で受け、ここで routing key（wld_id）と direct 候補
/// （endpoints — §S6 dialer が消費）に解決する。
async fn discover_entry_by_handle(client: &HubClient, handle: &str) -> Result<WorldEntry> {
    let worlds = client.discover().await.context("discover に失敗")?;
    let target = pick_latest_by_handle(&worlds, handle).ok_or_else(|| {
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
    Ok(target.clone())
}

/// handle 一致の world から採用する 1 件を選ぶ純関数 — **registered_at 最新を選ぶ**。
///
/// registry には同一 handle の stale entry が残留し得る（daemon 再作成による wld_id 変化後の
/// 再 register、hub 側 cleanup 前 等）。先頭一致だと stale（死んだ wld_id）を拾って relay が
/// offline で失敗する（2026-07-04 実害: 別 PC の discover が mito-mba.local の stale を掴んだ）。
/// `registered_at` は ISO 8601（同一フォーマット）なので辞書順比較 = 時系列比較。
fn pick_latest_by_handle<'a>(worlds: &'a [WorldEntry], handle: &str) -> Option<&'a WorldEntry> {
    worlds
        .iter()
        .filter(|w| w.handle == handle)
        .max_by(|a, b| a.registered_at.cmp(&b.registered_at))
}

/// discovery 一時 register（[`federate_discover_lanes`] の短命 identity）の handle。
/// available worlds 表示（[`available_worlds`]）からは transient ノイズとして除外する。
pub const TRANSIENT_DISCO_HANDLE: &str = "vp-disco";

/// discover 結果から「hub の向こうに居る available worlds」を選ぶ純関数。
///
/// - **自 world（`own_handle`）を除外** — この list の意味論は「hub の向こうに誰がいるか」
/// - 空 handle と discovery 一時 register（[`TRANSIENT_DISCO_HANDLE`]）を除外（表示ノイズ）
/// - 同一 handle は registered_at 最新の 1 件に dedup（stale 残留対策、[`pick_latest_by_handle`]
///   と同基準 — registered_at は ISO 8601 なので辞書順比較 = 時系列比較）
/// - handle 昇順で返す（表示の安定化）
pub fn available_worlds(worlds: Vec<WorldEntry>, own_handle: &str) -> Vec<WorldEntry> {
    let mut by_handle: std::collections::BTreeMap<String, WorldEntry> =
        std::collections::BTreeMap::new();
    for w in worlds {
        if w.handle.is_empty() || w.handle == own_handle || w.handle == TRANSIENT_DISCO_HANDLE {
            continue;
        }
        match by_handle.get(&w.handle) {
            Some(cur) if cur.registered_at >= w.registered_at => {}
            _ => {
                by_handle.insert(w.handle.clone(), w);
            }
        }
    }
    by_handle.into_values().collect()
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

/// 遠方 world の lane へ wire envelope を送る（flow ③ = federation 送信、daemon-side）。
///
/// SSOT 原則（hub と話すのは TheWorld のみ）に従い、CLI ではなく **TheWorld の wire channel handler**
/// （`handle_wire_channel` の `wire/federate` method）から呼ぶ。MVP は短命接続:
/// connect → `discover` で `target_world_handle` → entry（wld_id + endpoints）解決 →
/// **direct 試行（§S6 HEv2 race）→ 全滅で relay floor（§S4）** — ADR-020 degrade ladder の
/// consume 側。受信 world はどちらの経路でも `dispatch_wire("send")` でローカル配送する
/// （direct = wire channel 直、relay = hub 経由 inbound handler、payload 形は同一）。
/// `from_label` は relay の `open{from}` に載るが受信側は envelope の `from` を使うため
/// cosmetic。永続接続の再利用は後の最適化。
pub async fn federate_wire_send(
    hub_addr: &str,
    target_world_handle: &str,
    from_label: &str,
    envelope: &Value,
) -> Result<()> {
    let client = HubClient::connect(hub_addr, 3)
        .await
        .context("federate-send: hub 接続に失敗")?;
    let entry = discover_entry_by_handle(&client, target_world_handle)
        .await
        .context("federate-send")?;
    // §S6: direct 試行（endpoints あり && kill-switch 有効）。全滅は relay floor に degrade
    // — direct の失敗は「エラー」でなく ladder の一段（warn でなく info）。
    if super::dialer::direct_enabled() && !entry.endpoints.is_empty() {
        match super::dialer::wire_send_direct(&entry, envelope).await {
            Ok(()) => {
                tracing::info!(wld_id = %entry.wld_id, "federation send via=direct");
                return Ok(());
            }
            Err(e) => {
                tracing::info!(wld_id = %entry.wld_id, "direct 全滅 → relay floor へ: {e:#}");
            }
        }
    }
    relay_send_on(&client, &entry.wld_id, from_label, envelope)
        .await
        .context("federate-send")?;
    tracing::info!(wld_id = %entry.wld_id, "federation send via=relay");
    Ok(())
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
        .register(
            &temp_wld,
            &[],
            TRANSIENT_DISCO_HANDLE,
            "VP discovery (transient)",
        )
        .await
        .context("discover-lanes: 一時 register に失敗")?;

    let target_wld = discover_entry_by_handle(&client, target_world_handle)
        .await
        .context("discover-lanes")?
        .wld_id;
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
/// が world status 横の `Hub ● connected` インジケータとして表示する。available worlds は
/// `worlds`（[`HubWorldsCache`]）に反映し、同じく `/api/health`（`hub_worlds` field）経由で
/// vp-app が Hub 行の「· N worlds」として表示する。
// federation session の固有パラメータ（identity 3 つ + status/worlds/shutdown/handler）。いずれも
// 独立に必要で struct 化しても可読性が上がらないため allow（caller は run_world の 1 箇所のみ）。
#[allow(clippy::too_many_arguments)]
pub async fn run_hub_federation<F, Fut>(
    addr: String,
    wld_id: String,
    endpoints: Vec<String>,
    handle: String,
    name: String,
    status: HubFederationStatus,
    worlds: HubWorldsCache,
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
    // available worlds の定期 discover 間隔（hub への負荷を考え短くしすぎない、30-60s レンジ）。
    const DISCOVER_INTERVAL: Duration = Duration::from_secs(45);
    // discover RPC の応答上限。QUIC 面 wedge 等で request が返らない場合に select loop
    //（= Disconnected 検知）を塞がないための保険。
    const DISCOVER_TIMEOUT: Duration = Duration::from_secs(10);

    while !shutdown.is_cancelled() {
        // この iteration の再接続が planned（proactive な token 巻き直し）か否か。planned なら末尾の
        // backoff を飛ばして即再接続する（下記）。
        let mut planned_reconnect = false;
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
                // discover_tick の初回 tick は即時発火 → connect(+再接続)直後の即 discover を兼ねる。
                let mut events = client.subscribe_connection_events();
                // proactive refresh: この接続で提示した token の失効 skew 前に 1 度だけ再接続し、次の
                // connect で credential を巻き直させる（安定接続が失効 token を運び続けるのを防ぐ +
                // refresh_token の idle 失効も防ぐ）。単一 refresh 点（connect 経路）を保つため inner
                // では refresh せず「再接続を促す」だけに留める。期限不明なら arm せず reactive に委ねる。
                let proactive_deadline = proactive_reconnect_delay(
                    hub_credential_expires_at(),
                    unix_now(),
                    HUB_REFRESH_SKEW_SECS,
                )
                .map(|d| tokio::time::Instant::now() + d);
                let mut discover_tick = tokio::time::interval(DISCOVER_INTERVAL);
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
                        _ = sleep_until_optional(proactive_deadline) => {
                            tracing::info!(
                                "chronista-hub credential 失効が近い — proactive に再接続して token を巻き直す"
                            );
                            planned_reconnect = true;
                            break;
                        }
                        _ = discover_tick.tick() => {
                            match tokio::time::timeout(DISCOVER_TIMEOUT, client.discover()).await {
                                Ok(Ok(list)) => {
                                    let total = list.len();
                                    let avail = available_worlds(list, &handle);
                                    tracing::debug!(
                                        "hub discover: registry {} 件 → available {} 件",
                                        total,
                                        avail.len()
                                    );
                                    worlds.set(avail);
                                }
                                // 一過性 error では cache を消さない（真の切断は Disconnected event /
                                // HEALTH_POLL 側が検知して下の clear に到達する）。
                                Ok(Err(e)) => tracing::warn!(
                                    "hub discover 失敗（cache 維持、次周期で再試行）: {}",
                                    e
                                ),
                                Err(_) => tracing::warn!(
                                    "hub discover timeout（{}s、cache 維持、次周期で再試行）",
                                    DISCOVER_TIMEOUT.as_secs()
                                ),
                            }
                        }
                    }
                }
                // 切断検知 → Disconnected を反映（次 iteration 冒頭で Connecting に戻る）。
                // available worlds も clear（未接続の間 stale list を「available」と見せない）。
                status.set(HubFederationState::Disconnected);
                worlds.clear();
                // client drop → connection close。
            }
            Err(e) => tracing::warn!(
                "chronista-hub 接続失敗（{}秒後に再試行、machine-local で継続）: {} (addr={})",
                RECONNECT_BACKOFF.as_secs(),
                e,
                addr
            ),
        }

        // backoff（shutdown で即中断可能）。planned な再接続（proactive な token 巻き直し）は hub
        // 健全が前提なので backoff を挟まず即再接続し、relay 受信 gap を最小化する。
        if !planned_reconnect {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(RECONNECT_BACKOFF) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proactive_reconnect_delay_arms_only_with_future_headroom() {
        let skew = HUB_REFRESH_SKEW_SECS;
        let now = 1_000_000;
        // 期限不明 → arm しない（reactive refresh に委ねる）。
        assert_eq!(proactive_reconnect_delay(None, now, skew), None);
        // 失効まで skew より十分先 → 「失効 skew 前」までの残りを返す。
        // exp = now + skew + 3600 なら deadline = now + 3600 → 残り 3600s。
        assert_eq!(
            proactive_reconnect_delay(Some(now + skew + 3600), now, skew),
            Some(Duration::from_secs(3600))
        );
        // ちょうど skew 手前（deadline == now）→ 残り 0 は arm しない。
        assert_eq!(proactive_reconnect_delay(Some(now + skew), now, skew), None);
        // 既に skew 内（refresh 失敗で expiry が進まなかった場合を含む）→ arm しない = storm 防止。
        assert_eq!(proactive_reconnect_delay(Some(now + 60), now, skew), None);
        // 既に失効済み → arm しない（次の自然な reconnect が refresh を撃つのに委ねる）。
        assert_eq!(proactive_reconnect_delay(Some(now - 10), now, skew), None);
    }

    #[test]
    fn hub_reply_error_detects_in_band_rejection() {
        // hub の auth 拒否は通常 Response の {"error": ...} で来る（unison_server.rs の unwrap_or_else）。
        let rejected = json!({ "error": "authentication required (scope 'federation.register')" });
        assert_eq!(
            hub_reply_error(&rejected),
            Some("authentication required (scope 'federation.register')")
        );
        // 成功応答（WorldEntry 形）は error なし → None（通常の型変換に進む）。
        let ok = json!({ "handle": "mito-mba.local", "registered_at": "2026-07-11T14:55:16Z" });
        assert_eq!(hub_reply_error(&ok), None);
        // error が文字列でない異常形も None（誤検出しない）。
        assert_eq!(hub_reply_error(&json!({ "error": 42 })), None);
    }

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

    /// テスト用 WorldEntry を最小構成で作る。
    fn entry(handle: &str, wld_id: &str, registered_at: &str) -> WorldEntry {
        WorldEntry {
            wld_id: wld_id.to_string(),
            endpoints: vec![],
            handle: handle.to_string(),
            name: String::new(),
            registered_at: registered_at.to_string(),
            connected: false,
        }
    }

    #[test]
    fn pick_latest_by_handle_prefers_newest_registration() {
        // stale（旧 wld）が先頭でも registered_at 最新の現役 entry を選ぶ（2026-07-04 実害の再現形）。
        let worlds = vec![
            entry("mba.local", "wld_stale", "2026-07-03T10:38:33.760655172Z"),
            entry("other.local", "wld_other", "2026-07-03T23:15:30.389430772Z"),
            entry("mba.local", "wld_live", "2026-07-03T23:00:07.784137934Z"),
        ];
        assert_eq!(
            pick_latest_by_handle(&worlds, "mba.local").map(|w| w.wld_id.as_str()),
            Some("wld_live")
        );
        // 一致 1 件ならそれを返す
        assert_eq!(
            pick_latest_by_handle(&worlds, "other.local").map(|w| w.wld_id.as_str()),
            Some("wld_other")
        );
        // 一致なしは None
        assert!(pick_latest_by_handle(&worlds, "nowhere.local").is_none());
    }

    #[test]
    fn available_worlds_excludes_self_and_transient_noise() {
        // 自 world・discovery 一時 register（vp-disco）・空 handle は「hub の向こうに誰が
        // いるか」の意味論から外れる表示ノイズ → 全て除外される。
        let worlds = vec![
            entry("mito-mba.local", "wld_self", "2026-07-12T00:00:00Z"),
            entry("other.local", "wld_other", "2026-07-12T00:00:01Z"),
            entry(TRANSIENT_DISCO_HANDLE, "wld_disco", "2026-07-12T00:00:02Z"),
            entry("", "wld_anon", "2026-07-12T00:00:03Z"),
        ];
        let avail = available_worlds(worlds, "mito-mba.local");
        assert_eq!(
            avail.iter().map(|w| w.handle.as_str()).collect::<Vec<_>>(),
            vec!["other.local"]
        );
    }

    #[test]
    fn available_worlds_dedups_by_handle_keeping_latest() {
        // 同一 handle の stale 残留（daemon 再作成後の再 register 等）は registered_at 最新の
        // 1 件に畳む（pick_latest_by_handle と同基準）。返り順は handle 昇順（表示安定化）。
        let worlds = vec![
            entry("b.local", "wld_b_stale", "2026-07-10T00:00:00Z"),
            entry("a.local", "wld_a", "2026-07-11T00:00:00Z"),
            entry("b.local", "wld_b_live", "2026-07-12T00:00:00Z"),
        ];
        let avail = available_worlds(worlds, "self.local");
        assert_eq!(
            avail
                .iter()
                .map(|w| (w.handle.as_str(), w.wld_id.as_str()))
                .collect::<Vec<_>>(),
            vec![("a.local", "wld_a"), ("b.local", "wld_b_live")]
        );
    }

    #[test]
    fn hub_worlds_cache_set_get_clear() {
        // set → get で反映、clear で空へ（切断時に stale を「available」と見せない根拠）。
        let cache = HubWorldsCache::new();
        assert!(cache.get().is_empty());
        cache.set(vec![entry("a.local", "wld_a", "2026-07-12T00:00:00Z")]);
        assert_eq!(cache.get().len(), 1);
        cache.clear();
        assert!(cache.get().is_empty());
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
