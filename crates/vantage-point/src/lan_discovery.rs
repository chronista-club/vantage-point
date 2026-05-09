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

/// VP-153: mDNS announce で使う hostname を OS の **Bonjour LocalHostName** から取得
///
/// macOS の hostname 三重構造:
/// - **ComputerName**: GUI 表示用 (例 `mito-mac`)
/// - **HostName**: `hostname` command 用 (typically 未設定 / `.local.` 付き)
/// - **LocalHostName**: Bonjour 専用 (例 `mito-mac-2`、 collision で auto-rename)
///
/// `hostname::get()` (= POSIX `gethostname(3)`) は macOS で ComputerName 由来を返すため、
/// OS Bonjour daemon が持つ LocalHostName と乖離 → mDNS register 時に衝突 → user dialog。
/// fix: macOS では `scutil --get LocalHostName` を shell-out で実時刻 LocalHostName 取得、
/// その他 OS では `hostname::get()` で fallback (= cross-platform 互換)。
#[cfg(target_os = "macos")]
pub fn os_local_hostname() -> Option<String> {
    std::process::Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "macos"))]
pub fn os_local_hostname() -> Option<String> {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|s| {
            s.trim_end_matches(".local")
                .trim_end_matches('.')
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

/// VP world の mDNS service type (`_vp._tcp.local.`)
pub const SERVICE_TYPE: &str = "_vp._tcp.local.";

/// mDNS announce の kind (VP-154 PR-1)
///
/// instance_name と TXT record `kind` field を 内部生成、 caller は kind だけ渡す。
/// `World` = machine identity anchor (1 machine 1 個)、 `Sp` = project-scoped data plane
/// (各 project 1 個、 cross-machine forward は SP に直接 1 hop で deliver する設計)。
#[derive(Debug, Clone)]
pub enum AnnounceKind {
    /// TheWorld daemon (= machine identity anchor、 control plane)
    World,
    /// SP (= project-scoped data plane、 mDNS browse で project 単位 resolve)
    Sp { project: String },
}

impl AnnounceKind {
    /// instance_name 生成: `world-<localhost>` or `sp-<project>-<localhost>`
    ///
    /// VP-153 Layer 2 problem 自然解決: instance が OS LocalHostName と **異なる** prefix
    /// (= `world-` / `sp-`) を持つため、 Bonjour 自身の hostname collision に至らず OS dialog 出ない。
    fn instance_name(&self, local_host: &str) -> String {
        match self {
            Self::World => format!("world-{}", local_host),
            Self::Sp { project } => format!("sp-{}-{}", project, local_host),
        }
    }

    /// TXT record の `kind` field 値 (= `"world"` or `"sp"`)
    fn kind_str(&self) -> &str {
        match self {
            Self::World => "world",
            Self::Sp { .. } => "sp",
        }
    }
}

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

/// mDNS で `_vp._tcp.local.` を announce する (VP-154 PR-1: kind-aware)
///
/// - `kind`: [`AnnounceKind::World`] (= machine anchor) or [`AnnounceKind::Sp { project }`] (= project SP)
/// - `port`: API port (= World は 32000、 SP は 33000+)
/// - `pubkey`: TXT record の pubkey field (= P3-4 まで [`PUBKEY_PLACEHOLDER`])
///
/// instance_name は kind から自動生成 (= `world-{localhost}` / `sp-{project}-{localhost}`)、
/// TXT record に `kind` (= `"world"` / `"sp"`) と project (Sp のみ) を含める。
/// 戻り値の [`MdnsAnnouncer`] が drop された時点で deregister される。
pub fn announce(
    kind: AnnounceKind,
    port: u16,
    pubkey: &str,
) -> Result<MdnsAnnouncer, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    // VP-153: macOS Bonjour LocalHostName を真の source of truth とする。 Moody Blues fix #2
    // (Score 77): fallback を `format!("vp-{}", port)` で port-base 一意化、 同 LAN 多台 fallback
    // 時の host 衝突回避 (= 旧 `"unknown"` だと全台同 host で再 collision)。
    let local_host = os_local_hostname().unwrap_or_else(|| format!("vp-{}", port));
    // mDNS hostname は末尾 `.` 必須、 `<localhostname>.local.` 形式
    let host_for_mdns = format!("{}.local.", local_host);
    let instance_name = kind.instance_name(&local_host);

    let mut properties: HashMap<String, String> = HashMap::new();
    properties.insert("kind".to_string(), kind.kind_str().to_string());
    if let AnnounceKind::Sp { project } = &kind {
        properties.insert("project".to_string(), project.clone());
    }
    properties.insert("pubkey".to_string(), pubkey.to_string());
    properties.insert("port".to_string(), port.to_string());
    properties.insert("version".to_string(), PROTOCOL_VERSION.to_string());

    // IP は daemon が auto-detect (`enable_addr_auto`)、 第 4 引数は空文字列で OK。
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
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
        "mDNS announce: kind={} instance={} host={} port={} pubkey={}",
        kind.kind_str(),
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

/// VP-149 PR-1: 永続 mDNS browse の event (= host lifecycle change)
///
/// `start_continuous_browse` の caller (= TheWorld daemon) が tokio channel 経由で受け取り、
/// AddressBook を auto-upsert / auto-remove する。
#[derive(Debug, Clone)]
pub enum LanEvent {
    /// host 発見 / 再 broadcast (= TXT record / port 等の更新含む)
    Discovered(DiscoveredWorld),
    /// host が mDNS から消えた (= daemon shutdown / withdraw goodbye)、 payload は instance_name
    Removed { instance_name: String },
}

/// 永続 mDNS browse を担う background daemon の owner (Drop で auto shutdown)
///
/// std thread で `mdns-sd` の event receiver を listen、 [`LanEvent`] に正規化して
/// `tokio::sync::mpsc::Sender` 経由で caller に push する。 ContinuousBrowser が drop
/// された時点で daemon shutdown が走り、 std thread は recv_timeout 超過で自然終了する。
pub struct ContinuousBrowser {
    daemon: ServiceDaemon,
}

impl Drop for ContinuousBrowser {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.shutdown() {
            tracing::warn!("mDNS continuous browse daemon shutdown 失敗: {}", e);
        }
    }
}

/// 永続 mDNS browse を起動 (VP-149 PR-1)
///
/// 戻り値の [`ContinuousBrowser`] が drop されるまで std thread が `_vp._tcp.local.` を
/// listen し続け、 [`LanEvent::Discovered`] / [`LanEvent::Removed`] を `tx` 経由で push する。
/// caller は tokio task で `rx.recv()` を loop して AddressBook を update する。
///
/// `tx.send` が channel closed でも std thread は loop 継続 (= caller 側の hand-off 不在でも
/// daemon shutdown は ContinuousBrowser drop で trigger される、 thread は次の recv で
/// `RecvError` を観測して exit する設計)。
pub fn start_continuous_browse(
    tx: tokio::sync::mpsc::Sender<LanEvent>,
) -> Result<ContinuousBrowser, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;
    let daemon_for_thread = daemon.clone();

    std::thread::Builder::new()
        .name("vp-mdns-browse".into())
        .spawn(move || {
            tracing::info!("mDNS continuous browse loop 起動: {}", SERVICE_TYPE);
            // 200ms timeout で ServiceDaemon がまだ active か polling、 daemon shutdown で
            // receiver が close された時に loop 終了する
            let poll_timeout = Duration::from_millis(200);
            loop {
                match receiver.recv_timeout(poll_timeout) {
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
                        let world = DiscoveredWorld {
                            hostname,
                            instance_name: instance_name.clone(),
                            port,
                            pubkey,
                            version,
                            properties,
                        };
                        if let Err(e) = tx.blocking_send(LanEvent::Discovered(world)) {
                            tracing::debug!("mDNS browse: send 失敗 (channel closed): {}", e);
                            // channel 閉鎖でも loop は維持 (= ContinuousBrowser drop で daemon
                            // shutdown され、 receiver が err 返すまで)
                        }
                    }
                    Ok(ServiceEvent::ServiceRemoved(_, instance_name)) => {
                        if let Err(e) = tx.blocking_send(LanEvent::Removed { instance_name }) {
                            tracing::debug!(
                                "mDNS browse: removed send 失敗 (channel closed): {}",
                                e
                            );
                        }
                    }
                    Ok(_) => {
                        // ServiceFound / SearchStarted 等は無視
                    }
                    Err(flume::RecvTimeoutError::Timeout) => {
                        // 通常の timeout、 loop 継続
                    }
                    Err(flume::RecvTimeoutError::Disconnected) => {
                        // daemon が shutdown された (= ContinuousBrowser drop)、 thread 終了
                        tracing::info!("mDNS continuous browse loop: daemon disconnected、 終了");
                        break;
                    }
                }
            }
        })
        .map_err(|e| mdns_sd::Error::Msg(format!("mDNS browse thread spawn 失敗: {}", e)))?;

    Ok(ContinuousBrowser {
        daemon: daemon_for_thread,
    })
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
    fn announce_kind_world_instance_name() {
        // VP-154 PR-1: World は `world-<localhost>` (= LocalHostName 直接 announce ではないので
        // OS Bonjour 自身の hostname と collision しない、 VP-153 Layer 2 自然解消)
        let kind = AnnounceKind::World;
        assert_eq!(kind.instance_name("mito-mac-4"), "world-mito-mac-4");
        assert_eq!(kind.kind_str(), "world");
    }

    #[test]
    fn announce_kind_sp_instance_name() {
        // VP-154 PR-1: SP は `sp-<project>-<localhost>` (= per-project unique instance)
        let kind = AnnounceKind::Sp {
            project: "creo-ui".to_string(),
        };
        assert_eq!(kind.instance_name("mito-mac-4"), "sp-creo-ui-mito-mac-4");
        assert_eq!(kind.kind_str(), "sp");
    }

    #[test]
    fn announce_kind_world_and_sp_dont_collide() {
        // 同 LocalHostName で World + 複数 SP が同時 announce しても instance_name は全 unique
        let world = AnnounceKind::World;
        let sp1 = AnnounceKind::Sp {
            project: "creo-ui".to_string(),
        };
        let sp2 = AnnounceKind::Sp {
            project: "vantage-point".to_string(),
        };
        let host = "mito-mac-4";
        let names: std::collections::HashSet<String> = [
            world.instance_name(host),
            sp1.instance_name(host),
            sp2.instance_name(host),
        ]
        .into_iter()
        .collect();
        assert_eq!(names.len(), 3);
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
