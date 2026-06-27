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
//! ## 注意（ADR-018 spike の地雷）
//! - VP は既に Unison native（daemon QUIC server / WorldControlClient）。rustls の
//!   CryptoProvider は VP 既存経路で install 済みのため、ここでの再 install は不要。
//! - `TrustAnchors::SkipVerification` は **INSECURE**（server 証明書未検証、dev/test 専用）。
//!   本番運用で hub を信頼境界越しに置くなら TrustAnchors を明示する必要がある（follow-up）。

use anyhow::{Context, Result};
use serde_json::json;
use unison::ProtocolClient;
use unison::network::channel::UnisonChannel;

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

/// hub の `worlds` channel に接続済みの client。
///
/// WorldControlClient（`daemon/client.rs`）と同じく `ProtocolClient` で QUIC 接続を張り、
/// channel だけを保持する（`open_channel` の戻り channel が接続を内部保持するため、
/// `client` 本体は drop してよい）。
pub struct HubClient {
    ch: UnisonChannel,
}

impl HubClient {
    /// hub Unison surface に接続し `worlds` channel を open する（リトライ付き）。
    ///
    /// `retries` 回まで接続を試み、全失敗なら最後のエラーを返す。caller（register 経路）は
    /// このエラーを warn ログに落として machine-local 動作を継続する（degradation）。
    pub async fn connect(addr: &str, retries: u32) -> Result<Self> {
        // INSECURE dev path: hub の dev cert を検証しない（WorldControlClient と同じ方針）。
        let transport = unison::network::quic::QuicClient::builder()
            .trust_anchors(unison::network::TrustAnchors::SkipVerification)
            .build()
            .context("hub QUIC クライアントの作成に失敗")?;
        let client = ProtocolClient::new(transport);

        let attempts = retries.max(1);
        let mut last_err: Option<String> = None;
        for attempt in 0..attempts {
            match client.connect(addr).await {
                Ok(_) => {
                    let ch = client
                        .open_channel("worlds")
                        .await
                        .map_err(|e| anyhow::anyhow!("worlds チャネル open 失敗: {}", e))?;
                    return Ok(Self { ch });
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
