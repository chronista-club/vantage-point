//! chronista-hub 配線の e2e（実 hub に対して新 hub_client コードを叩く）。
//!
//! 通常 CI では `#[ignore]`（hub の起動が前提のため）。手動実行:
//!
//! ```bash
//! # 別ターミナルで hub を stub mode 起動:
//! #   cd chronista-hub && STUB_AUTH_ALLOWED=true AUTO_MIGRATE_ENABLED=true \
//! #     CHRONISTA_HUB_DB_PATH=./data/hub-vp-test.rocksdb \
//! #     CHRONISTA_HUB_UNISON_ADDR='[::1]:7879' CHRONISTA_HUB_PORT=3000 \
//! #     cargo run -p chronista-hub-server
//! CHRONISTA_HUB_ADDR='[::1]:7879' \
//!   cargo test -p vantage-point --test hub_client_e2e -- --ignored --nocapture
//! ```
//!
//! 検証内容: run_world が起動時に呼ぶのと同じ `HubClient::connect → register → discover`
//! 経路を実 hub に対して実行し、自身の handle が discover 結果に現れることを確認する。

use vantage_point::daemon::hub_client::{self, HubClient};

#[tokio::test]
#[ignore = "実 chronista-hub の起動が前提（CHRONISTA_HUB_ADDR で addr 指定）"]
async fn register_then_discover_roundtrip() {
    // 本番では vp バイナリ (vp-cli/src/main.rs) が起動時に install する provider を、
    // 単体テストプロセスでも再現する（VP は aws-lc-rs を使う）。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let addr = hub_client::hub_addr()
        .expect("CHRONISTA_HUB_ADDR を設定して hub を起動した状態で実行すること");

    // run_world と同じ handle 解決ロジック（ここでは固定 handle でテスト独立性を担保）。
    let handle = "vp-e2e-world";
    let name = "VP e2e World";
    // federation L2: 固定の wld_id を載せる（hub S2 未実装なら無視されるが register は非破壊）。
    let wld_id = "wld_e2e-test";

    let client = HubClient::connect(&addr, 5).await.expect("hub 接続に失敗");

    let entry = client
        .register(wld_id, handle, name)
        .await
        .expect("register 失敗");
    assert_eq!(entry.handle, handle, "register が返す handle が一致しない");
    assert!(
        !entry.registered_at.is_empty(),
        "registered_at が空（hub が timestamp を返していない）"
    );

    let worlds = client.discover().await.expect("discover 失敗");
    assert!(
        worlds.iter().any(|w| w.handle == handle),
        "discover 結果に自身の handle が見つからない: {worlds:?}"
    );

    println!(
        "✅ e2e OK: handle={handle} registered_at={} discover={} 件",
        entry.registered_at,
        worlds.len()
    );
}
