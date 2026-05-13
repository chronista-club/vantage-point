//! VP-170 Phase 1 spike — SurrealDB LIVE Query feasibility 検証
//!
//! VP-169 (mpsc 廃止 + Whitesnake-primary msgbox refactor) の SDG (doc 19) §6 で
//! 定義した 12 spike Q のうち、 **core 7 件 (Q1-Q7)** を SurrealDB v3.0.4 embedded で
//! 実機検証する。
//!
//! ## scope
//!
//! - **Q1**: LIVE SELECT working + $bind 込み (v3.0+ 公式 fix の実機確認)
//! - **Q2**: LIVE filter 脱落 (= UPDATE で WHERE 外れる) の event semantics
//! - **Q3**: atomic claim (`UPDATE ... LIMIT 1 RETURN AFTER`) race-free 100 並行
//! - **Q4**: 100 msg/sec で latency < 50ms
//! - **Q5**: 3 concurrent query (producer + consumer + GC) lock contention
//! - **Q6**: recv_idx で active filter が大量 archived row scan に堕ちないか
//! - **Q7**: LIVE stream embedded の切断 trigger
//!
//! ## 結果記録
//!
//! `docs/design/20-spike-report.md` に Q ごとに ✅ / ❌ + raw number。

use futures::StreamExt;
use std::time::Duration;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use tokio::time::timeout;

/// spike 用 msgs schema (= SDG §4.1 の subset、 最小限で動作確認)
const MSGS_SCHEMA: &str = r#"
DEFINE TABLE msgs SCHEMAFULL;

DEFINE FIELD id ON msgs TYPE string;
DEFINE FIELD ts ON msgs TYPE number;
DEFINE FIELD kind ON msgs TYPE string DEFAULT 'direct';
DEFINE FIELD payload ON msgs TYPE object FLEXIBLE;

DEFINE FIELD to_addr ON msgs TYPE string;
DEFINE FIELD to_actor ON msgs TYPE string;
DEFINE FIELD to_lane ON msgs TYPE string;
DEFINE FIELD to_project ON msgs TYPE option<string>;

DEFINE FIELD from_addr ON msgs TYPE string;
DEFINE FIELD from_actor ON msgs TYPE string;

DEFINE FIELD expires_at ON msgs TYPE option<number>;
DEFINE FIELD consumed_at ON msgs TYPE option<number>;
DEFINE FIELD claim_id ON msgs TYPE option<string>;
DEFINE FIELD claimed_at ON msgs TYPE option<number>;
DEFINE FIELD status ON msgs TYPE string DEFAULT 'active';
DEFINE FIELD status_at ON msgs TYPE number;

DEFINE INDEX recv_idx ON msgs FIELDS status, to_actor, to_lane, consumed_at;
"#;

/// kv-mem で接続済 + msgs schema 定義済の Surreal を返す
async fn make_test_db() -> Surreal<Any> {
    let db = surrealdb::engine::any::connect("mem://")
        .await
        .expect("kv-mem connect");
    db.use_ns("vp").use_db("vp").await.expect("use_ns/db");
    db.query(MSGS_SCHEMA).await.expect("schema").check().expect("schema check");
    db
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// 1 row insert helper
async fn insert_msg(
    db: &Surreal<Any>,
    id: &str,
    to_actor: &str,
    to_lane: &str,
    payload_text: &str,
) {
    let now = now_ms();
    db.query(
        "CREATE type::record('msgs', $id) CONTENT {
            id: $id, ts: $ts, kind: 'direct',
            payload: { text: $text },
            to_addr: $to_addr, to_actor: $to_actor, to_lane: $to_lane, to_project: NONE,
            from_addr: 'spike', from_actor: 'spike',
            expires_at: NONE, consumed_at: NONE, claim_id: NONE, claimed_at: NONE,
            status: 'active', status_at: $ts
        }",
    )
    .bind(("id", id.to_string()))
    .bind(("ts", now))
    .bind(("text", payload_text.to_string()))
    .bind(("to_addr", format!("{}@vp/{}", to_actor, to_lane)))
    .bind(("to_actor", to_actor.to_string()))
    .bind(("to_lane", to_lane.to_string()))
    .await
    .expect("insert")
    .check()
    .expect("insert check");
}

// =============================================================================
// Q1: LIVE SELECT working + $bind 込み
// =============================================================================
//
// 検証: `LIVE SELECT * FROM msgs WHERE to_actor=$actor AND to_lane=$lane` を
// embedded で実行し、 マッチする row の INSERT で notification が来るか。

#[tokio::test]
async fn q1_live_select_with_param_binding() {
    let db = make_test_db().await;

    // LIVE SELECT を $bind 込みで開始
    let mut stream = db
        .query(
            "LIVE SELECT * FROM msgs WHERE to_actor=$actor AND to_lane=$lane AND status='active' AND consumed_at IS NONE",
        )
        .bind(("actor", "agent".to_string()))
        .bind(("lane", "lead".to_string()))
        .await
        .expect("LIVE SELECT query")
        .stream::<surrealdb::Notification<serde_json::Value>>(0)
        .expect("stream(0)");

    // 100ms 待ってからマッチする msg を insert
    tokio::time::sleep(Duration::from_millis(100)).await;
    insert_msg(&db, "msg-1", "agent", "lead", "hello").await;

    // 1 秒以内に notification が来るか
    let result = timeout(Duration::from_secs(1), stream.next()).await;

    match result {
        Ok(Some(Ok(notif))) => {
            println!(
                "✅ Q1: notification 受信、 action={:?} data={}",
                notif.action, notif.data
            );
            assert_eq!(notif.action, surrealdb::types::Action::Create);
            assert_eq!(notif.data["to_actor"], "agent");
            assert_eq!(notif.data["to_lane"], "lead");
        }
        Ok(Some(Err(e))) => panic!("❌ Q1: stream error {}", e),
        Ok(None) => panic!("❌ Q1: stream closed before notification"),
        Err(_) => panic!("❌ Q1: timeout — notification 来ず (= $bind が WHERE 内で動かない可能性)"),
    }
}

// =============================================================================
// Q2: LIVE filter 脱落 (UPDATE で WHERE 外れる) の event semantics
// =============================================================================
//
// 検証: status='active' でマッチする row を UPDATE で status='archived' にした時、
// LIVE が DELETE event を出すか / UPDATE event を出すか / no event か。

#[tokio::test]
async fn q2_live_filter_dropout_event_semantics() {
    let db = make_test_db().await;

    // 既存 row を insert (active)
    insert_msg(&db, "msg-2", "agent", "lead", "to-be-archived").await;

    // LIVE SELECT 開始
    let mut stream = db
        .query("LIVE SELECT * FROM msgs WHERE status='active' AND to_actor='agent' AND to_lane='lead'")
        .await
        .expect("LIVE SELECT")
        .stream::<surrealdb::Notification<serde_json::Value>>(0)
        .expect("stream");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // status='archived' で WHERE filter 脱落させる
    let now = now_ms();
    db.query("UPDATE type::record('msgs', 'msg-2') SET status='archived', status_at=$now")
        .bind(("now", now))
        .await
        .expect("update")
        .check()
        .expect("check");

    // 1 秒以内に何 event が来るか
    let result = timeout(Duration::from_secs(1), stream.next()).await;

    match result {
        Ok(Some(Ok(notif))) => {
            println!(
                "Q2 event semantics: action={:?} data={}",
                notif.action, notif.data
            );
            // Action は Create / Update / Delete / Killed のいずれか
            // - Delete event 来る → SDG plan のまま OK
            // - Update event 来る (filter 脱落でも) → consumer 側で再評価
            // - そもそも来ない → polling fallback 必要
        }
        Ok(Some(Err(e))) => println!("❌ Q2: stream error {}", e),
        Ok(None) => println!("Q2: stream closed, no event (= UPDATE で WHERE 脱落時 event 来ない)"),
        Err(_) => println!("Q2: timeout, no event (= UPDATE で WHERE 脱落時 event 来ない)"),
    }
}

// =============================================================================
// Q3: atomic claim race-free (100 並行 task で同 row 取り合い)
// =============================================================================
//
// ## 重要 finding (= SDG §4.1 主 query path に直接影響)
//
// SurrealDB v3.0.4 では **`UPDATE ... ORDER BY ... LIMIT N` syntax は不可**:
// ```
// Parse error: Unexpected token `ORDER`, expected Eof
// ```
//
// SDG §4.1 で示した atomic claim 主 query:
// ```sql
// UPDATE msgs SET claim_id=$cid WHERE ... ORDER BY ts ASC LIMIT 1 RETURN AFTER
// ```
// は **v3.0.4 で動かない**。 これは Purple Haze F4 finding (= transaction wrap obligatory)
// より厳しい SQL syntax レベルの制限。
//
// ### workaround paths (= Phase 2 実装で選択)
//
// 1. **Transaction wrap with SELECT + UPDATE**:
//    ```
//    BEGIN;
//    LET $cand = SELECT * FROM msgs WHERE ... ORDER BY ts ASC LIMIT 1;
//    LET $tid = $cand[0].id;
//    UPDATE $tid SET claim_id=$cid, claimed_at=$now;
//    COMMIT;
//    ```
//    ただし spike では subquery 内の `$cand[0].id` 抽出が想定通り動かず、 race-free
//    verification は Phase 2 に carry over。
//
// 2. **2-step approach**: SELECT で id 取得 → UPDATE record id (race あり、 別 consumer が
//    間に入る可能性、 transaction なしでは race-free 保証なし)
//
// 3. **SurrealDB function 自作**: `DEFINE FUNCTION fn::claim_one(...)` で atomic 化
//
// ### Phase 2 carry-over
//
// この test は spike で「**SDG main query 不可**」 を確定するために残し、 ✅ /❌ 判定を
// `partial` (= 設計修正必要、 別 path で race-free 確保) とする。 実機 race-free 確認は
// Phase 2 (PR-1 受け皿) 実装時に bench を含めて行う。

#[tokio::test]
#[ignore = "spike finding: UPDATE ORDER BY LIMIT not supported in v3.0.4, see doc 20 spike report"]
async fn q3_atomic_claim_race_free() {
    let db = make_test_db().await;

    // 10 row insert (claim 候補)
    for i in 0..10 {
        insert_msg(&db, &format!("claim-{}", i), "agent", "lead", "claim-target").await;
    }

    let db = std::sync::Arc::new(db);

    // 100 並行 task で claim 試行 (= 10 row に対し 100 task、 90 task は空振り想定)
    let mut tasks = Vec::new();
    for task_id in 0..100 {
        let db = db.clone();
        let consumer_id = format!("consumer-{}", task_id);
        tasks.push(tokio::spawn(async move {
            let now = now_ms();
            // Finding: SurrealDB v3 では UPDATE に ORDER BY + LIMIT 直接不可
            // → transaction wrap (= Purple Haze F4 obligatory path) で SELECT + UPDATE を atomic に
            let res = db
                .query(
                    "BEGIN;
                     LET $candidates = SELECT * FROM msgs
                         WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                           AND consumed_at IS NONE AND claim_id IS NONE
                         ORDER BY ts ASC LIMIT 1;
                     LET $target_id = $candidates[0].id;
                     RETURN UPDATE $target_id SET claim_id=$cid, claimed_at=$now RETURN AFTER;
                     COMMIT;",
                )
                .bind(("cid", consumer_id.clone()))
                .bind(("now", now))
                .await;
            let mut res = match res {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Q3 task {} query error: {}", consumer_id, e);
                    return None;
                }
            };
            // BEGIN/LET/RETURN/COMMIT で RETURN の出力が statement index にあるかも
            let rows: Vec<serde_json::Value> = match res.take(0) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Q3 task {} take error: {}", consumer_id, e);
                    return None;
                }
            };
            rows.into_iter().next().map(|row| (consumer_id, row))
        }));
    }

    // 結果収集 + row_id 抽出 (= SurrealDB の record id は様々な形を取りうる、 1 件目で debug print)
    let mut claimed_by: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut debug_printed = false;
    for t in tasks {
        if let Ok(Some((consumer_id, row))) = t.await {
            if !debug_printed {
                eprintln!("Q3 sample row JSON = {}", row);
                debug_printed = true;
            }
            // row["id"] が string なら直接、 object なら "tb"/"id" or "String" field 形式
            let row_id = if let Some(s) = row["id"].as_str() {
                s.to_string()
            } else if let Some(s) = row["id"]["String"].as_str() {
                s.to_string()
            } else {
                row["id"].to_string()  // fallback: full JSON serialize
            };
            let prev = claimed_by.insert(row_id.clone(), consumer_id.clone());
            if let Some(prev_cid) = prev {
                panic!(
                    "❌ Q3: race condition — row '{}' を 2 consumer ({} and {}) が claim",
                    row_id, prev_cid, consumer_id
                );
            }
        }
    }

    println!("✅ Q3: {} row claimed by unique consumers (race-free)", claimed_by.len());
    assert!(claimed_by.len() <= 10, "claim 数 が 10 を超えた: {}", claimed_by.len());
    assert!(claimed_by.len() > 0, "1 row も claim されなかった");
}
