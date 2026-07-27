//! PoC: repos を db/machine 真実源化（VP-188 revert）の go/no-go 検証。
//!
//! epic 設計: creo `mem_1CbmWjCGNi9z49s3r21TwQ` / `.note/control-plane-consolidation-epic.md`
//!
//! 検証する仮説:
//! - T1: db/machine の repos table に insert→list できる
//! - T2: DB→repos.kdl の一方向 export が round-trip する（read back しない出力専用）
//! - T3 ★: DB dir が消失しても（VP-182 シナリオ）、export 済 kdl から import で復旧できる
//!   = VP-188 council が「embedded DB は ephemeral」として file に逃げた壁を、
//!   「Daemon 専用安定 DB + 一方向 export backstop」で正面突破できることの実証。

use vantage_point::db::VpDb;
use vantage_point::repos_file::{RepoEntry, ReposFile};

/// テスト専用の一時 DB dir（embedded surrealkv は実ディスクに書くため）。
/// tag でテスト間を分離（並列実行でも LOCK 衝突しない）。
fn temp_db_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vp-repos-poc-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn connect(dir: &std::path::Path) -> VpDb {
    let db = VpDb::connect_embedded(dir).await.expect("embedded connect");
    db.define_schema().await.expect("schema define");
    db
}

/// 2 件の登録 repo サンプル（enabled None / Some(false)、slot あり で field 網羅）。
fn sample() -> Vec<RepoEntry> {
    vec![
        RepoEntry {
            name: "vantage-point".into(),
            path: "/repos/vantage-point".into(),
            enabled: None,
            slot: Some(2),
        },
        RepoEntry {
            name: "creo-memories".into(),
            path: "/repos/creo-memories".into(),
            enabled: Some(false),
            slot: Some(3),
        },
    ]
}

/// T1: db/machine repos table に insert → list できる。
#[tokio::test]
async fn t1_insert_and_list() {
    let dir = temp_db_dir("t1");
    let db = connect(&dir).await;

    db.import_repos(&sample()).await.unwrap();
    let rows = db.list_repos().await.unwrap();

    assert_eq!(rows.len(), 2, "2 件 insert → 2 件 list");
    // ord 昇順で並ぶ（sidebar 並び順保持）
    assert_eq!(rows[0]["path"].as_str(), Some("/repos/vantage-point"));
    assert_eq!(rows[1]["path"].as_str(), Some("/repos/creo-memories"));

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// T2: DB → repos.kdl 一方向 export が round-trip する。
#[tokio::test]
async fn t2_export_roundtrip() {
    let dir = temp_db_dir("t2");
    let db = connect(&dir).await;

    db.import_repos(&sample()).await.unwrap();
    let entries = db.export_repos().await.unwrap();
    let kdl = ReposFile { repos: entries }.to_kdl().unwrap();

    assert!(kdl.contains("vantage-point"), "export に repo 名が出る");
    assert!(kdl.contains("creo-memories"));

    let back = ReposFile::from_kdl(&kdl).unwrap();
    assert_eq!(back.repos.len(), 2);
    assert_eq!(back.repos[0].path, "/repos/vantage-point", "順序保持");
    assert_eq!(back.repos[1].enabled, Some(false), "enabled=#false 保持");
    assert_eq!(back.repos[0].slot, Some(2), "slot 保持");

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// T3 ★: DB dir 消失（VP-182）→ 一方向 export からの import で復旧できる。
/// これが通れば VP-188 を revert して DB 真実源化する go 判定。
#[tokio::test]
async fn t3_db_loss_recovery_via_export() {
    let dir_a = temp_db_dir("t3a");

    // 1. DB に repos を入れ、export を kdl 文字列として退避（= repos.kdl 相当）
    let exported_kdl = {
        let db = connect(&dir_a).await;
        db.import_repos(&sample()).await.unwrap();
        let entries = db.export_repos().await.unwrap();
        let kdl = ReposFile { repos: entries }.to_kdl().unwrap();
        drop(db);
        kdl
    };

    // 2. DB dir 消失（VP-182 = DB dir 変更/消失で repos 全喪失のシナリオ）
    std::fs::remove_dir_all(&dir_a).unwrap();

    // 3. 新しい DB dir = 空。DB 単体では repos が失われていることを確認（VP-182 再現）
    let dir_b = temp_db_dir("t3b");
    let db2 = connect(&dir_b).await;
    assert_eq!(
        db2.list_repos().await.unwrap().len(),
        0,
        "DB 消失で repos は失われる（VP-182 が逃げた問題の再現）"
    );

    // 4. ★ 一方向 export（repos.kdl）から import → 復旧
    let recovered = ReposFile::from_kdl(&exported_kdl).unwrap();
    db2.import_repos(&recovered.repos).await.unwrap();
    let rows = db2.list_repos().await.unwrap();

    assert_eq!(
        rows.len(),
        2,
        "export/import backstop で repos 復旧（VP-188 の壁を正面突破）"
    );
    assert_eq!(rows[0]["name"].as_str(), Some("vantage-point"));
    assert_eq!(rows[1]["name"].as_str(), Some("creo-memories"));

    drop(db2);
    let _ = std::fs::remove_dir_all(&dir_b);
}

// =====================================================================
// PR-C: replace_all_repos (persist_repos の DB 全置換セマンティクス)
// =====================================================================

/// B1a: replace_all で古い entry が消え、 ord が新しい出現順に焼き直される。
#[tokio::test]
async fn b1a_replace_all_removes_stale_and_reorders() {
    let dir = temp_db_dir("b1a");
    let db = connect(&dir).await;

    // 3 件 import
    let three = vec![
        RepoEntry {
            name: "a".into(),
            path: "/r/a".into(),
            enabled: None,
            slot: Some(0),
        },
        RepoEntry {
            name: "b".into(),
            path: "/r/b".into(),
            enabled: None,
            slot: Some(1),
        },
        RepoEntry {
            name: "c".into(),
            path: "/r/c".into(),
            enabled: None,
            slot: Some(2),
        },
    ];
    db.import_repos(&three).await.unwrap();
    assert_eq!(db.list_repos().await.unwrap().len(), 3);

    // replace_all で 2 件に (c を落とし、 b→a の順に入れ替え)
    let two = vec![
        RepoEntry {
            name: "b".into(),
            path: "/r/b".into(),
            enabled: None,
            slot: Some(1),
        },
        RepoEntry {
            name: "a".into(),
            path: "/r/a".into(),
            enabled: None,
            slot: Some(0),
        },
    ];
    db.replace_all_repos(&two).await.unwrap();
    let rows = db.list_repos().await.unwrap();

    assert_eq!(rows.len(), 2, "全置換で 3 → 2 件");
    assert!(
        !rows.iter().any(|r| r["name"].as_str() == Some("c")),
        "stale entry (c) が消える"
    );
    // ord は新しい出現順 (b=0, a=1)
    assert_eq!(rows[0]["name"].as_str(), Some("b"), "新出現順で ord=0");
    assert_eq!(rows[1]["name"].as_str(), Some("a"), "新出現順で ord=1");

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// B1b: replace_all(&[]) で repos テーブルが空になる (= 全 repo 削除の DB 反映)。
#[tokio::test]
async fn b1b_replace_all_empty_clears() {
    let dir = temp_db_dir("b1b");
    let db = connect(&dir).await;

    db.import_repos(&sample()).await.unwrap();
    assert_eq!(db.list_repos().await.unwrap().len(), 2);

    db.replace_all_repos(&[]).await.unwrap();
    assert_eq!(db.list_repos().await.unwrap().len(), 0, "空 Vec で全消し");

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// B1c: path 重複を含む entries → UNIQUE index (idx_repos_path) で後勝ち 1 件に集約。
#[tokio::test]
async fn b1c_replace_all_dup_path_last_wins() {
    let dir = temp_db_dir("b1c");
    let db = connect(&dir).await;

    let dups = vec![
        RepoEntry {
            name: "first".into(),
            path: "/r/same".into(),
            enabled: None,
            slot: Some(0),
        },
        RepoEntry {
            name: "second".into(),
            path: "/r/same".into(),
            enabled: None,
            slot: Some(1),
        },
    ];
    db.replace_all_repos(&dups).await.unwrap();
    let rows = db.list_repos().await.unwrap();

    assert_eq!(rows.len(), 1, "path UNIQUE で 1 件に集約");
    assert_eq!(
        rows[0]["name"].as_str(),
        Some("second"),
        "後勝ち (upsert ON DUPLICATE KEY UPDATE)"
    );

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// B2: DB → export → kdl → from_kdl → replace_all → export の round-trip が一致する
/// (= persist_repos の「DB 全置換 + kdl ミラー」往復が情報を失わない)。
#[tokio::test]
async fn b2_replace_all_roundtrip_via_kdl() {
    let dir = temp_db_dir("b2");
    let db = connect(&dir).await;

    db.import_repos(&sample()).await.unwrap();
    let kdl = ReposFile {
        repos: db.export_repos().await.unwrap(),
    }
    .to_kdl()
    .unwrap();

    // kdl → from_kdl → replace_all (= persist 側の DB 真実源化経路)
    let back = ReposFile::from_kdl(&kdl).unwrap();
    db.replace_all_repos(&back.repos).await.unwrap();

    let again = db.export_repos().await.unwrap();
    assert_eq!(again.len(), 2, "round-trip で件数保持");
    assert_eq!(again[0].path, "/repos/vantage-point", "順序保持");
    assert_eq!(again[1].enabled, Some(false), "enabled=#false 保持");
    assert_eq!(again[0].slot, Some(2), "slot 保持");

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
