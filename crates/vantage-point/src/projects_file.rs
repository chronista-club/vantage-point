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
//! - config.kdl とは責務分離: config.kdl は人間が編集する global 設定、
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
        use std::sync::atomic::{AtomicU64, Ordering};

        /// atomic write の temp file 名を経路ごとにユニークにする連番。
        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

        let path = projects_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("config dir 作成失敗: {}", parent.display()))?;
        }
        let body = club_kdl::to_string_pretty(self).context("projects.kdl シリアライズ失敗")?;

        // atomic write: 同一ディレクトリの temp file に書いて rename。
        // rename は同一ファイルシステム内で atomic、 reader は古い or 新しいファイルの
        // どちらか一方を必ず読む (= 中途半端な内容を読まない)。
        //
        // temp file 名は (pid + 連番) でユニークにする ── 固定名だと複数の write 経路
        // (`persist_projects` / `persist_projects_kdl` / `sync`) が並行で save した
        // とき同じ temp を奪い合い、 先に rename した側が temp を消すので後発の rename が
        // ENOENT で失敗する race になる (VP-189 dogfood で daemon 起動直後の slot 永続化が
        // 確定的に罹患)。 経路ごとに専用 temp を持てば rename は最後の書き手が atomic に勝つ。
        let tmp = path.with_file_name(format!(
            "projects.kdl.{}.{}.tmp",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("projects.kdl temp 作成失敗: {}", tmp.display()))?;
            f.write_all(body.as_bytes())
                .context("projects.kdl temp 書き込み失敗")?;
            f.sync_all().context("projects.kdl temp fsync 失敗")?;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            // rename にコケたら temp が残るので掃除 (leak 防止)。
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("projects.kdl rename 失敗: {}", path.display()));
        }
        Ok(())
    }
}

/// `dir_path` が `pf` に未登録なら `ProjectEntry` を追加する純粋ロジック。
///
/// 登録したら `true`、 既に同 path が登録済みなら `false` (idempotent)。
/// fs / cwd に触らないので単体テストできる。
fn register_dir_into(pf: &mut ProjectsFile, dir_path: &str, name: &str) -> bool {
    if pf.projects.iter().any(|p| p.path == dir_path) {
        return false;
    }
    pf.projects.push(ProjectEntry {
        name: name.to_string(),
        path: dir_path.to_string(),
        enabled: None,
        slot: None,
    });
    true
}

/// `pf` から「`dir_exists` が `false` を返す path」 の entry を除去する純粋ロジック。
///
/// 除去した project 名を出現順で返す。 dir 実在判定 (`dir_exists`) を注入式に
/// したので fs 非依存で単体テストできる。
fn prune_ghosts_with<F: Fn(&str) -> bool>(pf: &mut ProjectsFile, dir_exists: F) -> Vec<String> {
    let mut removed = Vec::new();
    pf.projects.retain(|p| {
        if dir_exists(&p.path) {
            true
        } else {
            removed.push(p.name.clone());
            false
        }
    });
    removed
}

/// 稼働中の daemon (TheWorld) に projects.kdl の reload を通知する (best-effort)。
///
/// VP-189: `ProjectsFile::sync` が projects.kdl を書き換えても、 既に稼働している
/// daemon は in-memory projects を保持したままで乖離する。 `POST /api/world/projects/reload`
/// を叩いて daemon に projects.kdl を読み直させる。
///
/// daemon が動いていなければ黙って無視する (= 次回 daemon 起動時の `load_config` で
/// projects.kdl が読まれるため取りこぼしにならない)。 テスト環境では no-op。
#[cfg(test)]
fn notify_daemon_reload() {}

/// 稼働中の daemon (TheWorld) に projects.kdl の reload を通知する (best-effort)。
#[cfg(not(test))]
fn notify_daemon_reload() {
    let url = format!(
        "http://[::1]:{}/api/world/projects/reload",
        crate::cli::WORLD_PORT
    );
    if let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        // best-effort: daemon 不在・エラーは無視 (projects.kdl 自体は更新済)。
        let _ = client.post(&url).send();
    }
}

/// [`ProjectsFile::sync`] の結果サマリ。
#[derive(Debug, Default)]
pub struct SyncOutcome {
    /// 新規登録した起点 project 名 (起動時 sync で起点 dir が未登録だった場合)。
    pub added: Option<String>,
    /// ghost (dir 実在せず) として除去した project 名。
    pub removed: Vec<String>,
}

impl SyncOutcome {
    /// projects.kdl に変更があったか。
    pub fn changed(&self) -> bool {
        self.added.is_some() || !self.removed.is_empty()
    }
}

impl ProjectsFile {
    /// projects.kdl を現実と同期する (VP-189 follow-up)。
    ///
    /// 2 つの操作を 1 回の load → save にまとめる:
    ///
    /// 1. **起点 dir の登録** — `start_dir` が `Some` なら、 その起点ディレクトリを
    ///    project として登録 (未登録なら)。 VP のメンタルモデル「起点ディレクトリ →
    ///    そこから SP が立ち上がる」 に沿い、 `vp app start` / `vp sp start` で
    ///    起点 dir を VP に追加する (起動アクション = project 追加)。 `dir` は
    ///    そのまま (正規化のみ) project になる ── git repo root への丸めや git 判定は
    ///    しない (D11「正規化ディレクトリパスが Process の一意キー」)。
    /// 2. **ghost 除去** — path が実在しない (dir が削除/移動された) ghost project を
    ///    projects.kdl から除去する。 サイドバーに出るが開けない死に entry を掃除。
    ///
    /// `vp sync` (明示コマンド) は `start_dir = None` で呼び ghost 除去のみ。 起動時
    /// (`vp app start` / `vp sp start`) は起点 dir を渡し「追加 + 除去」 を同時に行う。
    /// 変更があったときだけ save する (= projects.kdl への無駄な書き込みを避ける)。
    pub fn sync(start_dir: Option<&std::path::Path>) -> Result<SyncOutcome> {
        let mut pf = ProjectsFile::load()?;
        let mut outcome = SyncOutcome::default();

        // 1. 起点 dir を登録 (起動時 sync のみ)。
        if let Some(dir) = start_dir {
            // 他の project entry と比較可能なよう正規化パスに揃える。
            let dir_path = crate::config::Config::normalize_path(dir);
            let name = std::path::Path::new(&dir_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string();
            if register_dir_into(&mut pf, &dir_path, &name) {
                outcome.added = Some(name);
            }
        }

        // 2. ghost 除去: path が実在しない (ディレクトリでない) entry を落とす。
        outcome.removed = prune_ghosts_with(&mut pf, |path| std::path::Path::new(path).is_dir());

        if outcome.changed() {
            pf.save()?;
            // 稼働中 daemon に projects.kdl の変更を伝える (best-effort)。
            notify_daemon_reload();
        }
        Ok(outcome)
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

    /// VP-189: 起点ディレクトリの新規登録と重複スキップ (register_dir_into 純粋ロジック)
    #[test]
    fn register_dir_into_adds_new_and_skips_duplicate() {
        let mut pf = ProjectsFile::default();
        // 新規登録 → true、 entry 追加
        assert!(register_dir_into(
            &mut pf,
            "/repos/vantage-point",
            "vantage-point"
        ));
        assert_eq!(pf.projects.len(), 1);
        assert_eq!(pf.projects[0].name, "vantage-point");
        assert_eq!(pf.projects[0].path, "/repos/vantage-point");
        // enabled / slot は省略 (None) で登録される
        assert!(pf.projects[0].enabled.is_none());
        assert!(pf.projects[0].slot.is_none());
        // 同 path の再登録 → false、 件数不変 (idempotent)
        assert!(!register_dir_into(&mut pf, "/repos/vantage-point", "別名"));
        assert_eq!(pf.projects.len(), 1);
        // 別 path → true、 件数増加
        assert!(register_dir_into(
            &mut pf,
            "/repos/creo-memories",
            "creo-memories"
        ));
        assert_eq!(pf.projects.len(), 2);
    }

    /// VP-189: ghost project (dir 実在せず) のみ除去する (prune_ghosts_with 純粋ロジック)
    #[test]
    fn prune_ghosts_with_removes_only_nonexistent_dirs() {
        let mut pf = ProjectsFile {
            projects: vec![
                ProjectEntry {
                    name: "alive-a".into(),
                    path: "/repos/a".into(),
                    enabled: None,
                    slot: Some(0),
                },
                ProjectEntry {
                    name: "ghost".into(),
                    path: "/repos/gone".into(),
                    enabled: None,
                    slot: Some(1),
                },
                ProjectEntry {
                    name: "alive-b".into(),
                    path: "/repos/b".into(),
                    enabled: None,
                    slot: None,
                },
            ],
        };
        // "/repos/gone" だけ実在しない扱い
        let removed = prune_ghosts_with(&mut pf, |path| path != "/repos/gone");
        assert_eq!(removed, vec!["ghost".to_string()]);
        // 生存 entry は出現順を保って残る
        assert_eq!(pf.projects.len(), 2);
        assert_eq!(pf.projects[0].name, "alive-a");
        assert_eq!(pf.projects[1].name, "alive-b");
    }

    /// VP-189: 全 dir が実在すれば何も除去しない
    #[test]
    fn prune_ghosts_with_keeps_all_when_all_exist() {
        let mut pf = ProjectsFile {
            projects: vec![ProjectEntry {
                name: "a".into(),
                path: "/repos/a".into(),
                enabled: None,
                slot: None,
            }],
        };
        let removed = prune_ghosts_with(&mut pf, |_| true);
        assert!(removed.is_empty());
        assert_eq!(pf.projects.len(), 1);
    }
}
