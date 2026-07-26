//! chronista-hub S4 relay の e2e（dialer + target inbound、ADR-020 §S4 = universal floor）。
//!
//! 通常 CI では `#[ignore]`（hub の起動が前提）。手動実行:
//!
//! ```bash
//! # 別ターミナルで hub を stub mode 起動:
//! #   cd chronista-hub && STUB_AUTH_ALLOWED=true AUTO_MIGRATE_ENABLED=true \
//! #     CHRONISTA_HUB_DB_PATH=./data/hub-vp-test.rocksdb \
//! #     CHRONISTA_HUB_UNISON_ADDR='[::1]:7879' CHRONISTA_HUB_PORT=3000 \
//! #     cargo run -p chronista-hub-server
//! CHRONISTA_HUB_ADDR='[::1]:7879' \
//!   cargo test -p vantage-point --test relay_e2e -- --ignored --nocapture
//! ```
//!
//! 検証内容: target daemon が `relay` server-channel を connect 前に登録して hub registry に
//! register（hub が wld_id→ctx を index）→ source daemon が `dial_relay(target_wld_id)` で hub 経由
//! の片方向 relay を確立 → data frame を 1 本送る → target の inbound handler が同じ payload を
//! 送信元 wld_id 付きで受け取る、という relay 往復（source→hub→target）を実証する。

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vantage_point::daemon::hub_client::{self, HubClient};

#[tokio::test]
#[ignore = "実 chronista-hub の起動が前提（CHRONISTA_HUB_ADDR で addr 指定）"]
async fn relay_source_to_target_roundtrip() {
    // 本番では vp バイナリが install する crypto provider を単体テストでも再現する（aws-lc-rs）。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let addr = hub_client::hub_addr()
        .expect("CHRONISTA_HUB_ADDR を設定して hub を起動した状態で実行すること");

    // 位置独立 routing key。test 固定値で独立性を担保（hub registry は wld_id keyed）。
    let target_wld = "wld_relay-target";
    let source_wld = "wld_relay-source";

    // ── target daemon: relay inbound handler を登録 → register（hub が wld_id→ctx を index）。
    let (tx, mut rx) = mpsc::unbounded_channel();
    let target = HubClient::connect_with_inbound(&addr, 5, move |inbound| {
        let tx = tx.clone();
        async move {
            // 受信した relay frame を test 側へ転送（assert 用）。
            let _ = tx.send(inbound);
        }
    })
    .await
    .expect("target: connect_with_inbound 失敗");
    target
        .register(target_wld, &[], "vp-relay-target", "VP relay target")
        .await
        .expect("target: register 失敗");

    // ── source daemon: 接続して target へ relay を張る。register（hub が target_wld→ctx を
    //    引けるよう、target の register が hub に反映されてから dial する＝この await 順序で担保）。
    let source = HubClient::connect(&addr, 5)
        .await
        .expect("source: connect 失敗");
    source
        .register(source_wld, &[], "vp-relay-source", "VP relay source")
        .await
        .expect("source: register 失敗");

    let dial = source
        .dial_relay(target_wld, source_wld)
        .await
        .expect("source: dial_relay 失敗（target が registry に居て established を返すこと）");
    assert_eq!(dial.target(), target_wld);

    // ── source → target に data frame を 1 本送る（hub が dumb forward）。
    let payload = json!({ "kind": "ping", "n": 42 });
    dial.send(&payload)
        .await
        .expect("source: relay data 送信失敗");

    // ── target の inbound handler が同じ payload を source の wld_id 付きで受け取る。
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("target が relay data を受信しなかった（タイムアウト）")
        .expect("inbound channel が閉じた");

    assert_eq!(got.from, source_wld, "送信元 wld_id が一致しない");
    assert_eq!(got.payload, payload, "forward された payload が一致しない");

    println!(
        "✅ relay e2e OK: {} → hub → {} （payload={}）",
        source_wld, target_wld, got.payload
    );
}

/// ②-A: 本番 daemon が使う常駐セッション [`hub_client::run_hub_federation`] が、register +
/// relay target inbound を「接続維持で」提供することを実証する。
///
/// 検証の核: source の `dial_relay` が `established` を返すのは、target が **hub の relay registry
/// (wld_id→ctx) に登録済み**のときだけ。これは常駐セッションが「接続 → register → 常駐」を
/// 成功させ、relay target として生きている証明になる（handler の中身を覗かずに assert できる）。
#[tokio::test]
#[ignore = "実 chronista-hub の起動が前提（CHRONISTA_HUB_ADDR で addr 指定）"]
async fn run_hub_federation_resident_relay_target() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let addr = hub_client::hub_addr()
        .expect("CHRONISTA_HUB_ADDR を設定して hub を起動した状態で実行すること");

    let target_wld = "wld_resident-target";
    let target_handle = "vp-resident-target";

    // 本番 daemon（process/server.rs）と同じ常駐セッションを起動。
    let shutdown = CancellationToken::new();
    let status = hub_client::HubFederationStatus::new();
    let daemons_cache = hub_client::HubNodesCache::new();
    // この test は register + relay target liveness を見るので、配送 handler は no-op で十分。
    let driver = tokio::spawn(hub_client::run_hub_federation(
        addr.clone(),
        target_wld.to_string(),
        vec![],
        target_handle.to_string(),
        "VP resident target".to_string(),
        status.clone(),
        daemons_cache.clone(),
        shutdown.clone(),
        |_inbound| async {},
    ));

    // source: 接続して register。
    let source_wld = "wld_resident-source";
    let source = HubClient::connect(&addr, 5)
        .await
        .expect("source: connect 失敗");
    source
        .register(source_wld, &[], "vp-resident-source", "VP resident source")
        .await
        .expect("source: register 失敗");

    // 常駐 target が registry に現れるまで poll（run_hub_federation の register は非同期）。
    let mut seen = false;
    for _ in 0..50 {
        let nodes = source.discover().await.expect("source: discover 失敗");
        if nodes.iter().any(|w| w.handle == target_handle) {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        seen,
        "常駐 target が registry に現れなかった（run_hub_federation の register 未完）"
    );

    // register 成功 → status は Connected（/api/health で vp-app に出る値）。
    assert_eq!(
        status.get(),
        hub_client::HubFederationState::Connected,
        "run_hub_federation の status が Connected に遷移していない"
    );

    // available nodes cache に**自 daemon は決して混入しない**（available_nodes の意味論 =
    // 「hub の向こうに誰がいるか」）。cache が空か populated かは discover tick の timing 次第
    // なので断定しない（初回 discover は接続直後、以降 45s 周期）。
    assert!(
        daemons_cache
            .get()
            .iter()
            .all(|w| w.handle != target_handle),
        "nodes cache に自 daemon (handle={target_handle}) が混入している"
    );

    // 常駐 target は relay registry にも居るはず → established を返せば relay target として live。
    let dial = source
        .dial_relay(target_wld, source_wld)
        .await
        .expect("常駐 target への dial_relay が established を返さなかった");
    assert_eq!(dial.target(), target_wld);
    dial.send(&json!({ "resident": true }))
        .await
        .expect("relay data 送信失敗");

    // 後片付け: 常駐ループを shutdown で停止（select の shutdown arm で即抜ける）。
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), driver).await;

    println!(
        "✅ resident relay target OK: run_hub_federation が {} を register + relay target inbound を常駐提供",
        target_handle
    );
}
