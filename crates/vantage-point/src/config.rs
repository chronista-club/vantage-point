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

    /// Lane 作成時の default Stand 名 (例: "echoes" / "shell" / "tmux")。
    ///
    /// `mise run vp:stand:{name}` の `name` 部分を指定。 None なら "echoes" fallback
    /// (`Config::default_stand_or_echoes()` 経由)。
    ///
    /// doc 11 §3 (Stand init_script system / mise task 路線)、 PR-B 対応。
    /// PR-pre2 (VP-118): "hd" → "echoes" rename (Stand metaphor + identifier sweep)。
    #[serde(default)]
    pub default_stand: Option<String>,

    /// Projects configuration
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,

    /// Port layout overrides (optional、default は PortLayout::default())
    /// VP Port Management Phase 1: config で layout 定数を変更可能に
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortLayoutOverrides>,

    /// SP startup behavior — Worker spawn の concurrency 制限等 (I-b、 2026-04-30)
    #[serde(default)]
    pub startup: StartupConfig,

    /// VP-154 PR-3.5: LAN networking config (= mDNS / hub federation の挙動を tweak)
    #[serde(default)]
    pub network: NetworkConfig,
}

/// VP-154 PR-3.5: LAN networking config — mDNS advertise の identity 安定化が主目的。
///
/// macOS LocalHostName が boot 時に collision 検出で auto-increment (`mito-mac` →
/// `mito-mac-3`) しても、 VP の LAN 識別子 (= `world-mito-mac` 等の mDNS instance_name)
/// を **config で固定** することで、 LAN 上の他 device から見た VP identity が不変になる。
///
/// SRV record の target hostname (= 接続解決のための A record 参照) は OS 現在値を使い続けるので、
/// 接続自体は OS rename にも追従する。 これで `instance_name 安定 + 接続動的` の両立。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkConfig {
    /// mDNS advertise の instance_name に使う hostname を強制指定 (例: `"mito-mac"`)。
    ///
    /// `Some(name)` なら `world-{name}` / `sp-{project}-{name}` で advertise、
    /// `None` (default) なら旧挙動 (= `scutil --get LocalHostName` から取得)。
    ///
    /// 用途: macOS LocalHostName auto-increment (= boot 時 collision 検出由来) で
    /// LAN identity が揺れる問題を回避。 同 instance_name の再 advertise は mDNS protocol の
    /// TTL refresh として処理されるので、 cache 上の entry は merge されて累積しない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise_hostname: Option<String>,
}

/// SP startup behavior config (I-b、 2026-04-30)。
///
/// Mailbox actor (`lane-spawn@<project>`) で Worker spawn を Cmd 化した上で、
/// 内部 Semaphore で同時実行数を gate する。 `max_concurrent_lane_spawn` で
/// 制限値を tweak、 default は **1** (= 完全 sequential、 dogfood の視覚 pop 体験 +
/// Claude CLI rate-limit 安全)。 計測 log (`Lane spawn completed: ... elapsed=`) を
/// dogfood で集計して N 値を実証的に上げる方針。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupConfig {
    /// 同時 Lane spawn 数の上限 (= Mailbox actor 内部 `Semaphore::new(N)`)。
    /// default 1 = sequential。
    #[serde(default = "default_max_concurrent_lane_spawn")]
    pub max_concurrent_lane_spawn: u32,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            max_concurrent_lane_spawn: default_max_concurrent_lane_spawn(),
        }
    }
}

fn default_max_concurrent_lane_spawn() -> u32 {
    1
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
    ///
    /// VP-188: registered projects の SSOT は `~/.config/vp/projects.kdl` に移行。
    /// config.toml をパースした後、 projects.kdl が存在すれば `projects` field を
    /// **projects.kdl の内容で上書き** する。 これで `config.projects` を読む全
    /// caller (resolve / TUI / ccws / reload_config) が無改修で projects.kdl を
    /// SSOT として参照できる。 projects.kdl が無ければ config.toml の `[[projects]]`
    /// (= legacy seed) をそのまま使う。
    pub fn load() -> Result<Self> {
        let path = config_file_path();

        let mut config: Config = if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            toml::from_str(&content)?
        } else {
            Self::default()
        };

        // VP-188: projects.kdl が SSOT。 存在すれば config.projects を置換。
        let projects_file = crate::projects_file::ProjectsFile::load()
            .map_err(|e| anyhow::anyhow!("projects.kdl 読み込み失敗: {}", e))?;
        if crate::projects_file::projects_file_path().exists() {
            config.projects = projects_file
                .projects
                .iter()
                .map(|e| ProjectConfig {
                    name: e.name.clone(),
                    path: e.path.clone(),
                    // port / slot は projects.kdl 管轄外 (= port_layout が slug から
                    // deterministic に計算する)。 enabled は projects.kdl の値。
                    port: None,
                    enabled: e.is_enabled(),
                    slot: None,
                })
                .collect();
        }

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

    /// Default Stand 名 (config 未指定なら "echoes" fallback)。
    ///
    /// `mise run vp:stand:{name}` の `name` 部分。 lane 作成時 (sidebar UI / HTTP API /
    /// LanePool::with_lead 等) で stand 指定が無い場合の選択値。
    ///
    /// PR-pre2 (VP-118): rename `default_stand_or_hd` → `default_stand_or_echoes`、
    /// fallback "hd" → "echoes" (HD → Echoes rename の一環)。
    pub fn default_stand_or_echoes(&self) -> &str {
        self.default_stand.as_deref().unwrap_or("echoes")
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

    /// VP-165 (doc 17 決定C): project の SP port を解決する（flat stable slot 方式）。
    ///
    /// 優先度: `port` 明示 override → 無ければ `ensure_slot`（未割当なら次の空き slot を割当、
    /// `self` を mutate）→ `PORT_RANGE_START + slot`。slot は config 永続なので、project リスト
    /// 変更でも既存 project の port は不変（旧 `port_for_configured` の `PORT_RANGE_START + index`
    /// 位置依存とは違う）。
    ///
    /// 注: 新規 slot 割当時に `self` が mutate されるので、caller は `save()` で永続化すること
    /// （[`crate::resolve::sp_port_for_project`] が load → 本 method → save をまとめている）。
    /// project が config に未登録なら `Err`（caller 側で `find_available_port` 等に fallback）。
    pub fn resolve_sp_port(&mut self, name: &str) -> Result<u16> {
        if let Some(p) = self
            .projects
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.port)
        {
            return Ok(p);
        }
        let slot = self.ensure_slot(name, None)?;
        Ok(crate::cli::PORT_RANGE_START + slot)
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
            default_stand: None,
            projects: vec![ProjectConfig {
                name: "vantage-point".to_string(),
                path: "/path/to/vantage-point".to_string(),
                port: Some(33000),
                enabled: true,
                slot: Some(0),
            }],
            ports: None,
            startup: StartupConfig::default(),
            network: NetworkConfig::default(),
        };

        let toml = toml::to_string_pretty(&config).unwrap();
        println!("{}", toml);

        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.default_port, 33001);
        assert_eq!(parsed.projects.len(), 1);
    }

    #[test]
    fn test_network_config_default_is_empty() {
        // VP-154 PR-3.5: default config に network section 不在でも問題なく load
        let config: Config = toml::from_str("").unwrap();
        assert!(config.network.advertise_hostname.is_none());
    }

    #[test]
    fn test_network_config_advertise_hostname_loads() {
        // VP-154 PR-3.5: `[network] advertise_hostname = "mito-mac"` が toml から正しく読める
        let raw = r#"
[network]
advertise_hostname = "mito-mac"
"#;
        let config: Config = toml::from_str(raw).unwrap();
        assert_eq!(
            config.network.advertise_hostname.as_deref(),
            Some("mito-mac")
        );
    }

    #[test]
    fn test_network_config_round_trip() {
        // serialize → parse round-trip で advertise_hostname が保持される
        let mut config = Config::default();
        config.network.advertise_hostname = Some("mito-mac".to_string());
        let raw = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(
            parsed.network.advertise_hostname.as_deref(),
            Some("mito-mac")
        );
    }

    #[test]
    fn resolve_sp_port_override_then_slot_and_position_independent() {
        let mk = |name: &str, port: Option<u16>| ProjectConfig {
            name: name.to_string(),
            path: format!("/repos/{name}"),
            port,
            enabled: true,
            slot: None,
        };
        let mut cfg = Config::default();
        cfg.projects.push(mk("a", Some(33099))); // port 明示 override
        cfg.projects.push(mk("b", None));
        cfg.projects.push(mk("c", None));

        // a: override が最優先、slot は触らない
        assert_eq!(cfg.resolve_sp_port("a").unwrap(), 33099);
        assert_eq!(
            cfg.projects.iter().find(|p| p.name == "a").unwrap().slot,
            None
        );

        // b: 最初の空き slot 0 → port = PORT_RANGE_START
        assert_eq!(
            cfg.resolve_sp_port("b").unwrap(),
            crate::cli::PORT_RANGE_START
        );
        assert_eq!(
            cfg.projects.iter().find(|p| p.name == "b").unwrap().slot,
            Some(0)
        );
        // 再呼び出しは同じ（mutate 済み、idempotent）
        assert_eq!(
            cfg.resolve_sp_port("b").unwrap(),
            crate::cli::PORT_RANGE_START
        );

        // c: 次の空き slot 1 → port = PORT_RANGE_START + 1
        assert_eq!(
            cfg.resolve_sp_port("c").unwrap(),
            crate::cli::PORT_RANGE_START + 1
        );

        // project リストの先頭に新 project を挿入しても b/c の slot は不変（= 位置非依存）
        cfg.projects.insert(0, mk("z", None));
        assert_eq!(
            cfg.projects.iter().find(|p| p.name == "b").unwrap().slot,
            Some(0)
        );
        assert_eq!(
            cfg.projects.iter().find(|p| p.name == "c").unwrap().slot,
            Some(1)
        );
        // z は次の空き slot 2
        assert_eq!(
            cfg.resolve_sp_port("z").unwrap(),
            crate::cli::PORT_RANGE_START + 2
        );

        // 未登録 project → Err
        assert!(cfg.resolve_sp_port("not-registered").is_err());
    }
}
