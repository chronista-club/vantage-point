//! VP-173 Phase 3 PR-1 mini-spike — claim 機構 path A/B/C 比較
//!
//! VP-171 PR-1 の `test_claim_and_mark_consumed` が #[ignore]、 spike Q3 と同じ
//! `$candidates[0].id` 抽出 issue。 本 spike で 3 path を実機 verify し、 working path を確定。
//!
//! ## paths
//!
//! - **A**: transaction wrap (= 5 variants で subquery extraction を debug)
//! - **B**: DEFINE FUNCTION (= DB 内 atomic 関数)
//! - **C**: CAS pattern (= SELECT then UPDATE WHERE claim_id IS NONE)
//!
//! ## 期待
//!
//! 一つでも race-free 動作する path を確定して `WhitesnakeStore::claim` に採用する。

use std::sync::Arc;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

const SCHEMA: &str = r#"
DEFINE TABLE msgs SCHEMAFULL;
DEFINE FIELD id ON msgs TYPE string;
DEFINE FIELD ts ON msgs TYPE number;
DEFINE FIELD kind ON msgs TYPE string DEFAULT 'direct';
DEFINE FIELD payload ON msgs TYPE object FLEXIBLE;
DEFINE FIELD to_addr ON msgs TYPE string;
DEFINE FIELD to_actor ON msgs TYPE string;
DEFINE FIELD to_lane ON msgs TYPE string;
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

async fn make_db() -> Surreal<Any> {
    let db = surrealdb::engine::any::connect("mem://").await.unwrap();
    db.use_ns("vp").use_db("vp").await.unwrap();
    db.query(SCHEMA).await.unwrap().check().unwrap();
    db
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn seed_msgs(db: &Surreal<Any>, n: usize) {
    for i in 0..n {
        let now = now_ms() + i as u64;
        db.query(
            "CREATE type::record('msgs', $id) CONTENT {
                id: $id, ts: $ts, kind: 'direct',
                payload: { text: $text },
                to_addr: 'agent@vp/lead', to_actor: 'agent', to_lane: 'lead',
                from_addr: 'spike', from_actor: 'spike',
                expires_at: NONE, consumed_at: NONE, claim_id: NONE, claimed_at: NONE,
                status: 'active', status_at: $ts
            }",
        )
        .bind(("id", format!("msg-{}", i)))
        .bind(("ts", now))
        .bind(("text", format!("payload-{}", i)))
        .await
        .unwrap()
        .check()
        .unwrap();
    }
}

// =============================================================================
// Path A: transaction wrap — 5 variants で subquery extraction を debug
// =============================================================================

/// A1: 元 SDG 例 (= VP-171 で null だった)
#[tokio::test]
async fn path_a1_original_subquery() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    let now = now_ms();
    let mut res = db
        .query(
            "BEGIN;
             LET $candidates = SELECT * FROM msgs
                 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1;
             LET $target_id = $candidates[0].id;
             UPDATE $target_id SET claim_id='c1', claimed_at=$now;
             COMMIT;
             RETURN $candidates[0];",
        )
        .bind(("now", now))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = res.take(0).ok().flatten();
    println!("A1: row = {:?}", row);
    // 期待: row が Some(msg)。 null なら VP-171 と同じ failure。
}

/// A2: SELECT VALUE id で id-only array 取得 → UPDATE
#[tokio::test]
async fn path_a2_select_value_id() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    let now = now_ms();
    let mut res = db
        .query(
            "BEGIN;
             LET $target_ids = SELECT VALUE id FROM msgs
                 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1;
             UPDATE $target_ids[0] SET claim_id='c1', claimed_at=$now;
             COMMIT;
             RETURN $target_ids[0];",
        )
        .bind(("now", now))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = res.take(0).ok().flatten();
    println!("A2 SELECT VALUE id: row = {:?}", row);
}

/// A3: ONLY keyword で single record (wrap 外し)
#[tokio::test]
async fn path_a3_only_keyword() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    let now = now_ms();
    let res = db
        .query(
            "BEGIN;
             LET $cand = SELECT * FROM msgs
                 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1 FETCH;
             COMMIT;
             RETURN $cand;",
        )
        .bind(("now", now))
        .await;
    match res {
        Ok(mut r) => {
            let row: Option<serde_json::Value> = r.take(0).ok().flatten();
            println!("A3 (no ONLY、 FETCH): row = {:?}", row);
        }
        Err(e) => println!("A3 error: {}", e),
    }
}

/// A4: inline subquery in UPDATE (= 中間 LET 不要)
#[tokio::test]
async fn path_a4_inline_subquery() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    let now = now_ms();
    let res = db
        .query(
            "BEGIN;
             UPDATE (SELECT VALUE id FROM msgs
                 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1)[0]
                 SET claim_id='c1', claimed_at=$now;
             COMMIT;",
        )
        .bind(("now", now))
        .await;
    match res {
        Ok(mut r) => {
            let row: Option<serde_json::Value> = r.take(0).ok().flatten();
            println!("A4 inline subquery: row = {:?}", row);
        }
        Err(e) => println!("A4 error: {}", e),
    }
}

/// A5: $candidates の type 確認 (= debug 用)
#[tokio::test]
async fn path_a5_debug_candidates_shape() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    // No transaction、 単純な LET + RETURN で shape 確認
    let mut res = db
        .query(
            "LET $candidates = SELECT * FROM msgs
                 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1;
             RETURN $candidates;",
        )
        .await
        .unwrap();
    let row: Option<serde_json::Value> = res.take(0).ok().flatten();
    println!(
        "A5 $candidates raw: {}",
        serde_json::to_string_pretty(&row).unwrap_or_default()
    );
}

// =============================================================================
// Path B: DEFINE FUNCTION で DB 内 atomic 化
// =============================================================================

#[tokio::test]
async fn path_b_define_function() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    // 1. function 定義
    db.query(
        "DEFINE FUNCTION fn::claim_one($actor: string, $lane: string, $cid: string) {
             LET $found = SELECT * FROM msgs
                 WHERE status='active' AND to_actor=$actor AND to_lane=$lane
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1;
             IF array::len($found) > 0 {
                 UPDATE $found[0].id SET claim_id=$cid, claimed_at=time::now();
                 RETURN $found[0];
             };
             RETURN NONE;
         };",
    )
    .await
    .unwrap()
    .check()
    .unwrap();

    // 2. 呼び出し
    let mut res = db
        .query("RETURN fn::claim_one('agent', 'lead', 'c1');")
        .await
        .unwrap();
    let row: Option<serde_json::Value> = res.take(0).ok().flatten();
    println!("Path B: row = {:?}", row);
}

// =============================================================================
// Path C: CAS pattern (= SELECT then UPDATE WHERE claim_id IS NONE)
// =============================================================================

#[tokio::test]
async fn path_c_cas() {
    let db = make_db().await;
    seed_msgs(&db, 3).await;

    // Step 1: 候補 SELECT (= full row)
    // 注: SurrealDB v3 で `SELECT id` は idiom error、 `SELECT *` で full row 取得 → caller で id 抽出
    let mut res = db
        .query(
            "SELECT * FROM msgs
                 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1;",
        )
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = res.take(0).unwrap_or_default();
    println!(
        "Path C step 1: rows = {}",
        serde_json::to_string_pretty(&rows).unwrap_or_default()
    );

    if rows.is_empty() {
        println!("Path C: 候補なし");
        return;
    }

    // id field は "msgs:`msg-0`" 形式 string、 local id 部分を抽出
    let id_full = rows[0]["id"].as_str().unwrap_or_default(); // "msgs:`msg-0`"
    let local_id = id_full
        .strip_prefix("msgs:")
        .map(|s| s.trim_matches('`'))
        .unwrap_or(id_full);
    println!("Path C local_id = {}", local_id);

    // Step 2: UPDATE with CAS (= type::record で record id 構築)
    let now = now_ms();
    let mut res2 = db
        .query(
            "UPDATE type::record('msgs', $id) SET claim_id='c1', claimed_at=$now
                 WHERE claim_id IS NONE;",
        )
        .bind(("id", local_id.to_string()))
        .bind(("now", now))
        .await
        .unwrap();
    let updated: Vec<serde_json::Value> = res2.take(0).unwrap_or_default();
    println!(
        "Path C step 2 (CAS UPDATE): updated = {} rows",
        updated.len()
    );
    if let Some(row) = updated.first() {
        println!(
            "Path C row[0] = {}",
            serde_json::to_string_pretty(row).unwrap_or_default()
        );
    }
}

/// Path C の record id 形式を debug
#[tokio::test]
async fn path_c_id_format() {
    let db = make_db().await;
    seed_msgs(&db, 1).await;

    // SELECT * で full row、 id field を確認
    let mut res = db.query("SELECT * FROM msgs LIMIT 1;").await.unwrap();
    let rows: Vec<serde_json::Value> = res.take(0).unwrap();
    println!(
        "Path C id format: row = {}",
        serde_json::to_string_pretty(&rows).unwrap_or_default()
    );
}

// =============================================================================
// Race-free 検証 (= 100 並行 task で同 row を取らないか)
// =============================================================================
//
// 5 row insert + 100 並行 task で claim 試行、 unique consumer に 5 row が
// 配分されることを確認。
// Path B か C で実施 (= path A 動かなかったら skip)。

/// Path C (= CAS pattern) で 5 row × 100 consumer の race-free 検証
///
/// 結果: 5 unique row が unique consumer に配分 + 95 task は空振り想定。
#[tokio::test]
async fn race_path_c_cas() {
    let db = Arc::new(make_db().await);
    seed_msgs(&db, 5).await;

    let mut tasks = Vec::new();
    for i in 0..100 {
        let db = db.clone();
        let cid = format!("c-{}", i);
        tasks.push(tokio::spawn(async move {
            // Work pool retry loop: claim 失敗 (= 0 updated) なら次の候補へ
            // 5 row × 100 task → 5 success + 95 空振り想定、 retry 数で配分動作確認
            for _retry in 0..10 {
                let now = now_ms();
                let mut res = db
                    .query(
                        "SELECT * FROM msgs
                             WHERE status='active' AND to_actor='agent' AND to_lane='lead'
                               AND consumed_at IS NONE AND claim_id IS NONE
                             ORDER BY ts ASC LIMIT 1;",
                    )
                    .await
                    .ok()?;
                let rows: Vec<serde_json::Value> = res.take(0).ok()?;
                if rows.is_empty() {
                    return None; // 全 row claim 済
                }
                let id_full = rows[0]["id"].as_str()?;
                let local_id = id_full
                    .strip_prefix("msgs:")
                    .map(|s| s.trim_matches('`').to_string())?;

                let mut res2 = db
                    .query(
                        "UPDATE type::record('msgs', $id) SET claim_id=$cid, claimed_at=$now
                             WHERE claim_id IS NONE;",
                    )
                    .bind(("id", local_id.clone()))
                    .bind(("cid", cid.clone()))
                    .bind(("now", now))
                    .await
                    .ok()?;
                let updated: Vec<serde_json::Value> = res2.take(0).ok()?;
                if !updated.is_empty() {
                    return Some((cid, local_id));
                }
                // 0 updated = 他 consumer が先に取った → retry で次 row へ
            }
            None
        }));
    }

    let mut claimed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for t in tasks {
        if let Ok(Some((cid, local_id))) = t.await {
            let prev = claimed.insert(local_id.clone(), cid.clone());
            if let Some(prev_cid) = prev {
                panic!(
                    "❌ race: row '{}' claimed by both {} and {}",
                    local_id, prev_cid, cid
                );
            }
        }
    }
    println!(
        "✅ Path C race-free: {} unique rows claimed by unique consumers",
        claimed.len()
    );
    assert!(claimed.len() <= 5);
    assert!(!claimed.is_empty());
}

#[tokio::test]
async fn race_path_b_function() {
    let db = Arc::new(make_db().await);
    seed_msgs(&db, 5).await;

    // function 定義
    db.query(
        "DEFINE FUNCTION fn::claim_one($actor: string, $lane: string, $cid: string) {
             LET $found = SELECT * FROM msgs
                 WHERE status='active' AND to_actor=$actor AND to_lane=$lane
                   AND consumed_at IS NONE AND claim_id IS NONE
                 ORDER BY ts ASC LIMIT 1;
             IF array::len($found) > 0 {
                 UPDATE $found[0].id SET claim_id=$cid, claimed_at=time::now();
                 RETURN $found[0];
             };
             RETURN NONE;
         };",
    )
    .await
    .unwrap()
    .check()
    .unwrap();

    let mut tasks = Vec::new();
    for i in 0..100 {
        let db = db.clone();
        let cid = format!("c-{}", i);
        tasks.push(tokio::spawn(async move {
            let mut res = db
                .query("RETURN fn::claim_one('agent', 'lead', $cid);")
                .bind(("cid", cid.clone()))
                .await
                .ok()?;
            let row: Option<serde_json::Value> = res.take(0).ok().flatten();
            row.map(|r| (cid, r))
        }));
    }

    let mut claimed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for t in tasks {
        if let Ok(Some((cid, row))) = t.await
            && !row.is_null()
        {
            let id = row["id"]
                .as_str()
                .unwrap_or_default()
                .trim_matches('`')
                .to_string();
            let prev = claimed.insert(id.clone(), cid.clone());
            if let Some(prev_cid) = prev {
                panic!(
                    "❌ race: row '{}' claimed by both {} and {}",
                    id, prev_cid, cid
                );
            }
        }
    }
    println!(
        "✅ Path B race-free: {} unique rows claimed by unique consumers",
        claimed.len()
    );
    assert!(claimed.len() <= 5);
    assert!(!claimed.is_empty());
}
