//! integration test — wiremsg threaded inbox を実 schema (VpDb) で verify
//!
//! `WiremsgStore` の unit test は wiremsg_store の inline test schema を使うが、
//! 本 test は `VpDb::define_schema()` 経由 (= db/mod.rs SCHEMA_SQL) で wire_messages /
//! agent_cursor / thread_participant table が正しく define され、 threading 操作が
//! 通ることを確認する。
//!
//! 設計 memory: mem_1CbDLrECNZiNEZqjySLfSB
//! (R1 — cursor local-seq 化 / thread_id 全廃、 R3 — cross-process wire delivery)

use std::sync::Arc;

use vantage_point::capability::{WireMessage, WiremsgStore};
use vantage_point::db::VpDb;

/// kv-mem で VpDb 接続 + SCHEMA_SQL define + WiremsgStore build
///
/// R1: `WiremsgStore::new` は async (起動時に local_seq 採番を `math::max` で復元)。
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
    assert_eq!(unread[0].local_seq, 1, "最初の message の local_seq は 1");
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
    // R1: thread_id 全廃 — thread の連続は prev 一本で表す
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

/// SCHEMA_SQL 経由で wire_thread (ancestor_chain) が動く — 途中参加者の backlog 取得
///
/// R2 (mem_1CbDLrECNZiNEZqjySLfSB): thread に途中参加した agent が、 受け取った
/// message に至る系譜 (root-first) を read-only で引けることを実 schema で確認する。
#[tokio::test]
async fn schema_sql_wire_thread_ancestor_chain() {
    let store = make_store().await;
    let root = store
        .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
        .await
        .expect("send_root");
    let r1 = store
        .send_reply("bob@vp", &[], body("r1"), &root.id)
        .await
        .expect("r1");
    // alice が carol を新規に巻き込んで reply
    let r2 = store
        .send_reply(
            "alice@vp",
            &["bob@vp".to_string(), "carol@vp".to_string()],
            body("r2"),
            &r1.id,
        )
        .await
        .expect("r2");

    // carol は r2 のみ未読で受信する (参加前の root / r1 は受信しない)
    let carol = store.recv("carol@vp").await.expect("carol recv");
    assert_eq!(carol.len(), 1, "途中参加者は参加時点以降のみ受信");

    // carol が wire_thread (ancestor_chain) で r2 に至る backlog を取得
    let chain = store.ancestor_chain(&r2.id).await.expect("ancestor_chain");
    let ids: Vec<&str> = chain.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![root.id.as_str(), r1.id.as_str(), r2.id.as_str()],
        "系譜は root-first (chronological) で root→r1→r2"
    );

    // wire_thread は read-only — carol の cursor を前進させない (再 recv は依然空)
    let carol_again = store.recv("carol@vp").await.expect("carol recv 2");
    assert!(
        carol_again.is_empty(),
        "ancestor_chain は cursor を触らない (read-only)"
    );
}

/// SCHEMA_SQL 経由で R3 cross-process 受信 (`receive_forwarded`) が動く
///
/// R3 (mem_1CbDLrECNZiNEZqjySLfSB): 他 SP から forward された WireMessage を実 schema の
/// wire_messages accumulation に取り込み、 受信者が `wire_recv` で読めることを確認する。
/// origin の id は保持され、 local_seq は受信側で採番される。
#[tokio::test]
async fn schema_sql_receive_forwarded_cross_process() {
    let store = make_store().await;
    // 他 SP が作った相当の message — origin が採番した id / local_seq を持つ
    let origin = WireMessage {
        id: uuid::Uuid::now_v7().to_string(),
        prev: None,
        from: "agent@other-project".to_string(),
        to: vec!["agent@vp".to_string()],
        body: body("cross-process hello"),
        created_at: 1_700_000_000_000,
        local_seq: 777, // origin 側の値 — 受信側では振り直される
    };
    let (stored, inserted) = store
        .receive_forwarded(&origin)
        .await
        .expect("receive_forwarded");
    assert!(inserted, "新規 forward は INSERT される");
    assert_eq!(stored.id, origin.id, "origin の id を保持");
    assert_eq!(
        stored.local_seq, 1,
        "local_seq は受信側 accumulation で採番 (空 store なので 1)"
    );

    // 受信者 agent@vp は forward された message を未読で受け取れる
    let unread = store.recv("agent@vp").await.expect("recv");
    assert_eq!(unread.len(), 1, "forward された message が未読で届く");
    assert_eq!(unread[0].id, origin.id);
}

/// SCHEMA_SQL 経由で R3 受信が冪等 — 同一 id の二重 forward は二重 INSERT にならない
///
/// best-effort forward は retry を持たないが、 ネットワーク事情で二重着信しうる。
/// 同一 id の `receive_forwarded` は 2 回目以降スキップし、 accumulation を汚さない。
#[tokio::test]
async fn schema_sql_receive_forwarded_idempotent() {
    let store = make_store().await;
    let origin = WireMessage {
        id: uuid::Uuid::now_v7().to_string(),
        prev: None,
        from: "agent@other-project".to_string(),
        to: vec!["agent@vp".to_string()],
        body: body("dup forward"),
        created_at: 1_700_000_000_000,
        local_seq: 0,
    };
    // 二重 forward を模擬: 同一 message を 2 回 receive
    let (_, inserted1) = store.receive_forwarded(&origin).await.expect("recv 1");
    let (_, inserted2) = store.receive_forwarded(&origin).await.expect("recv 2");
    assert!(inserted1, "1 回目は INSERT");
    assert!(!inserted2, "2 回目は冪等スキップ");

    // accumulation 上は 1 件のまま (二重 INSERT になっていない)
    let unread = store.recv("agent@vp").await.expect("recv");
    assert_eq!(unread.len(), 1, "二重 forward でも message は 1 件");
}
