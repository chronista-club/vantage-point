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
// Running Processes Management
// =============================================================================

/// プロセスが生存しているか確認（kill -0）
fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Running Process information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningProcessInfo {
    /// Port number
    pub port: u16,
    /// QUIC (Unison) ポート番号（HTTP port + 100）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic_port: Option<u16>,
    /// Project directory (canonical path)
    pub project_dir: String,
    /// Process ID
    pub pid: u32,
    /// Started timestamp (Unix epoch seconds)
    pub started_at: u64,
    /// Terminal チャネル認証トークン（起動時にランダム生成）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
}

/// Running Processes registry
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunningProcesses {
    #[serde(alias = "stands")]
    pub processes: Vec<RunningProcessInfo>,
}

impl RunningProcesses {
    /// Load running processes from file
    ///
    /// 読み込み時に PID liveness チェックを行い、死んだプロセスを自動除去する。
    pub fn load() -> Result<Self> {
        let path = Self::file_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)?;
        let mut procs: RunningProcesses = serde_json::from_str(&content)?;

        // ゴースト除去: 死んだプロセスをフィルタ
        let before = procs.processes.len();
        procs.processes.retain(|p| is_process_alive(p.pid));
        let removed = before - procs.processes.len();

        if removed > 0 {
            tracing::debug!("Removed {} dead process(es) from running.json", removed);
            // 除去結果を書き戻し
            let _ = procs.save();
        }

        Ok(procs)
    }

    /// Save running processes to file
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

    /// Terminal チャネル用の認証トークンを生成（UUID v4）
    pub fn generate_terminal_token() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Register a new running Process
    pub fn register(port: u16, project_dir: &str, pid: u32, quic_port: Option<u16>) -> Result<()> {
        let mut procs = Self::load().unwrap_or_default();

        // パス正規化を Config::normalize_path() に統一
        let canonical_dir = Config::normalize_path(std::path::Path::new(project_dir));

        // Remove any existing entry for this port or project
        procs
            .processes
            .retain(|s| s.port != port && s.project_dir != canonical_dir);

        // Add new entry
        procs.processes.push(RunningProcessInfo {
            port,
            quic_port,
            project_dir: canonical_dir,
            pid,
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            terminal_token: None,
        });

        procs.save()
    }

    /// 既存エントリの pid, quic_port, terminal_token を更新する
    ///
    /// start.rs で仮登録した後、server.rs でサーバー起動後に正確な値で更新する。
    pub fn update_pid_and_quic(
        port: u16,
        pid: u32,
        quic_port: u16,
        terminal_token: &str,
    ) -> Result<()> {
        let mut procs = Self::load().unwrap_or_default();

        if let Some(entry) = procs.processes.iter_mut().find(|p| p.port == port) {
            entry.pid = pid;
            entry.quic_port = Some(quic_port);
            entry.terminal_token = Some(terminal_token.to_string());
            procs.save()
        } else {
            // 仮登録が見つからない場合はフル登録にフォールバック
            tracing::warn!(
                "Pre-registered entry not found for port={}, performing full registration",
                port
            );
            drop(procs);
            // project_dir が不明なので呼び出し元で対処すべきだが、
            // ここではエラーを返す
            Err(anyhow::anyhow!(
                "No pre-registered entry found for port {}",
                port
            ))
        }
    }

    /// Unregister a Process by port
    pub fn unregister_by_port(port: u16) -> Result<()> {
        let mut procs = Self::load().unwrap_or_default();
        procs.processes.retain(|s| s.port != port);
        procs.save()
    }

    /// Unregister a Process by project directory
    pub fn unregister_by_project(project_dir: &str) -> Result<()> {
        let mut procs = Self::load().unwrap_or_default();

        let canonical_dir = Config::normalize_path(std::path::Path::new(project_dir));

        procs.processes.retain(|s| s.project_dir != canonical_dir);
        procs.save()
    }

    /// Find a running Process by project directory
    ///
    /// プロセス生存確認済みのエントリのみ返す。
    pub fn find_by_project(project_dir: &str) -> Option<RunningProcessInfo> {
        let procs = Self::load().ok()?;

        let canonical_dir = Config::normalize_path(std::path::Path::new(project_dir));

        procs
            .processes
            .into_iter()
            .find(|s| s.project_dir == canonical_dir && is_process_alive(s.pid))
    }

    /// Find a running Process for the current working directory
    /// Returns the Process that best matches the current directory
    pub fn find_for_cwd() -> Option<RunningProcessInfo> {
        let cwd = std::env::current_dir().ok()?;
        let cwd_str = Config::normalize_path(&cwd);

        let procs = Self::load().ok()?;

        // プロセス生存確認フィルタ
        let alive_processes: Vec<_> = procs
            .processes
            .into_iter()
            .filter(|s| is_process_alive(s.pid))
            .collect();

        // First try exact match
        if let Some(found) = alive_processes.iter().find(|s| s.project_dir == cwd_str) {
            return Some(found.clone());
        }

        // Then try to find a Process whose project is an ancestor of cwd
        alive_processes
            .into_iter()
            .filter(|s| cwd_str.starts_with(&s.project_dir))
            .max_by_key(|s| s.project_dir.len()) // Most specific match
    }

    /// Get all running Processes（プロセス生存確認済み）
    pub fn list() -> Vec<RunningProcessInfo> {
        Self::load()
            .map(|s| {
                s.processes
                    .into_iter()
                    .filter(|s| is_process_alive(s.pid))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clean up stale entries (processes that are no longer running)
    pub fn cleanup_stale() -> Result<()> {
        let mut procs = Self::load().unwrap_or_default();
        let original_count = procs.processes.len();

        procs.processes.retain(|s| {
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

        if procs.processes.len() != original_count {
            procs.save()?;
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
