//! ユーザー設定の永続化 (VP-100 follow-up)
//!
//! `~/.config/vantage/vp-app.toml` (Linux), `~/Library/Application Support/vantage/vp-app.toml` (macOS),
//! `%APPDATA%\vantage\vp-app.toml` (Windows) に TOML で保存する。
//!
//! `vp` daemon 側が使う `~/.config/vantage/config.toml` とは別ファイル
//! (vp-app 固有の UI 設定なので分離)。
//!
//! 例:
//! ```toml
//! developer_mode = true
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Settings ファイル名
const SETTINGS_FILE: &str = "vp-app.toml";

/// vp-app の永続設定
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// 開発者モード (DevTools 有効化等)。
    /// `None` = 未設定 (env var or `cfg!(debug_assertions)` にフォールバック)。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub developer_mode: Option<bool>,
    /// デフォルトのプロジェクトルートディレクトリ。
    ///
    /// - Add Project ボタンの folder picker の初期ディレクトリ
    /// - Clone Repository の clone 先親ディレクトリ
    ///
    /// `None` の時は `~/repos` (存在すれば) → home dir のフォールバック。
    /// WSL2 上の Linux home を Windows-native の vp-app から指したい場合は
    /// `\\\\wsl$\\<distro>\\home\\<user>\\repos` のような UNC path を入れる。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default_project_root: Option<String>,
}

impl Settings {
    /// Settings ファイルのフルパスを返す
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("vantage").join(SETTINGS_FILE))
    }

    /// 設定ファイルを読み込む。存在しなければ `Default`。
    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            tracing::warn!("config_dir 取得失敗、Settings::default() を使用");
            return Self::default();
        };
        if !p.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&p) {
            Ok(s) => match toml::from_str::<Settings>(&s) {
                Ok(settings) => {
                    tracing::info!("Settings 読込: {}", p.display());
                    settings
                }
                Err(e) => {
                    tracing::warn!(
                        "Settings TOML パース失敗 ({}): {} - default を使用",
                        p.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Settings 読込失敗 ({}): {} - default を使用",
                    p.display(),
                    e
                );
                Self::default()
            }
        }
    }

    /// 設定ファイルを書き込む。親ディレクトリが無ければ作成する。
    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("config_dir が解決できない"))?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(&p, s)?;
        tracing::info!("Settings 保存: {}", p.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_none() {
        let s = Settings::default();
        assert_eq!(s.developer_mode, None);
    }

    #[test]
    fn round_trip_toml() {
        let s = Settings {
            developer_mode: Some(true),
            default_project_root: Some("/home/mito/repos".to_string()),
        };
        let toml = toml::to_string(&s).unwrap();
        let parsed: Settings = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.developer_mode, Some(true));
        assert_eq!(
            parsed.default_project_root.as_deref(),
            Some("/home/mito/repos")
        );
    }
}
