//! Configuration management using XDG Base Directory specification
//!
//! Config file location: $XDG_CONFIG_HOME/vantage/config.toml
//! Default: ~/.config/vantage/config.toml

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// XDG config directory for vantage
fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vantage")
}

/// XDG data directory for vantage (for runtime data)
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vantage")
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
    /// Priority: CLI flag > env var > config default > cwd
    pub fn resolve_project_dir(cli_project_dir: Option<&str>, config: &Config) -> String {
        // 1. CLI flag
        if let Some(dir) = cli_project_dir {
            return dir.to_string();
        }

        // 2. Environment variable
        if let Ok(dir) = std::env::var("VANTAGE_PROJECT_DIR") {
            return dir;
        }

        // 3. Config default
        if let Some(ref dir) = config.default_project_dir {
            return dir.clone();
        }

        // 4. Current working directory
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
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
