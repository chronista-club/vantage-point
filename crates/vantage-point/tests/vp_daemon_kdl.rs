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
//!   5. daemon-control の全 method は「KDL に記述」か「意図的な omission」のどちらかに属する
//!      （doc 45 段 1 で追加。4 の逆方向 = 記述漏れの検出）
//!
//! SSOT: crates/vantage-point/src/daemon/server.rs の register_channel(...) と各 match method。
//! channel 名と daemon-control の method は source から抽出して突き合わせる（自動）。
//! registry / events の method だけは小さく安定なので手動 const（コメント参照）。

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
    "daemon-repo",
    "lanes",
    // doc 52 §6: canvas 語彙退去 — "canvas-ingest" → "gui-ingest" / "canvas" → "gui"
    "gui-ingest",
    "gui",
    "repo-proxy",
    "daemon-device",
    "device",
    "registry",
    "events",
    "daemon-control",
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

/// 3. starter set（registry / events）+ 2026-07-12 拡張（daemon-control read-safe subset）が
///    KDL に存在する。
#[test]
fn starter_channels_present() {
    let reg = registry();
    for want in ["registry", "events", "daemon-control"] {
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
    // SSOT: server.rs registry handler の match method。
    // doc 44 P1 (fold-in): repo 向けの register / unregister / heartbeat / lanes/* は撤去し、
    // read-only の `list` だけ残した（自己登録しに来る repo が存在しない）。この const を
    // 実態に合わせないと、消えた method を KDL に書き足しても本テストが素通りして
    // dead tool が出荷される（= 撤去で const だけ取り残される drift、`"control"` と同じ轍）。
    const REGISTRY_METHODS: &[&str] = &["list"];
    // SSOT: server.rs events handler の match method（emit/query）。
    const EVENTS_METHODS: &[&str] = &["emit", "query"];
    // daemon-control は最も method が増減する handler なので、手動 const ではなく
    // source（`handle_daemon_control` の match arm）から抽出して突き合わせる。
    // これで「method を消したのに const が取り残される」drift（registry で実際に起きた）が
    // daemon-control では構造的に起きない。
    let daemon_control_methods = extract_daemon_control_methods();

    let reg = registry();
    assert_requests_subset(&reg, "registry", REGISTRY_METHODS);
    assert_requests_subset(&reg, "events", EVENTS_METHODS);
    assert_requests_subset(&reg, "daemon-control", &daemon_control_methods);
}

/// KDL に**意図的に記述しない** daemon-control method。
///
/// 方針（vp-daemon.kdl の channel コメントと対）: daemon-control に描くのは read-safe な
/// request だけ。mutation は手で叩くと repos.kdl / lane descriptor の状態を壊すので
/// unison-mcp の合成 tool に露出させない。`ping` は liveness probe（意味のある観測面ではない）。
///
/// この const の存在意義は「omission を明示的な決定にする」こと。テスト 4 は
/// KDL → source の片方向しか見ないため、handler に method を足しても KDL に書き忘れれば
/// 素通りする（= 露出させるか否かを誰も判断しないまま出荷される）。下の
/// [`daemon_control_methods_are_described_or_explicitly_omitted`] が逆方向を塞ぐ。
const DAEMON_CONTROL_OMITTED_BY_DESIGN: &[&str] = &[
    // repos mutation（repos.kdl / db の状態を壊しうる）
    "repos/list",
    "repos/add",
    "repos/remove",
    "repos/rename",
    "repos/set_enabled",
    "repos/reorder",
    "repos/start",
    "repos/stop",
    "repos/update",
    "repos/sync",
    "repos/reload",
    "repos/restart",
    "repos/pointview",
    // lane mutation（descriptor は daemon-canonical truth なので手動注入させない）
    "lanes/create",
    "lanes/set_active",
    // 帳簿 mutation（doc 44 §7.5）— 見送りの記録は「実際に何が起きたか」の観測なので、
    // 手で叩けると**起きていない見送り**を履歴に書ける。読み側 host/farewell_log は
    // read-safe なので KDL に記述済み。
    "host/farewell_observe",
    "host/farewell_reclaimed",
    // liveness probe（surface の共有 connection 用、観測面ではない）
    "ping",
];

/// 5. daemon-control の全 method は「KDL に記述」か「[`DAEMON_CONTROL_OMITTED_BY_DESIGN`]」の
///    どちらかに属する。
///
/// テスト 4（KDL ⊆ source）は dead tool を防ぐが、逆に **source に増えた method を KDL に
/// 書き忘れる**方は検出しない。それ自体は壊れないものの、「agent に露出させるか」を
/// 誰も判断しないまま面が増える状態になる（doc 45 §1 の利得は「Unison に乗せた面が
/// そのまま agent の面になる」ことなので、無意識の非露出は利得の取りこぼし）。
/// 新 method を足したら KDL に書くか、omission list に理由付きで足すかを選ぶこと。
#[test]
fn daemon_control_methods_are_described_or_explicitly_omitted() {
    let reg = registry();
    let described: std::collections::BTreeSet<&str> = reg
        .channel("daemon-control")
        .expect("daemon-control が KDL に無い")
        .requests
        .iter()
        .map(|r| r.name.as_str())
        .collect();

    for method in extract_daemon_control_methods() {
        assert!(
            described.contains(method.as_str())
                || DAEMON_CONTROL_OMITTED_BY_DESIGN.contains(&method.as_str()),
            "daemon-control.{method} が KDL にも omission list にも無い\
             （露出させるなら schema/vp-daemon.kdl に request を足す、\
             露出させないなら DAEMON_CONTROL_OMITTED_BY_DESIGN に理由付きで足す）"
        );
    }
}

/// omission list が実装から取り残されていないこと（撤去した method が list に残らない）。
///
/// `DAEMON_CHANNELS` で実際に起きた「実装から消えたのに const だけ残る」drift の同型。
#[test]
fn omitted_methods_still_exist_in_source() {
    let found: std::collections::BTreeSet<String> =
        extract_daemon_control_methods().into_iter().collect();
    for omitted in DAEMON_CONTROL_OMITTED_BY_DESIGN {
        assert!(
            found.contains(*omitted),
            "DAEMON_CONTROL_OMITTED_BY_DESIGN の '{omitted}' が handle_daemon_control に無い\
             （method を撤去したら本 const からも消すこと）"
        );
    }
}

/// `handle_daemon_control` の `"method" =>` arm を source から抽出する。
///
/// 独立関数なので「関数開始 → 列 0 の `}` で終端」で body を切り出せる。method arm は
/// `"xxx" => {` の形で一意（内部の match arm は `Ok(_)` / `Err(_)` で文字列リテラルではない）。
fn extract_daemon_control_methods() -> Vec<String> {
    let start = DAEMON_SERVER_RS
        .find("async fn handle_daemon_control")
        .expect("handle_daemon_control が server.rs に無い");
    let body = &DAEMON_SERVER_RS[start..];
    // 関数終端 = 次に現れる列 0 の閉じ括弧。
    let end = body.find("\n}\n").map(|e| e + 2).unwrap_or(body.len());
    body[..end]
        .lines()
        .filter_map(|line| {
            let t = line.trim_start();
            let rest = t.strip_prefix('"')?;
            let (name, after) = rest.split_once('"')?;
            after
                .trim_start()
                .starts_with("=>")
                .then(|| name.to_string())
        })
        .collect()
}

fn assert_requests_subset<S: AsRef<str>>(reg: &SchemaRegistry, channel: &str, methods: &[S]) {
    let ch = reg
        .channel(channel)
        .unwrap_or_else(|| panic!("channel '{channel}' が KDL に無い"));
    for req in &ch.requests {
        assert!(
            methods.iter().any(|m| m.as_ref() == req.name),
            "channel '{channel}' の request '{}' が daemon の match method 集合に無い（method 文字列一致していない = dead tool）",
            req.name
        );
    }
}
