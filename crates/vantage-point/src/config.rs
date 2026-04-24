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

    /// Port layout overrides (optional、default は PortLayout::default())
    /// VP Port Management Phase 1: config で layout 定数を変更可能に
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortLayoutOverrides>,
}

/// PortLayout の config 上書き (全 field optional、未指定は default)
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PortLayoutOverrides {
    pub world_port: Option<u16>,
    pub project_slot_base: Option<u16>,
    pub project_slot_size: Option<u16>,
    pub max_projects: Option<u16>,
    pub lane_base_offset: Option<u16>,
    pub lane_size: Option<u16>,
    #[serde(default)]
    pub roles: Option<std::collections::BTreeMap<String, u16>>,
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
    /// SP 自動起動の有効/無効（デフォルト: true）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Port slot (VP Port Management Phase 1, deterministic layout 用)
    /// 永続 assign: 一度割り当てたら project の port は常にこの slot から計算
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u16>,
}

fn default_enabled() -> bool {
    true
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

    // =========================================================================
    // VP Port Management — Phase 1 (memory mem_1CaKCbNE24KTQDuf9x4Eim)
    // =========================================================================

    /// 実効 PortLayout (default + config overrides)
    pub fn port_layout(&self) -> crate::port_layout::PortLayout {
        let mut layout = crate::port_layout::PortLayout::default();
        if let Some(ov) = &self.ports {
            if let Some(v) = ov.world_port {
                layout.world_port = v;
            }
            if let Some(v) = ov.project_slot_base {
                layout.project_slot_base = v;
            }
            if let Some(v) = ov.project_slot_size {
                layout.project_slot_size = v;
            }
            if let Some(v) = ov.max_projects {
                layout.max_projects = v;
            }
            if let Some(v) = ov.lane_base_offset {
                layout.lane_base_offset = v;
            }
            if let Some(v) = ov.lane_size {
                layout.lane_size = v;
            }
            if let Some(r) = &ov.roles {
                layout.roles = r.clone();
            }
        }
        layout
    }

    /// project 名 → slot index (未割当 / 未登録なら None)
    pub fn resolve_slot_by_name(&self, name: &str) -> Option<u16> {
        self.projects
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.slot)
    }

    /// slot index → project (割当済みの場合)
    pub fn project_by_slot(&self, slot: u16) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.slot == Some(slot))
    }

    /// 使用中 slot 集合
    pub fn used_slots(&self) -> std::collections::BTreeSet<u16> {
        self.projects.iter().filter_map(|p| p.slot).collect()
    }

    /// 次の空き slot を返す (0..max_projects 内で未使用のうち最小)
    pub fn next_free_slot(&self) -> Option<u16> {
        let layout = self.port_layout();
        let used = self.used_slots();
        (0..layout.max_projects).find(|s| !used.contains(s))
    }

    /// project に slot を assign (未割当の場合のみ)。指定 slot の衝突は Err。
    /// 戻り値: 割当られた slot
    pub fn ensure_slot(&mut self, project_name: &str, preferred: Option<u16>) -> Result<u16> {
        // 既に割当済み: そのまま返す
        if let Some(s) = self.resolve_slot_by_name(project_name) {
            return Ok(s);
        }

        let layout = self.port_layout();
        let slot = match preferred {
            Some(s) => {
                if s >= layout.max_projects {
                    anyhow::bail!("slot {} exceeds max_projects ({})", s, layout.max_projects);
                }
                if let Some(existing) = self.project_by_slot(s) {
                    anyhow::bail!("slot {} already assigned to project '{}'", s, existing.name);
                }
                s
            }
            None => self.next_free_slot().ok_or_else(|| {
                anyhow::anyhow!(
                    "no free slot available (max_projects={})",
                    layout.max_projects
                )
            })?,
        };

        // 該当 project を探して slot field を更新 (存在しない場合は登録なしとして Err)
        let entry = self.projects.iter_mut().find(|p| p.name == project_name);
        match entry {
            Some(p) => {
                p.slot = Some(slot);
                Ok(slot)
            }
            None => anyhow::bail!("project '{}' not registered in config", project_name),
        }
    }

    /// project の slot 割当解除
    pub fn unassign_slot(&mut self, project_name: &str) -> Result<()> {
        let entry = self
            .projects
            .iter_mut()
            .find(|p| p.name == project_name)
            .ok_or_else(|| anyhow::anyhow!("project '{}' not found", project_name))?;
        entry.slot = None;
        Ok(())
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
                path: "/path/to/vantage-point".to_string(),
                port: Some(33000),
                enabled: true,
                slot: Some(0),
            }],
            ports: None,
        };

        let toml = toml::to_string_pretty(&config).unwrap();
        println!("{}", toml);

        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.default_port, 33001);
        assert_eq!(parsed.projects.len(), 1);
    }
}
