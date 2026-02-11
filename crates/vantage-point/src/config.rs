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

/// Vantage Stand configuration
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
// Running Stands Management
// =============================================================================

/// Running Stand information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningStandInfo {
    /// Port number
    pub port: u16,
    /// Project directory (canonical path)
    pub project_dir: String,
    /// Process ID
    pub pid: u32,
    /// Started timestamp (Unix epoch seconds)
    pub started_at: u64,
}

/// Running Stands registry
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunningStands {
    pub stands: Vec<RunningStandInfo>,
}

impl RunningStands {
    /// Load running stands from file
    pub fn load() -> Result<Self> {
        let path = Self::file_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)?;
        let stands: RunningStands = serde_json::from_str(&content)?;
        Ok(stands)
    }

    /// Save running stands to file
    pub fn save(&self) -> Result<()> {
        let path = Self::file_path();

        // Create config directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Get the file path for running.json
    pub fn file_path() -> PathBuf {
        config_dir().join("running.json")
    }

    /// Register a new running Stand
    pub fn register(port: u16, project_dir: &str, pid: u32) -> Result<()> {
        let mut stands = Self::load().unwrap_or_default();

        // Canonicalize the project directory for consistent matching
        let canonical_dir = std::fs::canonicalize(project_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| project_dir.to_string());

        // Remove any existing entry for this port or project
        stands
            .stands
            .retain(|s| s.port != port && s.project_dir != canonical_dir);

        // Add new entry
        stands.stands.push(RunningStandInfo {
            port,
            project_dir: canonical_dir,
            pid,
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });

        stands.save()
    }

    /// Unregister a Stand by port
    pub fn unregister_by_port(port: u16) -> Result<()> {
        let mut stands = Self::load().unwrap_or_default();
        stands.stands.retain(|s| s.port != port);
        stands.save()
    }

    /// Unregister a Stand by project directory
    pub fn unregister_by_project(project_dir: &str) -> Result<()> {
        let mut stands = Self::load().unwrap_or_default();

        let canonical_dir = std::fs::canonicalize(project_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| project_dir.to_string());

        stands.stands.retain(|s| s.project_dir != canonical_dir);
        stands.save()
    }

    /// Find a running Stand by project directory
    pub fn find_by_project(project_dir: &str) -> Option<RunningStandInfo> {
        let stands = Self::load().ok()?;

        let canonical_dir = std::fs::canonicalize(project_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| project_dir.to_string());

        stands
            .stands
            .into_iter()
            .find(|s| s.project_dir == canonical_dir)
    }

    /// Find a running Stand for the current working directory
    /// Returns the Stand that best matches the current directory
    pub fn find_for_cwd() -> Option<RunningStandInfo> {
        let cwd = std::env::current_dir().ok()?;
        let cwd_str = std::fs::canonicalize(&cwd)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| cwd.display().to_string());

        let stands = Self::load().ok()?;

        // First try exact match
        if let Some(stand) = stands.stands.iter().find(|s| s.project_dir == cwd_str) {
            return Some(stand.clone());
        }

        // Then try to find a Stand whose project is an ancestor of cwd
        stands
            .stands
            .into_iter()
            .filter(|s| cwd_str.starts_with(&s.project_dir))
            .max_by_key(|s| s.project_dir.len()) // Most specific match
    }

    /// Get all running Stands
    pub fn list() -> Vec<RunningStandInfo> {
        Self::load().map(|s| s.stands).unwrap_or_default()
    }

    /// Clean up stale entries (processes that are no longer running)
    pub fn cleanup_stale() -> Result<()> {
        let mut stands = Self::load().unwrap_or_default();
        let original_count = stands.stands.len();

        stands.stands.retain(|s| {
            // Check if process is still running
            // On Unix, we can use kill with signal 0 to check
            #[cfg(unix)]
            {
                use std::process::Command;
                Command::new("kill")
                    .args(["-0", &s.pid.to_string()])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                // On Windows, we'd need different logic
                true
            }
        });

        if stands.stands.len() != original_count {
            stands.save()?;
        }

        Ok(())
    }
}

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
