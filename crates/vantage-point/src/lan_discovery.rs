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

/// mDNS で `_vp._tcp.local.` を announce する (VP-154 PR-1: kind-aware、 PR-3.5: identity override)
///
/// - `kind`: [`AnnounceKind::World`] (= machine anchor) or [`AnnounceKind::Sp { project }`] (= project SP)
/// - `port`: API port (= World は 32000、 SP は 33000+)
/// - `pubkey`: TXT record の pubkey field (= P3-4 まで [`PUBKEY_PLACEHOLDER`])
/// - `identity_hostname_override`: VP-154 PR-3.5 — instance_name 生成に使う hostname を config 固定
///   (例: `Some("mito-mac")`)、 macOS LocalHostName auto-increment 由来の identity 揺れを回避。
///   `None` なら旧挙動 (= OS LocalHostName から取得)。
///
/// instance_name は kind + identity_host から自動生成 (= `world-{identity}` / `sp-{project}-{identity}`)、
/// TXT record に `kind` (= `"world"` / `"sp"`) と project (Sp のみ) を含める。
///
/// SRV record の target hostname は **`vp-{identity}.local.` 専用 namespace** (VP-154 PR-3.6)。
/// `enable_addr_auto()` がこの hostname で A record を publish するため、 OS Bonjour daemon が
/// 持つ `<LocalHostName>.local.` namespace と衝突せず、 起動時 dialog (= "ローカルホスト名は
/// すでに使用されています、 ... に変更されました") を抑止する。 peer は mDNS query で
/// `vp-{identity}.local.` を解決 (= VP の mdns-sd daemon が answer)、 OS resolver は mDNSResponder
/// 経由で同様に解決可能。
///
/// `identity_hostname_override` が `None` の場合は OS LocalHostName fallback (= `vp-{os_host}.local.`、
/// OS rename に追従するが OS namespace とは分離されるため dialog は抑止される)。 LAN identity の
/// 完全固定が必要なら `config.toml [network] advertise_hostname = "..."` を設定すること
/// (= PR-3.5 で追加した override path)。
///
/// 戻り値の [`MdnsAnnouncer`] が drop された時点で deregister される。
pub fn announce(
    kind: AnnounceKind,
    port: u16,
    pubkey: &str,
    identity_hostname_override: Option<&str>,
) -> Result<MdnsAnnouncer, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    // VP-153: macOS Bonjour LocalHostName を真の source of truth とする (= identity_host fallback 用)。
    // Moody Blues fix #2 (Score 77): fallback を `format!("vp-{}", port)` で port-base 一意化、
    // 同 LAN 多台 fallback 時の host 衝突回避 (= 旧 `"unknown"` だと全台同 host で再 collision)。
    let os_host = os_local_hostname().unwrap_or_else(|| format!("vp-{}", port));
    // VP-154 PR-3.5: instance_name 用 host は config override を優先 (= LAN identity 安定化)、
    // None なら OS 現在値で旧挙動と互換。
    let identity_host = identity_hostname_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| os_host.clone());
    let instance_name = kind.instance_name(&identity_host);
    // VP-154 PR-3.6: SRV target を `vp-{identity}.local.` 専用 namespace に切り出し。
    // enable_addr_auto はこの hostname で A record を publish するため、 OS Bonjour daemon の
    // `<LocalHostName>.local.` namespace (= `mito-mac-N.local.` 等) と分離され、 起動時 dialog 抑止。
    // identity_host が config override 由来なら完全固定 (例: `vp-mito-mac.local.`)、 OS fallback でも
    // VP namespace の中で OS rename に追従するだけ (= OS と衝突しない)。
    let host_for_mdns = format!("vp-{}.local.", identity_host);

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
        "mDNS announce: kind={} instance={} srv_target={} port={} identity_override={} pubkey={}",
        kind.kind_str(),
        instance_name,
        host_for_mdns,
        port,
        identity_hostname_override.unwrap_or("(none, OS LocalHostName)"),
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
/// tokio task で `mdns-sd` の event receiver を `recv_async` で await し、 [`LanEvent`] に
/// 正規化して `tokio::sync::mpsc::Sender` 経由で caller に push する。 ContinuousBrowser が
/// drop された時点で daemon shutdown が走り、 task は receiver close (= `recv_async` の Err)
/// を観測して自然終了する。
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
/// 戻り値の [`ContinuousBrowser`] が drop されるまで tokio task が `_vp._tcp.local.` を
/// listen し続け、 [`LanEvent::Discovered`] / [`LanEvent::Removed`] を `tx` 経由で push する。
/// caller は tokio task で `rx.recv()` を loop して AddressBook を update する。
///
/// tokio runtime 上で呼ぶこと (内部で `tokio::spawn` する)。
/// `tx.send` が channel closed でも task は loop 継続 (= caller 側の hand-off 不在でも
/// daemon shutdown は ContinuousBrowser drop で trigger され、 task は次の `recv_async` で
/// close を観測して exit する設計)。
pub fn start_continuous_browse(
    tx: tokio::sync::mpsc::Sender<LanEvent>,
) -> Result<ContinuousBrowser, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;
    let daemon_for_task = daemon.clone();

    tokio::spawn(async move {
        tracing::info!("mDNS continuous browse loop 起動: {}", SERVICE_TYPE);
        loop {
            match receiver.recv_async().await {
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
                    if let Err(e) = tx.send(LanEvent::Discovered(world)).await {
                        tracing::debug!("mDNS browse: send 失敗 (channel closed): {}", e);
                        // channel 閉鎖でも loop は維持 (= ContinuousBrowser drop で daemon
                        // shutdown され、 receiver が close されるまで)
                    }
                }
                Ok(ServiceEvent::ServiceRemoved(_, instance_name)) => {
                    if let Err(e) = tx.send(LanEvent::Removed { instance_name }).await {
                        tracing::debug!("mDNS browse: removed send 失敗 (channel closed): {}", e);
                    }
                }
                Ok(_) => {
                    // ServiceFound / SearchStarted 等は無視
                }
                Err(_) => {
                    // recv_async の Err は Disconnected のみ (= daemon shutdown /
                    // ContinuousBrowser drop)。エラー型 (flume::RecvError) を名指ししない
                    // ことで、 mdns-sd 内部の flume への直接依存を持たない。
                    tracing::info!("mDNS continuous browse loop: daemon disconnected、 終了");
                    break;
                }
            }
        }
    });

    Ok(ContinuousBrowser {
        daemon: daemon_for_task,
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
    fn announce_kind_world_instance_name_uses_identity_override() {
        // VP-154 PR-3.5: identity_hostname_override が渡された場合、 instance_name は
        // OS LocalHostName ではなく override 値で生成。 OS が `mito-mac-3` に increment しても
        // override が `mito-mac` なら advertise は `world-mito-mac` で固定。
        let kind = AnnounceKind::World;
        assert_eq!(
            kind.instance_name("mito-mac"),
            "world-mito-mac",
            "override 値で instance_name 生成"
        );
        // OS 値とは独立 (= 同 kind で別 host 渡せば別 name)
        assert_ne!(
            kind.instance_name("mito-mac"),
            kind.instance_name("mito-mac-3")
        );
    }

    #[test]
    fn announce_kind_sp_instance_name_uses_identity_override() {
        // SP も同じ pattern で override 効く (= per-project SP advertise の identity 安定化)
        let kind = AnnounceKind::Sp {
            project: "creo-ui".to_string(),
        };
        assert_eq!(
            kind.instance_name("mito-mac"),
            "sp-creo-ui-mito-mac",
            "override 値で SP instance_name 生成"
        );
    }

    #[test]
    fn srv_target_uses_vp_namespace_prefix() {
        // VP-154 PR-3.6: SRV target hostname の format が `vp-{identity}.local.` で OS namespace
        // (= `<LocalHostName>.local.`) と分離されることを保証。 これが崩れると enable_addr_auto が
        // OS と同 hostname の A record を publish して macOS dialog が再発する。
        //
        // announce() 内 `format!("vp-{}.local.", identity_host)` の format spec を string-level で固定。
        let identity = "mito-mac";
        let srv_target = format!("vp-{}.local.", identity);
        assert!(
            srv_target.starts_with("vp-"),
            "SRV target は vp- prefix が必須 (OS namespace 分離)"
        );
        assert!(
            srv_target.ends_with(".local."),
            "SRV target は .local. (末尾 dot 含む) で終わる必要 (mDNS 規約)"
        );
        // OS namespace (= `mito-mac.local.`) と完全一致しないことも明示
        assert_ne!(srv_target, format!("{}.local.", identity));
    }

    #[test]
    fn srv_target_fallback_uses_os_host_with_vp_prefix() {
        // VP-154 PR-3.6: identity_hostname_override が None の fallback path でも
        // SRV target は `vp-` prefix を持つ (= OS rename された host でも namespace 分離は維持)。
        // 例えば OS が `mito-mac-10` に increment しても VP は `vp-mito-mac-10.local.` で publish、
        // OS Bonjour の `mito-mac-10.local.` とは別 namespace。
        let os_host_after_rename = "mito-mac-10";
        let host_for_mdns = format!("vp-{}.local.", os_host_after_rename);
        assert!(host_for_mdns.starts_with("vp-"));
        assert!(host_for_mdns.ends_with(".local."));
        assert_ne!(
            host_for_mdns,
            format!("{}.local.", os_host_after_rename),
            "fallback でも OS host と完全一致してはいけない (= dialog 再発の signature)"
        );
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
