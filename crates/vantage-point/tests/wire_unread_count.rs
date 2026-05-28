//! integration test — `wire_unread_count` QUIC/HTTP handler の read-only 性を verify
//!
//! flow_progress の集約 view で使う `wire_unread_count` 経路が:
//! 1. 未読 count を正しく返す
//! 2. cursor を進めない (= 再 call しても count が同じ、 `wire_recv` で同 message を再取得できる)
//!
//! を保証する。

use std::sync::Arc;

use vantage_point::capability::WiremsgStore;
use vantage_point::db::VpDb;

async fn make_store() -> WiremsgStore {
    let db = VpDb::connect_mem().await.expect("kv-mem connect");
    db.define_schema().await.expect("schema define");
    WiremsgStore::new(Arc::new(db.inner().clone()))
        .await
        .expect("WiremsgStore::new")
}

fn body(text: &str) -> serde_json::Value {
    serde_json::json!({ "text": text })
}

/// unread_count_by_thread は cursor を進めない (= 再 call で同 count、 wire_recv で同 message)
#[tokio::test]
async fn unread_count_is_read_only() {
    let store = make_store().await;

    // alice → bob に 3 thread (= 3 root) 送信
    let r1 = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("a"))
        .await
        .unwrap();
    let r2 = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("b"))
        .await
        .unwrap();
    let r3 = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("c"))
        .await
        .unwrap();

    // bob の未読 count を 2 回連続で取得 — どちらも同じ値 (cursor 不触り)
    let counts1 = store.unread_count_by_thread("bob@vp").await.unwrap();
    let counts2 = store.unread_count_by_thread("bob@vp").await.unwrap();
    let total1: u64 = counts1.values().sum();
    let total2: u64 = counts2.values().sum();
    assert_eq!(total1, 3, "3 つの thread root が未読として count される");
    assert_eq!(total2, 3, "再 call も同 count (cursor 不触り)");
    assert!(counts1.contains_key(&r1.id));
    assert!(counts1.contains_key(&r2.id));
    assert!(counts1.contains_key(&r3.id));

    // wire_recv で全 message を取得できる (= unread_count_by_thread が cursor を進めていない証拠)
    let unread = store.recv("bob@vp").await.unwrap();
    assert_eq!(unread.len(), 3, "wire_recv で全 3 message 取得できる");

    // wire_recv で cursor を進めた後は unread_count は 0
    let counts3 = store.unread_count_by_thread("bob@vp").await.unwrap();
    let total3: u64 = counts3.values().sum();
    assert_eq!(total3, 0, "wire_recv 後は未読 0");
}

/// 送信者は自分の thread を未読として count されない
#[tokio::test]
async fn unread_count_excludes_sender() {
    let store = make_store().await;
    store
        .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
        .await
        .unwrap();

    let alice_counts = store.unread_count_by_thread("alice@vp").await.unwrap();
    let alice_total: u64 = alice_counts.values().sum();
    assert_eq!(alice_total, 0, "送信者の未読 count は 0");

    let bob_counts = store.unread_count_by_thread("bob@vp").await.unwrap();
    let bob_total: u64 = bob_counts.values().sum();
    assert_eq!(bob_total, 1, "受信者は 1 未読");
}

/// reply は parent thread の同一 root key 配下に集約される
#[tokio::test]
async fn unread_count_groups_by_thread_root() {
    let store = make_store().await;

    let root = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
        .await
        .unwrap();

    // bob が root を読む (cursor 前進)
    let _ = store.recv("bob@vp").await.unwrap();

    // alice → bob に reply を 3 つ
    let _r1 = store
        .send_reply("alice@vp", &["bob@vp".to_string()], body("a1"), &root.id)
        .await
        .unwrap();
    let _r2 = store
        .send_reply("alice@vp", &["bob@vp".to_string()], body("a2"), &root.id)
        .await
        .unwrap();
    let _r3 = store
        .send_reply("alice@vp", &["bob@vp".to_string()], body("a3"), &root.id)
        .await
        .unwrap();

    let counts = store.unread_count_by_thread("bob@vp").await.unwrap();
    // 3 reply は同一 thread の同 root id にまとまる
    assert_eq!(counts.len(), 1, "1 thread だけ");
    assert_eq!(*counts.get(&root.id).unwrap(), 3, "3 reply が root に集約");
}
