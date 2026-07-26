//! `vp sync` — projects.kdl を現実と同期する。
//!
//! ghost project (= projects.kdl に登録されているが dir が実在しない) を除去する。
//! 起動時 (`vp app start` / `vp sp start`) にも自動 sync されるが、 本コマンドで
//! いつでも手動実行できる。

use anyhow::Result;

use crate::projects_file::ProjectsFile;

/// `vp sync` を実行。 projects.kdl から ghost project を除去する。
pub fn execute() -> Result<()> {
    // PR-D: ghost 除去を daemon (db/machine 真実源) 経由で行う。 daemon 不在は kdl フォールバック。
    let outcome = match crate::daemon_client::notify_daemon_sync() {
        Some(o) => o,
        None => ProjectsFile::sync()?,
    };
    if outcome.removed.is_empty() {
        println!("✅ projects.kdl は最新です (ghost project なし)");
    } else {
        println!(
            "🧹 ghost project を {} 件除去しました:",
            outcome.removed.len()
        );
        for name in &outcome.removed {
            println!("   - {name}");
        }
    }
    Ok(())
}
