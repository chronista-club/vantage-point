//! Registered projects の SSOT — `~/.config/vp/projects.kdl`
//!
//! ## 背景 (VP-188)
//!
//! registered projects (= ユーザーが登録した開発プロジェクト一覧) は元々
//! embedded SurrealDB の `projects` テーブルに保存されていた。 しかし VP-182 で
//! DB ディレクトリを分離した際、 旧 DB の projects が新ディレクトリに来ず、
//! **登録 projects が全消失** する regression が発生した。
//!
//! council (2026-05-16) の結論: projects は唯一の永続データであり、 ephemeral な
//! embedded DB に置いたのが設計ミス。 → **人間可読 file を SSOT に**。
//!
//! ## 設計
//!
//! - SSOT = `~/.config/vp/projects.kdl` (KDL 形式、 VP の club-kdl 資産と統一)
//! - config.toml とは責務分離: config.toml は人間が編集する global 設定、
//!   projects.kdl は VP が全権を持つ (= ファイル全体を serialize し直してよい)
//! - node 出現順 = sidebar 並び順 (= 明示 order field 不要)
//! - 書き込みは atomic write (temp file → rename) で partial read を防ぐ
//!
//! ## projects.kdl の形
//!
//! ```kdl
//! project name="vantage-point" path="/Users/makoto/repos/vantage-point"
//! project name="creo-memories" path="/Users/makoto/repos/creo-memories"
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use club_kdl::{KdlDeserialize, KdlSerialize};

/// projects.kdl の 1 project エントリ
#[derive(Debug, Clone, KdlDeserialize, KdlSerialize)]
#[kdl(name = "project")]
pub struct ProjectEntry {
    /// 表示名
    #[kdl(property)]
    pub name: String,
    /// プロジェクトディレクトリの絶対パス
    #[kdl(property)]
    pub path: String,
    /// SP 自動起動の有効/無効。 省略時は有効 (= `true`)。
    /// `enabled=#false` の時だけ projects.kdl に明記される想定。
    #[kdl(property)]
    pub enabled: Option<bool>,
    /// Port slot (VP-165: deterministic port layout)。 一度割り当てたら永続。
    /// 未割当の project は省略 (= `None`)。
    #[kdl(property)]
    pub slot: Option<u16>,
}

impl ProjectEntry {
    /// `enabled` の実効値 (省略時 = 有効)。
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// projects.kdl 全体 (= project node の列)
#[derive(Debug, Clone, Default, KdlDeserialize, KdlSerialize)]
#[kdl(document)]
pub struct ProjectsFile {
    /// 登録 project 一覧。 Vec の順序 = sidebar 並び順。
    #[kdl(children, name = "project")]
    pub projects: Vec<ProjectEntry>,
}

/// projects.kdl のパス (`~/.config/vp/projects.kdl`)
pub fn projects_file_path() -> PathBuf {
    crate::config::config_dir().join("projects.kdl")
}

impl ProjectsFile {
    /// projects.kdl を読み込む。 ファイルが無ければ空の `ProjectsFile` を返す
    /// (= 初回起動 / 未登録状態を空 projects として扱う)。
    pub fn load() -> Result<Self> {
        let path = projects_file_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("projects.kdl 読み込み失敗: {}", path.display()))?;
        // 空ファイルは空 projects 扱い (= club_kdl の空 document parse 差異を吸収)
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        club_kdl::from_str(&content)
            .with_context(|| format!("projects.kdl パース失敗: {}", path.display()))
    }

    /// projects.kdl に書き出す。 atomic write (temp → rename) で partial read を防ぐ。
    ///
    /// テスト環境では no-op (= 本番 `~/.config/vp/projects.kdl` の破壊防止)。
    /// 全ての projects.kdl write 経路 (`persist_projects` / `persist_projects_kdl`) は
    /// 本メソッドを通るため、 ここで cfg(test) ガードすれば write 経路全体が test 安全。
    #[cfg(test)]
    pub fn save(&self) -> Result<()> {
        Ok(())
    }

    /// projects.kdl に書き出す。 atomic write (temp → rename) で partial read を防ぐ。
    #[cfg(not(test))]
    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        let path = projects_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("config dir 作成失敗: {}", parent.display()))?;
        }
        let body = club_kdl::to_string_pretty(self).context("projects.kdl シリアライズ失敗")?;

        // atomic write: 同一ディレクトリの temp file に書いて rename。
        // rename は同一ファイルシステム内で atomic、 reader は古い or 新しいファイルの
        // どちらか一方を必ず読む (= 中途半端な内容を読まない)。
        let tmp = path.with_extension("kdl.tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("projects.kdl temp 作成失敗: {}", tmp.display()))?;
            f.write_all(body.as_bytes())
                .context("projects.kdl temp 書き込み失敗")?;
            f.sync_all().context("projects.kdl temp fsync 失敗")?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("projects.kdl rename 失敗: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ProjectsFile の KDL round-trip (serialize → parse で等価)
    #[test]
    fn projects_file_round_trip() {
        let pf = ProjectsFile {
            projects: vec![
                ProjectEntry {
                    name: "vantage-point".to_string(),
                    path: "/Users/makoto/repos/vantage-point".to_string(),
                    enabled: None,
                    slot: Some(2),
                },
                ProjectEntry {
                    name: "creo-memories".to_string(),
                    path: "/Users/makoto/repos/creo-memories".to_string(),
                    enabled: Some(false),
                    slot: None,
                },
            ],
        };
        let kdl = club_kdl::to_string_pretty(&pf).expect("serialize");
        let back: ProjectsFile = club_kdl::from_str(&kdl).expect("parse");
        assert_eq!(back.projects.len(), 2);
        // node 出現順 = 並び順が保たれること
        assert_eq!(back.projects[0].name, "vantage-point");
        assert_eq!(back.projects[1].name, "creo-memories");
        assert_eq!(back.projects[1].path, "/Users/makoto/repos/creo-memories");
        // enabled: 省略 → is_enabled() = true、 明示 false は保持
        assert!(back.projects[0].is_enabled());
        assert!(!back.projects[1].is_enabled());
        // slot: VP-165 port layout、 round-trip で保持されること
        assert_eq!(back.projects[0].slot, Some(2));
        assert_eq!(back.projects[1].slot, None);
    }

    /// 空 ProjectsFile の round-trip
    #[test]
    fn empty_projects_file_round_trip() {
        let pf = ProjectsFile::default();
        let kdl = club_kdl::to_string_pretty(&pf).expect("serialize");
        let back: ProjectsFile = club_kdl::from_str(&kdl).unwrap_or_default();
        assert_eq!(back.projects.len(), 0);
    }
}
