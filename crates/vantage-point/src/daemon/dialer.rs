//! federation direct dialer — ADR-020 §S6 の VP 消費側（discover → direct race → relay floor）。
//!
//! chronista-hub の `worlds.Discover` が返す direct 到達候補
//! （[`WorldEntry::endpoints`]、`["[GUA]:port", ..]`）を club-unison の Happy Eyeballs v2
//! dialer（`ProtocolClient::connect_race`、RFC 8305 staggered race）で並行 dial し、勝った
//! 接続の `wire` channel に envelope を送る。全滅は `Err` — caller
//! （[`super::hub_client::federate_wire_send`]）が relay floor（§S4）に落とす。
//! この「direct 試行 → 全滅で relay」が ADR-020 degrade ladder の consume 側。
//!
//! ## trust（現状と将来）
//!
//! - 全候補 loopback → `SkipVerification`（dev/test。unison 側が loopback 限定を enforce）。
//! - 非 loopback を含む → `System`（webpki）。**現状の TheWorld は dev cert のため remote
//!   direct はここで fast-fail → relay floor に落ちる（= 正しい degradation）**。mesh cert
//!   （PR-3 InternalMeshKeypair）が入ると direct が勝ち始める。cert で挙動が変わるのは
//!   dialer でなく trust の責務（S1 と同型）。
//! - SNI = `wld_id`（位置独立 identity、ADR-020 D2）。将来の mesh cert は SAN=wld_id を
//!   推奨 — 「どの機械に居るか」でなく「どの world か」を検証する（Skip 時は未使用）。

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use unison::network::RaceCfg;

use super::hub_client::WorldEntry;

/// direct 試行の ops kill-switch。`VP_FEDERATION_DIRECT=0|false|off` で無効（default 有効）。
///
/// relay floor は常に生きているため、direct を切っても federation は degrade するだけで
/// 止まらない（churn 調査等で direct を切り分けたい時のノブ）。
pub fn direct_enabled() -> bool {
    match std::env::var("VP_FEDERATION_DIRECT") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        Err(_) => true,
    }
}

/// registry の endpoint 文字列（`"[GUA]:port"`）群を `SocketAddr` に解く純関数。
///
/// 不正 entry は warn + skip — endpoints は他 world の self-report（opaque、D2）なので
/// 1 個の不正で全体を落とさない。
pub fn parse_endpoints(endpoints: &[String]) -> Vec<SocketAddr> {
    endpoints
        .iter()
        .filter_map(|s| match s.parse::<SocketAddr>() {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!("direct endpoint を無視（parse 不能）: {s:?} — {e}");
                None
            }
        })
        .collect()
}

/// federation 送信の direct 試行 race 設定。
///
/// `overall_deadline` を短く抑える（1.5s）のが要 — 全候補 blackhole（firewall drop）の
/// 最悪ケースでも relay floor への追加遅延がこの値で有界になる。cert 不一致 / 接続拒否は
/// handshake が即エラーを返すので deadline を待たない（≈1 RTT）。stagger は RFC 8305 既定。
fn federation_race_cfg() -> RaceCfg {
    RaceCfg {
        stagger: Duration::from_millis(250),
        // 現 cut の候補は direct のみ（relay は race 外の floor、hub relay は §S4 経路）。
        relay_handicap: Duration::from_millis(500),
        overall_deadline: Duration::from_millis(1500),
    }
}

/// `entry.endpoints` へ Happy Eyeballs v2 staggered race で direct QUIC 接続する。
///
/// 勝者の [`unison::ProtocolClient`] を返す。**caller は使用後 `disconnect()` を必ず呼ぶ**
/// （drop 任せは quinn endpoint driver = UDP socket が解放されない。world_wire の
/// 「1 call = 1 fd leak」と同じ罠）。
pub async fn dial_direct(entry: &WorldEntry) -> Result<unison::ProtocolClient> {
    let addrs = parse_endpoints(&entry.endpoints);
    if addrs.is_empty() {
        anyhow::bail!("direct 候補なし（endpoints が空 / 全 entry parse 不能）");
    }
    // trust 選択: 全候補 loopback = dev/test → SkipVerification（unison 側 loopback 限定）。
    // 非 loopback を含む → System（現状 dev cert で fast-fail → caller が relay floor へ）。
    let all_loopback = addrs.iter().all(|a| a.ip().is_loopback());
    let trust = if all_loopback {
        unison::network::TrustAnchors::SkipVerification
    } else {
        unison::network::TrustAnchors::System
    };
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(trust)
        .build()
        .context("QUIC client build 失敗")?;
    let client = unison::ProtocolClient::new(transport);
    // SNI = wld_id（位置独立 identity）。SkipVerification 時は名前検証に使われない。
    client
        .connect_race(addrs, &entry.wld_id, federation_race_cfg())
        .await
        .with_context(|| {
            format!(
                "direct 全滅（wld_id={}, endpoints={:?}）",
                entry.wld_id, entry.endpoints
            )
        })?;
    Ok(client)
}

/// direct 経由で wire envelope を 1 本送る（dial → `wire` channel → `wire/send`）。
///
/// MVP は短命接続（`federate_wire_send` の relay 経路と同型、永続接続の再利用は後の最適化）。
/// 成否に関わらず disconnect でリソースを解放する。受信側 daemon の wire channel はエラーを
/// success frame の `{"error": ...}` で返す（VP-163 慣習）ため、response の `error` key を検査。
pub async fn wire_send_direct(entry: &WorldEntry, envelope: &Value) -> Result<()> {
    let client = dial_direct(entry).await?;
    let result = async {
        let channel = client
            .open_channel("wire")
            .await
            .context("direct: wire channel open 失敗")?;
        let resp: Value = channel
            .request("wire/send", envelope)
            .await
            .context("direct: wire/send request 失敗")?;
        if let Some(err) = resp.get("error") {
            anyhow::bail!("direct: 受信側 wire/send がエラー: {err}");
        }
        Ok(())
    }
    .await;
    // 短命接続: leak 防止のため Ok/Err 共通で必ず disconnect（module doc の罠参照）。
    let _ = client.disconnect().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoints_accepts_bracketed_v6_and_skips_garbage() {
        let eps = vec![
            "[2001:db8::1]:32000".to_string(),
            "not-an-addr".to_string(),
            "[::1]:7879".to_string(),
            "".to_string(),
        ];
        let out = parse_endpoints(&eps);
        assert_eq!(out.len(), 2, "不正 entry は skip、正常 entry は保持");
        assert!(out.iter().all(|a| a.is_ipv6()));
    }

    #[test]
    fn parse_endpoints_empty_is_empty() {
        assert!(parse_endpoints(&[]).is_empty());
    }

    #[test]
    fn direct_enabled_env_semantics() {
        // 未設定 = 有効（default on）。他テストと env を共有しないよう 1 テストに集約し、
        // 終了時に必ず remove する。
        unsafe { std::env::remove_var("VP_FEDERATION_DIRECT") };
        assert!(direct_enabled(), "未設定は有効");
        for off in ["0", "false", "OFF", " off "] {
            unsafe { std::env::set_var("VP_FEDERATION_DIRECT", off) };
            assert!(!direct_enabled(), "{off:?} は無効化");
        }
        unsafe { std::env::set_var("VP_FEDERATION_DIRECT", "1") };
        assert!(direct_enabled(), "1 は有効");
        unsafe { std::env::remove_var("VP_FEDERATION_DIRECT") };
    }
}
