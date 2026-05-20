//! integration test — wiremsg threaded inbox を実 schema (VpDb) で verify
//!
//! `WiremsgStore` の unit test は wiremsg_store の inline test schema を使うが、
//! 本 test は `VpDb::define_schema()` 経由 (= db/mod.rs SCHEMA_SQL) で wire_messages /
//! agent_cursor / thread_participant table が正しく define され、 threading 操作が
//! 通ることを確認する。
//!
//! 設計 memory: mem_1CbDLrECNZiNEZqjySLfSB (決定 III — per-agent 単一 cursor)

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

/// SCHEMA_SQL 経由で thread 途中参加者は参加時点以降の message のみ受け取る
///
/// 決定 III: to ベース配送のため、 reply の to に入った carol は reply のみ受信し、
/// 参加前の root は受信しない (参加前 backlog は wire_thread query の責務)。
#[tokio::test]
async fn schema_sql_new_participant_sees_messages_from_join() {
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

    // carol は reply の to に入った時点以降 = reply の 1 件のみ
    let carol = store.recv("carol@vp").await.expect("carol recv");
    assert_eq!(
        carol.len(),
        1,
        "途中参加者は参加時点以降の message のみ受信"
    );
    assert_eq!(carol[0].id, reply.id, "受信するのは reply");
    assert!(carol[0].id != root.id, "参加前の root は受信しない");
}

/// SCHEMA_SQL 経由で reply-all 展開 (REQ-THREAD-005) が動く
#[tokio::test]
async fn schema_sql_reply_all_expansion() {
    let store = make_store().await;
    let root = store
        .send_root(
            "alice@vp",
            &["bob@vp".to_string(), "carol@vp".to_string()],
            body("root"),
        )
        .await
        .expect("send_root");

    // bob が to を空指定で reply — reply-all 展開で alice/carol に届く
    let reply = store
        .send_reply("bob@vp", &[], body("reply"), &root.id)
        .await
        .expect("send_reply");
    assert!(reply.to.contains(&"alice@vp".to_string()), "alice に展開");
    assert!(reply.to.contains(&"carol@vp".to_string()), "carol に展開");
    assert!(!reply.to.contains(&"bob@vp".to_string()), "from は除外");

    let carol = store.recv("carol@vp").await.expect("carol recv");
    assert!(
        carol.iter().any(|m| m.id == reply.id),
        "carol が reply-all で reply を受信"
    );
}
