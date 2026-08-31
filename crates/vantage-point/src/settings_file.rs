//! user の「好み」の SSOT — `~/.config/vp/settings.kdl`（doc 59）
//!
//! ## なぜ config.kdl と分けるか
//!
//! 設定は「誰が書くか」ではなく **「何に属するか」** で割る（doc 59 §1）:
//!
//! | 層 | 何に属するか | 置き場所 | 書き手 |
//! |---|---|---|---|
//! | **環境** | このマシンの事実（claude-cli-path / hub-addr） | `config.kdl` | 人だけ |
//! | **好み** | **user 本人**（ログ詳細度 / 既定 agent × model / theme） | **本 file** | daemon |
//! | **作業** | 今なにを開いているか | `repos.kdl` | VP |
//!
//! `claude-cli-path` はマシンを移ると無意味になるが、「ログは debug で見たい」は人に
//! 付いていく。寿命も持ち運び先も違うものが config.kdl に同居していたのが元の捻れだった。
//!
//! ## 誰が書くか
//!
//! **daemon が唯一の書き手**（[`crate::repos_file`] と同じ流儀）。GUI / CLI は daemon に
//! 頼む形にして、同時書き込みで壊れる余地を構造的に消す。ただし **人が手で編集しても
//! 構わない** — 人間可読な KDL であることが repos.kdl から引き継いだ利点で、
//! daemon の次の読み込みで拾われる。
//!
//! ## 優先順位
//!
//! **env > settings.kdl > 組み込み既定**。env を最優先に残すのは、既存の逃げ道
//! （`VP_LOG` / `VANTAGE_DEBUG`）を壊さないため。⚠️ `config.kdl` とはキーを**重複させない** —
//! 同じ設定が 2 箇所にあると「どちらが勝つか」を覚える必要が生まれる。
//!
//! ## 不正値は落とさず degrade する
//!
//! 綴り違いは**その key だけ無視**して既定に倒す（`Config::default_lane_model` と同じ流儀）。
//! 設定 file の typo 1 つで daemon が起動しなくなるほうが害が大きい。

use std::path::PathBuf;

use anyhow::{Context, Result};
use club_kdl::{KdlDeserialize, KdlSerialize};

/// 受理するログ詳細度（`tracing` の level 名）。
///
/// 表記は `VP_LOG` と揃えてある（user が env で使い慣れた語をそのまま設定にも書ける）。
const LOG_LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// settings.kdl 全体。
///
/// **全 field が `Option`** = 「書かれていない」を表現できる形にしてある。未設定と
/// 明示的な既定値は意味が違う（未設定なら将来 VP 側の既定を変えたときに追随するが、
/// 明示値は user の意思として固定される）。
#[derive(Debug, Clone, Default, PartialEq, KdlDeserialize, KdlSerialize)]
#[kdl(document)]
pub struct SettingsFile {
    /// ログ詳細度（`trace` / `debug` / `info` / `warn` / `error`）。
    ///
    /// ⚠️ **daemon の起動時にしか読まれない**（`init_tracing` が `EnvFilter` を 1 回作って
    /// 固定する。reload layer は未導入 — doc 59 §5）。設定を変えたら daemon 再起動が要る。
    #[kdl(child, name = "log-level", unwrap_arg)]
    pub log_level: Option<String>,
}

/// settings.kdl のパス（`~/.config/vp/settings.kdl`）。
pub fn settings_file_path() -> PathBuf {
    crate::config::config_dir().join("settings.kdl")
}

impl SettingsFile {
    /// settings.kdl を読み込む。**存在しない / 壊れている場合は既定**（= 全て未設定）。
    ///
    /// パース失敗で `Err` を返さないのは、設定 file の typo が daemon の起動を妨げないため
    /// （module doc の degrade 方針）。壊れていることは warn で残す。
    pub fn load() -> Self {
        let path = settings_file_path();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(s) => match Self::from_kdl(&s) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(
                        "settings.kdl のパースに失敗（既定で続行）: {} — {e}",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "settings.kdl の読み込みに失敗（既定で続行）: {} — {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// kdl 文字列から parse（path 非依存 = テスト可能）。空文字は既定。
    pub fn from_kdl(s: &str) -> Result<Self> {
        if s.trim().is_empty() {
            return Ok(Self::default());
        }
        club_kdl::from_str(s).context("settings.kdl パース失敗")
    }

    /// kdl 文字列へ（path 非依存 = テスト可能）。
    pub fn to_kdl(&self) -> Result<String> {
        club_kdl::to_string_pretty(self).context("settings.kdl シリアライズ失敗")
    }

    /// ログ詳細度の**実効値**。未設定 / 綴り違いは `None`（= 既定に倒す）。
    ///
    /// 検証をここに置くのは、読み手（`init_tracing`）が綴りを知らなくて済むようにするため。
    /// 不正値を素通しすると `EnvFilter` の directive が壊れて**全ログが黙る**（= 設定 file の
    /// typo が観測手段そのものを奪う）。
    pub fn log_level_valid(&self) -> Option<&str> {
        self.log_level
            .as_deref()
            .map(str::trim)
            .filter(|v| LOG_LEVELS.contains(&v.to_ascii_lowercase().as_str()))
    }

    /// settings.kdl に書き出す。テスト環境では **no-op**（本番 `~/.config/vp/settings.kdl` の
    /// 破壊防止 — [`crate::repos_file::ReposFile::save`] と同じ規律）。
    #[cfg(test)]
    pub fn save(&self) -> Result<()> {
        Ok(())
    }

    /// settings.kdl に書き出す。atomic write（temp → rename）で partial read を防ぐ。
    ///
    /// rename は同一ファイルシステム内で atomic なので、読み手は**古い file か新しい file の
    /// どちらか**しか見ない（書きかけを読むことがない）。全ての write 経路がここを通る。
    #[cfg(not(test))]
    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        /// atomic write の temp file 名を経路ごとにユニークにする連番。
        static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

        let path = settings_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("config dir 作成失敗: {}", parent.display()))?;
        }
        let body = self.to_kdl()?;

        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("kdl.tmp{}.{seq}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("settings.kdl temp 作成失敗: {}", tmp.display()))?;
            f.write_all(HEADER.as_bytes())?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("settings.kdl rename 失敗: {}", path.display()))?;
        tracing::info!("settings.kdl 保存: {}", path.display());
        Ok(())
    }
}

/// 書き出す file の先頭に置く案内。**人が開いたときに「何を書く場所か」が分かる**ように
/// （この file は VP が書くが、人が手で編集することも許している）。
#[cfg_attr(test, allow(dead_code))]
const HEADER: &str = "\
// VP の user 設定（好み）— VP が読み書きします。手で編集しても構いません。
//
// ここに置くのは「あなたに属する設定」です。マシン固有の環境（claude-cli-path /
// hub-addr）は config.kdl の担当で、キーは重複しません。
// 優先順位: 環境変数 > このファイル > 組み込み既定。
//
// 変更の反映には daemon の再起動が要ります（vp daemon restart）。

";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_missing_yields_default() {
        assert_eq!(SettingsFile::from_kdl("").unwrap(), SettingsFile::default());
        assert_eq!(
            SettingsFile::from_kdl("   \n  ").unwrap(),
            SettingsFile::default()
        );
    }

    #[test]
    fn log_level_roundtrips_through_kdl() {
        let f = SettingsFile {
            log_level: Some("debug".to_string()),
        };
        let kdl = f.to_kdl().unwrap();
        assert!(kdl.contains("log-level"), "kdl: {kdl}");
        // ⚠️ 往復で固定する — serialize / deserialize の片側だけ直すと wire が黙ってズレる。
        assert_eq!(SettingsFile::from_kdl(&kdl).unwrap(), f);
    }

    #[test]
    fn invalid_log_level_degrades_to_none() {
        // 設定 file の typo 1 つで EnvFilter の directive が壊れると**全ログが黙る**ため、
        // 読み手に渡す前にここで落とす。
        let f = SettingsFile {
            log_level: Some("verbose".to_string()),
        };
        assert_eq!(f.log_level_valid(), None);
    }

    #[test]
    fn log_level_is_case_insensitive() {
        let f = SettingsFile {
            log_level: Some("DEBUG".to_string()),
        };
        // 受理はするが、値はそのまま返す（呼び手が lowercase 化して使う）。
        assert_eq!(f.log_level_valid(), Some("DEBUG"));
    }

    #[test]
    fn log_level_trims_surrounding_whitespace() {
        let f = SettingsFile {
            log_level: Some("  info  ".to_string()),
        };
        assert_eq!(f.log_level_valid(), Some("info"));
    }

    #[test]
    fn all_known_levels_are_accepted() {
        for lv in LOG_LEVELS {
            let f = SettingsFile {
                log_level: Some(lv.to_string()),
            };
            assert_eq!(f.log_level_valid(), Some(lv), "level={lv}");
        }
    }
}
