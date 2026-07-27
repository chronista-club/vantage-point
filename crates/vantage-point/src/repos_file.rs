//! Registered repos の SSOT — `~/.config/vp/repos.kdl`
//!
//! ## 背景 (VP-188)
//!
//! registered repos (= ユーザーが登録した開発プロジェクト一覧) は元々
//! embedded SurrealDB の `repos` テーブルに保存されていた。 しかし VP-182 で
//! DB ディレクトリを分離した際、 旧 DB の repos が新ディレクトリに来ず、
//! **登録 repos が全消失** する regression が発生した。
//!
//! council (2026-05-16) の結論: repos は唯一の永続データであり、 ephemeral な
//! embedded DB に置いたのが設計ミス。 → **人間可読 file を SSOT に**。
//!
//! ## 設計
//!
//! - SSOT = `~/.config/vp/repos.kdl` (KDL 形式、 VP の club-kdl 資産と統一)
//! - config.kdl とは責務分離: config.kdl は人間が編集する global 設定、
//!   repos.kdl は VP が全権を持つ (= ファイル全体を serialize し直してよい)
//! - node 出現順 = sidebar 並び順 (= 明示 order field 不要)
//! - 書き込みは atomic write (temp file → rename) で partial read を防ぐ
//!
//! ## repos.kdl の形
//!
//! ```kdl
//! repo name="vantage-point" path="/Users/makoto/repos/vantage-point"
//! repo name="creo-memories" path="/Users/makoto/repos/creo-memories"
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use club_kdl::{KdlDeserialize, KdlSerialize};

/// repos.kdl の 1 repo エントリ
#[derive(Debug, Clone, KdlDeserialize, KdlSerialize)]
#[kdl(name = "repo")]
pub struct RepoEntry {
    /// 表示名
    #[kdl(property)]
    pub name: String,
    /// repoディレクトリの絶対パス
    #[kdl(property)]
    pub path: String,
    /// repo 自動起動の有効/無効。 省略時は有効 (= `true`)。
    /// `enabled=#false` の時だけ repos.kdl に明記される想定。
    #[kdl(property)]
    pub enabled: Option<bool>,
    /// Port slot (VP-165: deterministic port layout)。 一度割り当てたら永続。
    /// 未割当の repo は省略 (= `None`)。
    #[kdl(property)]
    pub slot: Option<u16>,
}

impl RepoEntry {
    /// `enabled` の実効値 (省略時 = 有効)。
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// repos.kdl 全体 (= repo node の列)
#[derive(Debug, Clone, Default, KdlDeserialize, KdlSerialize)]
#[kdl(document)]
pub struct ReposFile {
    /// 登録 repo 一覧。 Vec の順序 = sidebar 並び順。
    #[kdl(children, name = "repo")]
    pub repos: Vec<RepoEntry>,
}

/// repos.kdl のパス (`~/.config/vp/repos.kdl`)
pub fn repos_file_path() -> PathBuf {
    crate::config::config_dir().join("repos.kdl")
}

impl ReposFile {
    /// repos.kdl を読み込む。 ファイルが無ければ空の `ReposFile` を返す
    /// (= 初回起動 / 未登録状態を空 repos として扱う)。
    ///
    /// 読み込み時に verbatim prefix を落とす ([`Self::strip_verbatim_paths`])。
    pub fn load() -> Result<Self> {
        let mut pf = Self::load_raw()?;
        pf.strip_verbatim_paths();
        Ok(pf)
    }

    /// repos.kdl を verbatim prefix の正規化なしで読み込む (`sync` の移行判定用)。
    fn load_raw() -> Result<Self> {
        let path = repos_file_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("repos.kdl 読み込み失敗: {}", path.display()))?;
        // 空ファイルは空 repos 扱い (= club_kdl の空 document parse 差異を吸収)
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        club_kdl::from_str(&content)
            .with_context(|| format!("repos.kdl パース失敗: {}", path.display()))
    }

    /// 各 entry の path から Windows の verbatim prefix (`\\?\`) を落とす。 1 件でも変われば `true`。
    ///
    /// 旧版 (`std::fs::canonicalize` を使っていた頃) の Windows が保存した
    /// `path="\\?\C:\Users\..."` を読み込み時に修復する。 これを放置すると repo の spawn 引数
    /// (`vp sp start -C \\?\C:\...`) まで伝播し、 同じ dir が別 key として二重登録されうる。
    fn strip_verbatim_paths(&mut self) -> bool {
        let mut changed = false;
        for p in &mut self.repos {
            let stripped = crate::config::strip_verbatim_prefix(&p.path);
            if stripped.len() != p.path.len() {
                p.path = stripped.to_string();
                changed = true;
            }
        }
        changed
    }

    /// kdl 文字列に serialize（path 非依存、 PoC: DB→repos.kdl 一方向 export 用）。
    pub fn to_kdl(&self) -> Result<String> {
        club_kdl::to_string_pretty(self).context("repos.kdl シリアライズ失敗")
    }

    /// kdl 文字列から parse（path 非依存、 PoC: 復旧 import 用）。 空文字は空 repos。
    pub fn from_kdl(s: &str) -> Result<Self> {
        if s.trim().is_empty() {
            return Ok(Self::default());
        }
        let mut pf: Self = club_kdl::from_str(s).context("repos.kdl パース失敗")?;
        pf.strip_verbatim_paths();
        Ok(pf)
    }

    /// repos.kdl に書き出す。 atomic write (temp → rename) で partial read を防ぐ。
    ///
    /// テスト環境では no-op (= 本番 `~/.config/vp/repos.kdl` の破壊防止)。
    /// 全ての repos.kdl write 経路 (`persist_repos` / `persist_repos_kdl`) は
    /// 本メソッドを通るため、 ここで cfg(test) ガードすれば write 経路全体が test 安全。
    #[cfg(test)]
    pub fn save(&self) -> Result<()> {
        Ok(())
    }

    /// repos.kdl に書き出す。 atomic write (temp → rename) で partial read を防ぐ。
    #[cfg(not(test))]
    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        /// atomic write の temp file 名を経路ごとにユニークにする連番。
        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

        let path = repos_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("config dir 作成失敗: {}", parent.display()))?;
        }
        let body = club_kdl::to_string_pretty(self).context("repos.kdl シリアライズ失敗")?;

        // atomic write: 同一ディレクトリの temp file に書いて rename。
        // rename は同一ファイルシステム内で atomic、 reader は古い or 新しいファイルの
        // どちらか一方を必ず読む (= 中途半端な内容を読まない)。
        //
        // temp file 名は (pid + 連番) でユニークにする ── 固定名だと複数の write 経路
        // (`persist_repos` / `persist_repos_kdl` / `sync`) が並行で save した
        // とき同じ temp を奪い合い、 先に rename した側が temp を消すので後発の rename が
        // ENOENT で失敗する race になる (VP-189 dogfood で daemon 起動直後の slot 永続化が
        // 確定的に罹患)。 経路ごとに専用 temp を持てば rename は最後の書き手が atomic に勝つ。
        let tmp = path.with_file_name(format!(
            "repos.kdl.{}.{}.tmp",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("repos.kdl temp 作成失敗: {}", tmp.display()))?;
            f.write_all(body.as_bytes())
                .context("repos.kdl temp 書き込み失敗")?;
            f.sync_all().context("repos.kdl temp fsync 失敗")?;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            // rename にコケたら temp が残るので掃除 (leak 防止)。
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("repos.kdl rename 失敗: {}", path.display()));
        }
        Ok(())
    }
}

/// `pf` から「`dir_exists` が `false` を返す path」 の entry を除去する純粋ロジック。
///
/// 除去した repo 名を出現順で返す。 dir 実在判定 (`dir_exists`) を注入式に
/// したので fs 非依存で単体テストできる。
fn prune_ghosts_with<F: Fn(&str) -> bool>(pf: &mut ReposFile, dir_exists: F) -> Vec<String> {
    let mut removed = Vec::new();
    pf.repos.retain(|p| {
        if dir_exists(&p.path) {
            true
        } else {
            removed.push(p.name.clone());
            false
        }
    });
    removed
}

/// 稼働中の daemon (daemon) に repos.kdl の reload を通知する (best-effort)。
///
/// VP-189: `ReposFile::sync` が repos.kdl を書き換えても、 既に稼働している
/// daemon は in-memory repos を保持したままで乖離する。 daemon に repos.kdl を
/// 読み直させる (doc 45 段 2 で `POST /api/daemon/repos/reload` から Unison
/// `daemon-control.repos/reload` に差し替え、意味論は同じ best-effort)。
///
/// daemon が動いていなければ黙って無視する (= 次回 daemon 起動時の `load_config` で
/// repos.kdl が読まれるため取りこぼしにならない)。 テスト環境では no-op。
#[cfg(test)]
fn notify_daemon_reload() {}

/// 稼働中の daemon (daemon) に repos.kdl の reload を通知する (best-effort)。
#[cfg(not(test))]
fn notify_daemon_reload() {
    crate::daemon_client::notify_daemon_reload();
}

/// [`ReposFile::sync`] の結果サマリ。
#[derive(Debug, Default)]
pub struct SyncOutcome {
    /// ghost (dir 実在せず) として除去した repo 名。
    pub removed: Vec<String>,
}

impl SyncOutcome {
    /// repos.kdl に変更があったか。
    pub fn changed(&self) -> bool {
        !self.removed.is_empty()
    }
}

impl ReposFile {
    /// repos.kdl から ghost repo を除去して現実と同期する (VP-189 follow-up)。
    ///
    /// path が実在しない (dir が削除/移動された) ghost repo を repos.kdl から
    /// 除去する。 サイドバーに出るが開けない死に entry を掃除する。 変更があったときだけ
    /// save する (= repos.kdl への無駄な書き込みを避ける)。
    ///
    /// かつて `start_dir` で「起点 dir の自動登録」も行っていたが、 `vp sp start` の
    /// 起動時 sync が **削除済 repo を復活させる** resurrection バグの温床だったため
    /// 撤去した (#721)。 repo 登録は `add_repo` 経由の明示操作のみ (sidebar Add /
    /// `vp repos add`)。
    ///
    /// 併せて、 旧 Windows が保存した verbatim prefix (`\\?\C:\...`) を sync 時に恒久除去する
    /// (read 側は `load()` が毎回落とすが、 sync は file を書き戻す機会なので repos.kdl の
    /// 見た目も治す)。
    pub fn sync() -> Result<SyncOutcome> {
        let mut pf = ReposFile::load_raw()?;

        // 0. 旧 Windows が保存した verbatim prefix (`\\?\C:\...`) を落とす (永続除去)。
        let migrated = pf.strip_verbatim_paths();

        // ghost 除去: path が実在しない (ディレクトリでない) entry を落とす。
        let outcome = SyncOutcome {
            removed: prune_ghosts_with(&mut pf, |path| std::path::Path::new(path).is_dir()),
        };

        if outcome.changed() || migrated {
            pf.save()?;
            // 稼働中 daemon に repos.kdl の変更を伝える (best-effort)。
            notify_daemon_reload();
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 Windows が保存した `\\?\C:\...` は読み込み時に落ちる (移行)。 UNC は温存。
    #[test]
    fn from_kdl_strips_verbatim_prefix() {
        let pf = ReposFile {
            repos: vec![
                RepoEntry {
                    name: "vantage-point".to_string(),
                    path: r"\\?\C:\Users\mito\repos\vantage-point".to_string(),
                    enabled: None,
                    slot: Some(1),
                },
                RepoEntry {
                    name: "on-share".to_string(),
                    path: r"\\?\UNC\server\share\proj".to_string(),
                    enabled: None,
                    slot: None,
                },
            ],
        };
        let back = ReposFile::from_kdl(&pf.to_kdl().expect("serialize")).expect("parse");
        assert_eq!(back.repos[0].path, r"C:\Users\mito\repos\vantage-point");
        assert_eq!(back.repos[1].path, r"\\?\UNC\server\share\proj");
    }

    /// 正規化不要な repos.kdl では `strip_verbatim_paths` が false (= 無駄な save をしない)。
    #[test]
    fn strip_verbatim_paths_reports_change() {
        let mut clean = ReposFile {
            repos: vec![RepoEntry {
                name: "vp".to_string(),
                path: "/Users/makoto/repos/vp".to_string(),
                enabled: None,
                slot: None,
            }],
        };
        assert!(!clean.strip_verbatim_paths());

        let mut dirty = ReposFile {
            repos: vec![RepoEntry {
                name: "vp".to_string(),
                path: r"\\?\C:\vp".to_string(),
                enabled: None,
                slot: None,
            }],
        };
        assert!(dirty.strip_verbatim_paths());
        assert_eq!(dirty.repos[0].path, r"C:\vp");
    }

    /// ReposFile の KDL round-trip (serialize → parse で等価)
    #[test]
    fn repos_file_round_trip() {
        let pf = ReposFile {
            repos: vec![
                RepoEntry {
                    name: "vantage-point".to_string(),
                    path: "/Users/makoto/repos/vantage-point".to_string(),
                    enabled: None,
                    slot: Some(2),
                },
                RepoEntry {
                    name: "creo-memories".to_string(),
                    path: "/Users/makoto/repos/creo-memories".to_string(),
                    enabled: Some(false),
                    slot: None,
                },
            ],
        };
        let kdl = club_kdl::to_string_pretty(&pf).expect("serialize");
        let back: ReposFile = club_kdl::from_str(&kdl).expect("parse");
        assert_eq!(back.repos.len(), 2);
        // node 出現順 = 並び順が保たれること
        assert_eq!(back.repos[0].name, "vantage-point");
        assert_eq!(back.repos[1].name, "creo-memories");
        assert_eq!(back.repos[1].path, "/Users/makoto/repos/creo-memories");
        // enabled: 省略 → is_enabled() = true、 明示 false は保持
        assert!(back.repos[0].is_enabled());
        assert!(!back.repos[1].is_enabled());
        // slot: VP-165 port layout、 round-trip で保持されること
        assert_eq!(back.repos[0].slot, Some(2));
        assert_eq!(back.repos[1].slot, None);
    }

    /// 空 ReposFile の round-trip
    #[test]
    fn empty_repos_file_round_trip() {
        let pf = ReposFile::default();
        let kdl = club_kdl::to_string_pretty(&pf).expect("serialize");
        let back: ReposFile = club_kdl::from_str(&kdl).unwrap_or_default();
        assert_eq!(back.repos.len(), 0);
    }

    /// VP-189: ghost repo (dir 実在せず) のみ除去する (prune_ghosts_with 純粋ロジック)
    #[test]
    fn prune_ghosts_with_removes_only_nonexistent_dirs() {
        let mut pf = ReposFile {
            repos: vec![
                RepoEntry {
                    name: "alive-a".into(),
                    path: "/repos/a".into(),
                    enabled: None,
                    slot: Some(0),
                },
                RepoEntry {
                    name: "ghost".into(),
                    path: "/repos/gone".into(),
                    enabled: None,
                    slot: Some(1),
                },
                RepoEntry {
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
        assert_eq!(pf.repos.len(), 2);
        assert_eq!(pf.repos[0].name, "alive-a");
        assert_eq!(pf.repos[1].name, "alive-b");
    }

    /// VP-189: 全 dir が実在すれば何も除去しない
    #[test]
    fn prune_ghosts_with_keeps_all_when_all_exist() {
        let mut pf = ReposFile {
            repos: vec![RepoEntry {
                name: "a".into(),
                path: "/repos/a".into(),
                enabled: None,
                slot: None,
            }],
        };
        let removed = prune_ghosts_with(&mut pf, |_| true);
        assert!(removed.is_empty());
        assert_eq!(pf.repos.len(), 1);
    }
}
