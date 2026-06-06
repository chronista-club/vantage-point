//! PoC: projects を db/world 真実源化（VP-188 revert）の go/no-go 検証。
//!
//! epic 設計: creo `mem_1CbmWjCGNi9z49s3r21TwQ` / `.note/control-plane-consolidation-epic.md`
//!
//! 検証する仮説:
//! - T1: db/world の projects table に insert→list できる
//! - T2: DB→projects.kdl の一方向 export が round-trip する（read back しない出力専用）
//! - T3 ★: DB dir が消失しても（VP-182 シナリオ）、export 済 kdl から import で復旧できる
//!   = VP-188 council が「embedded DB は ephemeral」として file に逃げた壁を、
//!     「World 専用安定 DB + 一方向 export backstop」で正面突破できることの実証。

use vantage_point::db::VpDb;
use vantage_point::projects_file::{ProjectEntry, ProjectsFile};

/// テスト専用の一時 DB dir（embedded surrealkv は実ディスクに書くため）。
/// tag でテスト間を分離（並列実行でも LOCK 衝突しない）。
fn temp_db_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vp-projects-poc-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn connect(dir: &std::path::Path) -> VpDb {
    let db = VpDb::connect_embedded(dir).await.expect("embedded connect");
    db.define_schema().await.expect("schema define");
    db
}

/// 2 件の登録 project サンプル（enabled None / Some(false)、slot あり で field 網羅）。
fn sample() -> Vec<ProjectEntry> {
    vec![
        ProjectEntry {
            name: "vantage-point".into(),
            path: "/repos/vantage-point".into(),
            enabled: None,
            slot: Some(2),
        },
        ProjectEntry {
            name: "creo-memories".into(),
            path: "/repos/creo-memories".into(),
            enabled: Some(false),
            slot: Some(3),
        },
    ]
}

/// T1: db/world projects table に insert → list できる。
#[tokio::test]
async fn t1_insert_and_list() {
    let dir = temp_db_dir("t1");
    let db = connect(&dir).await;

    db.import_projects(&sample()).await.unwrap();
    let rows = db.list_projects().await.unwrap();

    assert_eq!(rows.len(), 2, "2 件 insert → 2 件 list");
    // ord 昇順で並ぶ（sidebar 並び順保持）
    assert_eq!(rows[0]["path"].as_str(), Some("/repos/vantage-point"));
    assert_eq!(rows[1]["path"].as_str(), Some("/repos/creo-memories"));

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// T2: DB → projects.kdl 一方向 export が round-trip する。
#[tokio::test]
async fn t2_export_roundtrip() {
    let dir = temp_db_dir("t2");
    let db = connect(&dir).await;

    db.import_projects(&sample()).await.unwrap();
    let entries = db.export_projects().await.unwrap();
    let kdl = ProjectsFile { projects: entries }.to_kdl().unwrap();

    assert!(kdl.contains("vantage-point"), "export に project 名が出る");
    assert!(kdl.contains("creo-memories"));

    let back = ProjectsFile::from_kdl(&kdl).unwrap();
    assert_eq!(back.projects.len(), 2);
    assert_eq!(back.projects[0].path, "/repos/vantage-point", "順序保持");
    assert_eq!(back.projects[1].enabled, Some(false), "enabled=#false 保持");
    assert_eq!(back.projects[0].slot, Some(2), "slot 保持");

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// T3 ★: DB dir 消失（VP-182）→ 一方向 export からの import で復旧できる。
/// これが通れば VP-188 を revert して DB 真実源化する go 判定。
#[tokio::test]
async fn t3_db_loss_recovery_via_export() {
    let dir_a = temp_db_dir("t3a");

    // 1. DB に projects を入れ、export を kdl 文字列として退避（= projects.kdl 相当）
    let exported_kdl = {
        let db = connect(&dir_a).await;
        db.import_projects(&sample()).await.unwrap();
        let entries = db.export_projects().await.unwrap();
        let kdl = ProjectsFile { projects: entries }.to_kdl().unwrap();
        drop(db);
        kdl
    };

    // 2. DB dir 消失（VP-182 = DB dir 変更/消失で projects 全喪失のシナリオ）
    std::fs::remove_dir_all(&dir_a).unwrap();

    // 3. 新しい DB dir = 空。DB 単体では projects が失われていることを確認（VP-182 再現）
    let dir_b = temp_db_dir("t3b");
    let db2 = connect(&dir_b).await;
    assert_eq!(
        db2.list_projects().await.unwrap().len(),
        0,
        "DB 消失で projects は失われる（VP-182 が逃げた問題の再現）"
    );

    // 4. ★ 一方向 export（projects.kdl）から import → 復旧
    let recovered = ProjectsFile::from_kdl(&exported_kdl).unwrap();
    db2.import_projects(&recovered.projects).await.unwrap();
    let rows = db2.list_projects().await.unwrap();

    assert_eq!(
        rows.len(),
        2,
        "export/import backstop で projects 復旧（VP-188 の壁を正面突破）"
    );
    assert_eq!(rows[0]["name"].as_str(), Some("vantage-point"));
    assert_eq!(rows[1]["name"].as_str(), Some("creo-memories"));

    drop(db2);
    let _ = std::fs::remove_dir_all(&dir_b);
}
