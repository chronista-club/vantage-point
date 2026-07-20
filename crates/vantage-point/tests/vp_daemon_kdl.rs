//! vp-daemon.kdl の drift 防止テスト（VP × unison-mcp Phase 1）。
//!
//! `schema/vp-daemon.kdl` は daemon channel の wire protocol を記述する KDL で、
//! `enable_discovery` が unison.discovery channel から配信する。記述（KDL）と実装
//! （server.rs の register_channel / match method）が乖離すると、unison-mcp が合成する
//! typed tool が「discover には出るが叩いても届かない dead tool」になる。本テストで検出する。
//!
//! テストリスト（t-wada 流、全て Small = 純 parse / assert、I/O なし）:
//!   1. ProtocolCache::new（= enable_discovery と同経路）で parse green
//!   2. KDL の channel 名 ⊆ daemon が register_channel する集合（channel レベル drift 検出）
//!   3. starter channel（registry / events）が KDL に存在する
//!   4. registry / events の request 名 ⊆ daemon handler の match method 集合
//!      （method レベル drift 検出 = 文字列一致しない request は dead tool になる）
//!
//! SSOT: crates/vantage-point/src/daemon/server.rs の register_channel(...) と各 match method。
//! server.rs を変えたら本テストの const も同期する（method レベルは手動同期の gate）。

use unison::network::{ProtocolCache, SchemaRegistry};

/// vp-daemon channel の wire protocol KDL（enable_discovery が配信する SSOT）。
const VP_DAEMON_KDL: &str = include_str!("../schema/vp-daemon.kdl");

/// daemon が register_channel する全 channel 名。
/// SSOT: server.rs の `register_channel("<name>", ...)` 群。
///
/// doc 44 P1 (fold-in) で `"control"` channel handler を削除した際、この const だけが
/// 取り残された（下の `channels_are_subset_of_daemon_registered` は KDL → const の
/// 片方向しか見ないため、実装から channel が消えても green のままだった）。
/// 現在は [`daemon_channels_const_matches_source`] が source と突き合わせるので、
/// server.rs 側の増減はテスト失敗として現れる。
const DAEMON_CHANNELS: &[&str] = &[
    "world-process",
    "lanes",
    "canvas-ingest",
    "canvas",
    "process-proxy",
    "world-device",
    "device",
    "registry",
    "events",
    "world-control",
    "wire",
];

/// daemon server の実ソース。`DAEMON_CHANNELS` の突き合わせ相手。
const DAEMON_SERVER_RS: &str = include_str!("../src/daemon/server.rs");

/// `DAEMON_CHANNELS` を source から抽出した実態と突き合わせる。
///
/// この const は手動同期で、以前は「実装から channel が消えても誰も気付けない」
/// 状態だった（fold-in で `"control"` を削除した時に実際に取り残された）。
/// source を直接読むことで drift を双方向に検出する。
#[test]
fn daemon_channels_const_matches_source() {
    let found: std::collections::BTreeSet<&str> = DAEMON_SERVER_RS
        .split(r#"register_channel(""#)
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    let declared: std::collections::BTreeSet<&str> = DAEMON_CHANNELS.iter().copied().collect();

    assert_eq!(
        found, declared,
        "DAEMON_CHANNELS が server.rs の register_channel 実態とずれている\
         （channel を増減したら本 const も更新すること）"
    );
}

fn registry() -> SchemaRegistry {
    SchemaRegistry::from_kdl(VP_DAEMON_KDL)
        .expect("vp-daemon.kdl は club-unison の SchemaParser で green にパースできる必要がある")
}

/// 1. enable_discovery と同経路（ProtocolCache::new）で parse green。
///    本番で discovery を有効化する瞬間に KDL が落ちる回帰を CI で先取りする。
#[test]
fn parses_via_protocol_cache() {
    ProtocolCache::new(VP_DAEMON_KDL).unwrap_or_else(|e| {
        panic!("vp-daemon.kdl が enable_discovery(ProtocolCache::new) で parse できない: {e}")
    });
}

/// 2. KDL の channel 名は全て daemon が register する channel 集合の部分集合。
#[test]
fn channels_are_subset_of_daemon_registered() {
    for ch in registry().channels() {
        assert!(
            DAEMON_CHANNELS.contains(&ch.name.as_str()),
            "KDL の channel '{}' が daemon の register_channel 集合に無い（typo か、server.rs 側の追加漏れ／リネーム）",
            ch.name
        );
    }
}

/// 3. starter set（registry / events）+ 2026-07-12 拡張（world-control read-safe subset）が
///    KDL に存在する。
#[test]
fn starter_channels_present() {
    let reg = registry();
    for want in ["registry", "events", "world-control"] {
        assert!(
            reg.channel(want).is_some(),
            "channel '{want}' が vp-daemon.kdl に無い"
        );
    }
}

/// 4. registry / events の request 名は daemon handler の match method 集合の部分集合。
///    （wire の msg.method と文字列一致しない request は届かない = dead な synthesized tool）
#[test]
fn request_names_match_daemon_methods() {
    // SSOT: server.rs registry handler の match method（register/unregister/heartbeat/list/lanes/*）。
    const REGISTRY_METHODS: &[&str] = &[
        "register",
        "unregister",
        "heartbeat",
        "list",
        "lanes/add",
        "lanes/remove",
        "lanes/update",
    ];
    // SSOT: server.rs events handler の match method（emit/query）。
    const EVENTS_METHODS: &[&str] = &["emit", "query"];
    // SSOT: server.rs world-control dispatch の match method（projects/* + hub/discover + ping）。
    const WORLD_CONTROL_METHODS: &[&str] = &[
        "projects/list",
        "projects/add",
        "projects/remove",
        "projects/rename",
        "projects/reorder",
        "projects/start",
        "projects/stop",
        "hub/discover",
        "ping",
    ];

    let reg = registry();
    assert_requests_subset(&reg, "registry", REGISTRY_METHODS);
    assert_requests_subset(&reg, "events", EVENTS_METHODS);
    assert_requests_subset(&reg, "world-control", WORLD_CONTROL_METHODS);
}

fn assert_requests_subset(reg: &SchemaRegistry, channel: &str, methods: &[&str]) {
    let ch = reg
        .channel(channel)
        .unwrap_or_else(|| panic!("channel '{channel}' が KDL に無い"));
    for req in &ch.requests {
        assert!(
            methods.contains(&req.name.as_str()),
            "channel '{channel}' の request '{}' が daemon の match method 集合に無い（method 文字列一致していない = dead tool）",
            req.name
        );
    }
}
