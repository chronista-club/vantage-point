//! LAN discovery via mDNS (Bonjour / Avahi 互換、 VP-148 PR-P3-1)
//!
//! VP-144 Epic Phase 3 sub-PR 1。 TheWorld daemon が起動時に `_vp._tcp.local` で
//! mDNS announce、 同 LAN 上の VP world を `discover()` で列挙する。 後続 sub-PR で
//! address book CLI (P3-2)、 cross-machine msg dispatch (P3-3)、 Ed25519 + NaCl
//! encrypt (P3-4)、 bounce 通知 + resolve cascade integration (P3-5) を構築する。
//!
//! ## design
//!
//! - **service type**: `_vp._tcp.local.` (= 4 segment 規約、 末尾 `.` 必須)
//! - **TXT record**: `pubkey` / `port` / `version` の 3 key
//!   - `pubkey`: Ed25519 pubkey fingerprint。 PR-P3-1 では `"pending"` placeholder、 P3-4 で actual 値
//!   - `port`: TheWorld API port (= 32000 etc.)
//!   - `version`: VP mailbox protocol version (例: `"v3"`)
//! - **lifecycle**: [`MdnsAnnouncer`] が drop された時点で deregister + daemon shutdown
//!
//! ## 既存 `discovery.rs` との関係
//!
//! 既存 `crate::discovery` は **TheWorld 中央 registry** (= 同 machine 上の他 Process)
//! の lookup module。 本 module は **同 LAN 上の他 machine** の discovery で scope が異なる。
//! 同 file 統合せず分離した。

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::time::Duration;

/// VP world の mDNS service type (`_vp._tcp.local.`)
pub const SERVICE_TYPE: &str = "_vp._tcp.local.";

/// pubkey TXT record の placeholder (PR-P3-4 で actual Ed25519 fingerprint に置換)
pub const PUBKEY_PLACEHOLDER: &str = "pending";

/// VP mailbox protocol version 表示 (TXT record の `version` field)
pub const PROTOCOL_VERSION: &str = "v3";

/// 単一 LAN world の discovery 結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWorld {
    /// hostname (例: `macbook-a.local.`)、 mDNS が解決した host segment
    pub hostname: String,
    /// mDNS service instance name (= machine name、 例: `macbook-a._vp._tcp.local.`)
    pub instance_name: String,
    /// TheWorld API port
    pub port: u16,
    /// Ed25519 pubkey fingerprint (PR-P3-4 まで `"pending"` placeholder)
    pub pubkey: String,
    /// VP mailbox protocol version (例: `"v3"`)
    pub version: String,
    /// 全 TXT record (debug / future expansion 用)
    pub properties: HashMap<String, String>,
}

/// mDNS service announcement の owner — drop で自動 deregister + daemon shutdown
///
/// Daemon startup で `lan_discovery::announce(...)?` の戻り値を保持し、
/// shutdown 時に Drop trait で cleanup される (= explicit unregister 不要)。
pub struct MdnsAnnouncer {
    daemon: ServiceDaemon,
    full_name: String,
}

impl Drop for MdnsAnnouncer {
    fn drop(&mut self) {
        // best-effort cleanup、 error は warn のみ (drop で panic しない)
        if let Err(e) = self.daemon.unregister(&self.full_name) {
            tracing::warn!("mDNS unregister 失敗 ({}): {}", self.full_name, e);
        }
        if let Err(e) = self.daemon.shutdown() {
            tracing::warn!("mDNS daemon shutdown 失敗: {}", e);
        }
    }
}

/// mDNS で `_vp._tcp.local.` を announce する
///
/// - `instance_name`: service の display name (= machine name や user choice)
/// - `port`: TheWorld API port
/// - `pubkey`: TXT record の pubkey field (= P3-4 まで [`PUBKEY_PLACEHOLDER`])
///
/// 戻り値の [`MdnsAnnouncer`] が drop された時点で deregister される。
pub fn announce(
    instance_name: &str,
    port: u16,
    pubkey: &str,
) -> Result<MdnsAnnouncer, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    // mDNS hostname は末尾 `.` 必須、 `.local.` を suffix にする
    let host_for_mdns = if hostname.ends_with('.') {
        hostname.clone()
    } else if hostname.ends_with(".local") {
        format!("{}.", hostname)
    } else {
        format!("{}.local.", hostname)
    };

    let mut properties: HashMap<String, String> = HashMap::new();
    properties.insert("pubkey".to_string(), pubkey.to_string());
    properties.insert("port".to_string(), port.to_string());
    properties.insert("version".to_string(), PROTOCOL_VERSION.to_string());

    // IP は daemon が auto-detect (`enable_addr_auto`)、 第 4 引数は空文字列で OK。
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        &host_for_mdns,
        "",
        port,
        properties,
    )?
    .enable_addr_auto();
    // Moody Blues fix #1 (Score 78): full_name は ServiceInfo が内部で escape する fullname を
    // 採用する (= 手動 format で `.` escape を漏らすと unregister 時 NotFound になる潜在 bug)。
    let full_name = info.get_fullname().to_string();
    daemon.register(info)?;

    tracing::info!(
        "mDNS announce: instance={} host={} port={} pubkey={}",
        instance_name,
        host_for_mdns,
        port,
        pubkey
    );
    Ok(MdnsAnnouncer { daemon, full_name })
}

/// 同 LAN 上の VP world を `timeout_ms` 以内に列挙
///
/// **blocking call** (= `recv_timeout` loop)。 async context (tokio runtime) から
/// 直接呼ぶと worker thread を block するため、 `tokio::task::spawn_blocking` で
/// wrap して呼ぶこと。 同期 caller (= P3-2 CLI、 std thread) からは直接 OK。
///
/// `mdns-sd` の `browse` で query を broadcast、 `ServiceResolved` event を集約して
/// [`DiscoveredWorld`] list を返す。 重複 entry (= 同 instance_name) は最後の値に上書きされる。
pub fn discover(timeout_ms: u64) -> Result<Vec<DiscoveredWorld>, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut found: HashMap<String, DiscoveredWorld> = HashMap::new();

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let hostname = info.get_hostname().to_string();
                let instance_name = info.get_fullname().to_string();
                let port = info.get_port();
                let properties: HashMap<String, String> = info
                    .get_properties()
                    .iter()
                    .map(|p| (p.key().to_string(), p.val_str().to_string()))
                    .collect();
                let pubkey = properties
                    .get("pubkey")
                    .cloned()
                    .unwrap_or_else(|| PUBKEY_PLACEHOLDER.to_string());
                let version = properties
                    .get("version")
                    .cloned()
                    .unwrap_or_else(|| PROTOCOL_VERSION.to_string());
                found.insert(
                    instance_name.clone(),
                    DiscoveredWorld {
                        hostname,
                        instance_name,
                        port,
                        pubkey,
                        version,
                        properties,
                    },
                );
            }
            Ok(_) => {
                // ServiceFound / SearchStarted / ServiceRemoved 等は無視
                // (= ServiceResolved だけが TXT record + port を含む完全 data)
            }
            Err(_) => break, // timeout
        }
    }

    if let Err(e) = daemon.shutdown() {
        tracing::warn!("mDNS discover daemon shutdown 失敗: {}", e);
    }
    Ok(found.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_type_format_is_valid_mdns() {
        // mDNS service type の規約: `_<service>._<protocol>.<domain>.` (末尾 `.` 必須、 4 segments)
        // `_vp._tcp.local.` を `.` で split すると ["_vp", "_tcp", "local", ""] の 4 elements
        assert!(SERVICE_TYPE.ends_with('.'));
        assert_eq!(SERVICE_TYPE.split('.').count(), 4);
        assert!(SERVICE_TYPE.starts_with("_vp._tcp."));
        assert!(SERVICE_TYPE.contains(".local."));
    }

    #[test]
    fn pubkey_placeholder_is_distinct_from_actual_pubkey() {
        // P3-4 で Ed25519 fingerprint (`ed25519:` prefix) に置換される
        assert!(!PUBKEY_PLACEHOLDER.contains(':'));
        assert_eq!(PUBKEY_PLACEHOLDER, "pending");
    }

    #[test]
    fn protocol_version_matches_mailbox_v3() {
        // docs/spec/mailbox-address-v3.md の v3 と整合
        assert_eq!(PROTOCOL_VERSION, "v3");
    }

    #[test]
    fn discovered_world_has_required_fields() {
        // struct shape の verification、 future expansion で field 削除を検出
        let w = DiscoveredWorld {
            hostname: "macbook-a.local.".to_string(),
            instance_name: "macbook-a._vp._tcp.local.".to_string(),
            port: 32000,
            pubkey: PUBKEY_PLACEHOLDER.to_string(),
            version: PROTOCOL_VERSION.to_string(),
            properties: HashMap::new(),
        };
        assert_eq!(w.port, 32000);
        assert_eq!(w.pubkey, "pending");
    }

    // mDNS announce / discover の integration test は OS multicast が必要 (CI 環境では unreliable)、
    // unit test で API spec のみ検証。 dogfood は mac × mac で `vp world list --lan` (P3-2) で実施。
}
