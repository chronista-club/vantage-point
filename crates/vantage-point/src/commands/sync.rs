//! `vp sync` — projects.kdl を現実と同期する。
//!
//! ghost project (= projects.kdl に登録されているが dir が実在しない) を除去する。
//! 起動時 (`vp app start` / `vp sp start`) にも自動 sync されるが、 本コマンドで
//! いつでも手動実行できる。

use anyhow::Result;

use crate::projects_file::ProjectsFile;

/// `vp sync` を実行。 projects.kdl から ghost project を除去する。
pub fn execute() -> Result<()> {
    // 明示コマンドは起点 dir 登録なし (= ghost 除去のみ)。
    let outcome = ProjectsFile::sync(None)?;
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
