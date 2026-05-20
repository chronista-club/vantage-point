//! Phase A ① integration test — wiremsg threaded inbox を実 schema (VpDb) で verify
//!
//! `WiremsgStore` の unit test は msgbox_v2 / wiremsg_store の inline test schema を使うが、
//! 本 test は `VpDb::define_schema()` 経由 (= db/mod.rs SCHEMA_SQL) で wire_messages /
//! thread_participant table が正しく define され、 threading 操作が通ることを確認する。
//!
//! 設計 memory: mem_1CbD9H1KGQykBaFG8XXVsn

use std::sync::Arc;

use vantage_point::capability::WiremsgStore;
use vantage_point::db::VpDb;

/// kv-mem で VpDb 接続 + SCHEMA_SQL define + WiremsgStore build
async fn make_store() -> WiremsgStore {
    let db = VpDb::connect_mem().await.expect("kv-mem connect");
    db.define_schema().await.expect("schema define");
    WiremsgStore::new(Arc::new(db.inner().clone()))
}

fn body(text: &str) -> serde_json::Value {
    serde_json::json!({ "text": text })
}

/// SCHEMA_SQL 経由でも root 送信 → 受信者が未読として受け取れる
#[tokio::test]
async fn schema_sql_root_send_recv() {
    let store = make_store().await;
    let root = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("hi"))
        .await
        .expect("send_root");

    let unread = store.recv("bob@vp").await.expect("recv");
    assert_eq!(unread.len(), 1, "起点 message が未読で届く");
    assert_eq!(unread[0].id, root.id);
    assert!(unread[0].prev.is_none(), "root の prev は None");
    assert_eq!(unread[0].thread_id, root.id, "root の thread_id は自 id");
}

/// SCHEMA_SQL 経由で reply の thread 継続が動く
#[tokio::test]
async fn schema_sql_reply_threading() {
    let store = make_store().await;
    let root = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
        .await
        .expect("send_root");

    // bob が root を読む (cursor 前進)
    let _ = store.recv("bob@vp").await.expect("bob recv root");

    // alice が reply
    let reply = store
        .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
        .await
        .expect("send_reply");
    assert_eq!(reply.thread_id, root.thread_id, "reply は同 thread");
    assert_eq!(reply.prev.as_deref(), Some(root.id.as_str()), "prev = root");

    // bob は reply を未読として受け取る
    let unread = store.recv("bob@vp").await.expect("bob recv reply");
    assert_eq!(unread.len(), 1, "reply 1 件");
    assert_eq!(unread[0].id, reply.id);
}

/// SCHEMA_SQL 経由で「送信者は起点を読まない / 受信者は読む」 の cursor 仕様を確認
#[tokio::test]
async fn schema_sql_cursor_semantics() {
    let store = make_store().await;
    store
        .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
        .await
        .expect("send_root");

    // 送信者 alice は自分の root を読まない
    let alice = store.recv("alice@vp").await.expect("alice recv");
    assert!(alice.is_empty(), "送信者は自分の root message を読まない");

    // 受信者 bob は読む
    let bob = store.recv("bob@vp").await.expect("bob recv");
    assert_eq!(bob.len(), 1, "受信者は起点 message を読む");

    // 2 度目は空 (cursor 前進済)
    let bob2 = store.recv("bob@vp").await.expect("bob recv 2");
    assert!(bob2.is_empty(), "cursor 前進後は再配信なし");
}

/// SCHEMA_SQL 経由で thread 新規参加者が thread 全体を受け取る
#[tokio::test]
async fn schema_sql_new_participant_full_thread() {
    let store = make_store().await;
    let root = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
        .await
        .expect("send_root");
    let reply = store
        .send_reply(
            "alice@vp",
            &["bob@vp".to_string(), "carol@vp".to_string()],
            body("reply"),
            &root.id,
        )
        .await
        .expect("send_reply");

    // carol は新規参加 (read_cursor=None) なので root + reply の 2 件
    let carol = store.recv("carol@vp").await.expect("carol recv");
    assert_eq!(carol.len(), 2, "新規参加者は thread 全 message を受け取る");
    assert_eq!(carol[0].id, root.id);
    assert_eq!(carol[1].id, reply.id);
}
