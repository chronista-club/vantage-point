//! Configuration management
//!
//! Config file location: ~/.config/vp/config.toml
//! 全プラットフォームで ~/.config/vp/ を使用（XDG準拠）

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Config directory for vp (~/.config/vp/)
/// 全プラットフォームで統一（macOS/Linux）
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("vp")
}

/// Data directory for vp (same as config_dir for simplicity)
pub fn data_dir() -> PathBuf {
    config_dir()
}

/// Scripts directory for Lua scripts
pub fn scripts_dir() -> PathBuf {
    config_dir().join("scripts")
}

/// Config file path
fn config_file_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Vantage Process configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Default project directory for Claude agent
    #[serde(default)]
    pub default_project_dir: Option<String>,

    /// Default port for vp
    #[serde(default = "default_port")]
    pub default_port: u16,

    /// Claude CLIのフルパス（mise/asdf等のGUI非対応環境用）
    /// 例: "/Users/user/.local/share/mise/installs/node/22.21.1/bin/claude"
    #[serde(default)]
    pub claude_cli_path: Option<String>,

    /// Projects configuration
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,
}

fn default_port() -> u16 {
    33000
}

/// Project-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Project name (for display)
    pub name: String,
    /// Project directory path
    pub path: String,
    /// Preferred port for this project (optional)
    pub port: Option<u16>,
}

impl Config {
    /// Load config from XDG config file
    pub fn load() -> Result<Self> {
        let path = config_file_path();

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Save config to XDG config file
    pub fn save(&self) -> Result<()> {
        let path = config_file_path();

        // Create config directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Get config file path (for display)
    pub fn config_path() -> PathBuf {
        config_file_path()
    }

    /// Resolve project directory from various sources
    /// Priority: CLI flag > cwd > config default
    /// 相対パスは絶対パスに変換される
    pub fn resolve_project_dir(cli_project_dir: Option<&str>, config: &Config) -> String {
        let path = if let Some(dir) = cli_project_dir {
            // 1. CLI flag (--project-dir)
            std::path::PathBuf::from(dir)
        } else if let Ok(cwd) = std::env::current_dir() {
            // 2. Current working directory
            cwd
        } else if let Some(ref dir) = config.default_project_dir {
            // 3. Config default（最終フォールバック）
            std::path::PathBuf::from(dir)
        } else {
            // 4. どれも使えない場合は "."
            std::path::PathBuf::from(".")
        };

        // 相対パスを絶対パスに変換
        Self::normalize_path(&path)
    }

    /// 指定パスに一致するプロジェクトの 0-based インデックスを返す
    ///
    /// CWD や --project-dir で解決されたパスが config 内のどのプロジェクトに
    /// 対応するかを検索し、ポート割り当てに使用する。
    pub fn find_project_index(&self, resolved_dir: &str) -> Option<usize> {
        self.projects.iter().position(|p| {
            let normalized = Self::normalize_path(std::path::Path::new(&p.path));
            normalized == resolved_dir
        })
    }

    /// パスを正規化（相対パス→絶対パス変換）
    pub fn normalize_path(path: &std::path::Path) -> String {
        if path.is_absolute() {
            // 絶対パスはそのまま正規化を試みる
            path.canonicalize()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string())
        } else {
            // 相対パスをcwdからの絶対パスに変換
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(path))
                .and_then(|p| p.canonicalize().ok())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| path.display().to_string())
        }
    }
}

// =============================================================================
// Running Processes Management — 廃止済み
// =============================================================================
// running.json ベースの状態管理は discovery.rs に移行済み。
// TheWorld (ProcessManagerCapability) のインメモリ状態が単一の真実源。
// 参照: crate::discovery

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_from_toml() {
        // serde default uses default_port() function
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.default_port, 33000);
        assert!(config.default_project_dir.is_none());
        assert!(config.projects.is_empty());
    }

    #[test]
    fn test_config_serialization() {
        let config = Config {
            default_project_dir: Some("/home/user/projects/main".to_string()),
            default_port: 33001,
            claude_cli_path: None,
            projects: vec![ProjectConfig {
                name: "vantage-point".to_string(),
                path: "/Users/makoto/repos/vantage-point".to_string(),
                port: Some(33000),
            }],
        };

        let toml = toml::to_string_pretty(&config).unwrap();
        println!("{}", toml);

        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.default_port, 33001);
        assert_eq!(parsed.projects.len(), 1);
    }
}
