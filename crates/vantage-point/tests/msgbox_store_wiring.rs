//! VP-174 Phase 3 PR-2 integration test — AppState 経由で WhitesnakeStore が動作することを verify
//!
//! 「配線のみ」 PR の動作確認:
//! - vpdb 接続から WhitesnakeStore を build
//! - msgs schema が SCHEMA_SQL 経由で自動 define
//! - AppState 同等の構成 (= store 参照 + 簡易操作) で insert / claim / stats が動く

use std::sync::Arc;
use vantage_point::capability::{Message, MessageKind, MsgboxStore, WhitesnakeStore};
use vantage_point::db::VpDb;

/// kv-mem で VpDb 接続 + schema define + WhitesnakeStore build
async fn make_test_store() -> WhitesnakeStore {
    let db = VpDb::connect_mem().await.expect("kv-mem connect");
    db.define_schema().await.expect("schema define");
    WhitesnakeStore::new(Arc::new(db.inner().clone()))
}

#[tokio::test]
async fn appstate_wiring_insert_and_stats() {
    let store = make_test_store().await;

    // 3 msg insert
    for i in 0..3 {
        let mut msg = Message::new("agent@vp/lead", "agent@vp/lead", MessageKind::Direct)
            .with_payload(&serde_json::json!({ "text": format!("wire-{}", i) }));
        msg.id = format!("wire-test-{}", i);
        store.insert(&msg).await.expect("insert");
    }

    // stats で active 3 を確認 (= AppState 経由 store 動作の foundation)
    let stats = store.stats().await.expect("stats");
    assert_eq!(stats.active, 3);
    assert_eq!(stats.dead_letter, 0);
    assert_eq!(stats.archived, 0);
}

#[tokio::test]
async fn appstate_wiring_claim_cycle() {
    let store = make_test_store().await;

    // 1 msg insert
    let mut msg = Message::new("agent@vp/lead", "agent@vp/lead", MessageKind::Direct)
        .with_payload(&serde_json::json!({ "text": "claim-cycle" }));
    msg.id = "wire-claim-1".to_string();
    store.insert(&msg).await.expect("insert");

    // claim → consume → 2 度目の claim は None (= consumed_at IS NOT NULL で除外)
    let claimed = store
        .claim("agent", "lead", "consumer-A")
        .await
        .expect("claim 1");
    assert!(claimed.is_some(), "1 件目 claim 成功");

    let claimed_msg = claimed.unwrap();
    store
        .mark_consumed(&claimed_msg.id)
        .await
        .expect("mark consumed");

    // 2 度目: 候補なし
    let claimed2 = store
        .claim("agent", "lead", "consumer-B")
        .await
        .expect("claim 2");
    assert!(claimed2.is_none(), "consumed 済 msg は claim されない");
}
