//! Configuration management
//!
//! VP の config / data の置き場所を OS 判定込みで一本化する (VP-192)。
//!
//! ## パス方針 (council + dogfood 調査 2026-05-16)
//!
//! ディレクトリ名は全 OS で `vp` に統一。 OS 判定は `dirs` クレートに委ねる。
//!
//! | 種別 | API | macOS | Linux | Windows |
//! |------|-----|-------|-------|---------|
//! | config | `vp_config_dir()` | `~/Library/Application Support/vp/` | `~/.config/vp/` | `%APPDATA%\vp\` |
//! | data   | `vp_data_dir()`   | `~/Library/Application Support/vp/` | `~/.local/share/vp/` | `%LOCALAPPDATA%\vp\` |
//!
//! DB / DISC / セッション状態 / ログ等の生成データは `vp_data_dir()` 配下に置く。
//! Windows の `%APPDATA%` は roaming で同期対象になり DB 破損リスクがあるため、
//! data は `%LOCALAPPDATA%` (= `dirs::data_local_dir()`) を使う。
//!
//! 旧パス (`~/.config/vp/` / `dirs::config_dir()/vantage/`) からの移行は
//! [`migrate_legacy_paths`] が起動時に 1 回だけ冪等に行う。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// VP の config ディレクトリ (OS 別)。
///
/// `dirs::config_dir()` に OS 判定を委ね、 末尾に `vp` を付ける。
/// macOS: `~/Library/Application Support/vp/`、 Linux: `~/.config/vp/`、
/// Windows: `%APPDATA%\vp\`。 `dirs` が None を返す環境 (sandbox 等) では
/// `$HOME/.config` を fallback に使う。
pub fn vp_config_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vp")
}

/// VP の data ディレクトリ (OS 別)。
///
/// `dirs::data_local_dir()` に OS 判定を委ね、 末尾に `vp` を付ける。
/// macOS: `~/Library/Application Support/vp/`、 Linux: `~/.local/share/vp/`、
/// Windows: `%LOCALAPPDATA%\vp\`。 `dirs` が None を返す環境では
/// `$HOME/.local/share` を fallback に使う。
///
/// DB / DISC / ログ等の生成データはこちらに置く (config と分離)。
pub fn vp_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vp")
}

/// Config directory for vp。
///
/// VP-192: 実体は [`vp_config_dir`] に委譲 (caller は無改修)。
pub fn config_dir() -> PathBuf {
    vp_config_dir()
}

/// Data directory for vp。
///
/// VP-192: config とは別ディレクトリ (`vp_data_dir()`) を返すよう変更。
pub fn data_dir() -> PathBuf {
    vp_data_dir()
}

/// Scripts directory for Lua scripts
pub fn scripts_dir() -> PathBuf {
    config_dir().join("scripts")
}

/// 旧 config/data パスから新パスへの冪等なデータ移行 (VP-192)。
///
/// VP は過去 config/data の置き場所が複数 (`~/.config/vp/` 直書き、
/// `dirs::config_dir()/vantage/`) に分裂していた。 OS 判定を `dirs` に委ねる形へ
/// 一本化したため、 旧パスのデータが孤立しないよう起動時に 1 回だけコピーする。
///
/// 設計:
/// - **コピー (move ではない)**。 旧データは残す = ロールバック安全。 旧データ削除は
///   別 issue (VP-193) の担当。
/// - **冪等**。 新パスに既にデータがあれば skip。 何度呼んでも安全。
/// - 失敗しても起動を阻害しない (warn ログのみ)。
///
/// main 初期化の早い段階で 1 回呼ぶこと。
pub fn migrate_legacy_paths() {
    // config: 旧 `~/.config/vp/` → 新 `vp_config_dir()`
    if let Some(home) = dirs::home_dir() {
        let legacy_config = home.join(".config").join("vp");
        migrate_dir_if_needed(&legacy_config, &vp_config_dir(), "config");
    }

    // data: 旧 `dirs::config_dir()/vantage/` → 新 `vp_data_dir()`
    if let Some(cfg) = dirs::config_dir() {
        let legacy_data = cfg.join("vantage");
        migrate_dir_if_needed(&legacy_data, &vp_data_dir(), "data");
    }
}

/// `legacy` ディレクトリの中身を `target` にコピーする (冪等ヘルパー)。
///
/// - `target` が存在して空でなければ skip (= 移行済み)。
/// - `legacy` が存在しない、 または `legacy == target` (同一パス) なら skip。
/// - コピーは再帰的。 失敗は warn ログのみで握り潰す (起動を止めない)。
fn migrate_dir_if_needed(legacy: &std::path::Path, target: &std::path::Path, label: &str) {
    // 旧パスと新パスが同一 (= 既に正規パス) なら何もしない。
    if legacy == target {
        return;
    }
    // 旧データが無ければ移行不要。
    if !legacy.is_dir() {
        return;
    }
    // 新パスに既にデータがあれば移行済みとみなす (冪等)。
    if dir_has_entries(target) {
        return;
    }
    tracing::info!(
        "VP-192 path migration ({}): {} → {}",
        label,
        legacy.display(),
        target.display()
    );
    if let Err(e) = copy_dir_recursive(legacy, target) {
        tracing::warn!(
            "VP-192 path migration ({}) 失敗 ({} → {}): {} — 旧パスのまま継続",
            label,
            legacy.display(),
            target.display(),
            e
        );
    }
}

/// ディレクトリが存在し、 かつ 1 つ以上のエントリを持つか。
fn dir_has_entries(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// `src` の中身を `dst` に再帰コピーする。 `dst` は無ければ作成。
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
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
    ///
    /// VP-188: SSOT は `~/.config/vp/projects.kdl`。 `Config::load()` が projects.kdl を
    /// 読んで本 field を populate する。 `skip_serializing` で `Config::save()` (config.toml)
    /// には書き出さない (= 二重 SSOT 防止)。 `default` は legacy config.toml の
    /// `[[projects]]` seed 読み込み互換のため維持。 永続化は `persist_projects_kdl()`。
    #[serde(default, skip_serializing)]
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
        // projects.kdl が無ければ config.toml の [[projects]] (legacy seed) をそのまま使う。
        if crate::projects_file::projects_file_path().exists() {
            let projects_file = crate::projects_file::ProjectsFile::load()
                .map_err(|e| anyhow::anyhow!("projects.kdl 読み込み失敗: {}", e))?;
            config.projects = projects_file
                .projects
                .iter()
                .map(|e| ProjectConfig {
                    name: e.name.clone(),
                    path: e.path.clone(),
                    // port は port_layout が slot から deterministic に計算する。
                    // enabled / slot は projects.kdl の値 (= projects.kdl が SSOT)。
                    port: None,
                    enabled: e.is_enabled(),
                    slot: e.slot,
                })
                .collect();
        }

        Ok(config)
    }

    /// `config.projects` を projects.kdl に書き出す (VP-188)。
    ///
    /// VP-165 の slot 永続化 (= `resolve::sp_port_for_project` の `ensure_slot`)
    /// 等、 `config.projects` を mutate した後に呼ぶ。 projects の SSOT は
    /// projects.kdl なので、 `Config::save()` (config.toml) ではなく本 helper を使う。
    pub fn persist_projects_kdl(&self) -> Result<()> {
        let pf = crate::projects_file::ProjectsFile {
            projects: self
                .projects
                .iter()
                .map(|p| crate::projects_file::ProjectEntry {
                    name: p.name.clone(),
                    path: p.path.clone(),
                    enabled: if p.enabled { None } else { Some(false) },
                    slot: p.slot,
                })
                .collect(),
        };
        pf.save()
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
    fn test_vp_config_dir_ends_with_vp() {
        // VP-192: config dir は OS によらず末尾が `vp`
        let dir = vp_config_dir();
        assert!(
            dir.ends_with("vp"),
            "vp_config_dir は 'vp' で終わるべき: {}",
            dir.display()
        );
    }

    #[test]
    fn test_vp_data_dir_ends_with_vp() {
        // VP-192: data dir も末尾が `vp`
        let dir = vp_data_dir();
        assert!(
            dir.ends_with("vp"),
            "vp_data_dir は 'vp' で終わるべき: {}",
            dir.display()
        );
    }

    #[test]
    fn test_config_dir_delegates_to_vp_config_dir() {
        // VP-192: config_dir() は vp_config_dir() に委譲
        assert_eq!(config_dir(), vp_config_dir());
    }

    #[test]
    fn test_data_dir_delegates_to_vp_data_dir() {
        // VP-192: data_dir() は vp_data_dir() に委譲
        assert_eq!(data_dir(), vp_data_dir());
    }

    #[test]
    fn test_copy_dir_recursive_copies_nested() {
        // 再帰コピーが nested file/dir を保持する
        let tmp = std::env::temp_dir().join(format!("vp192_copy_{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), "hello").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), "world").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
            "world"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_idempotent() {
        // VP-192: migration は冪等 — 新パスに既存データがあれば旧データで上書きしない
        let tmp = std::env::temp_dir().join(format!("vp192_mig_{}", std::process::id()));
        let legacy = tmp.join("legacy");
        let target = tmp.join("target");
        let _ = std::fs::remove_dir_all(&tmp);

        // 旧パスに古いデータ、新パスに既存データを置く
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("data.txt"), "OLD").unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("data.txt"), "NEW").unwrap();

        // 1 回目: 新パスに中身があるので skip されるはず
        migrate_dir_if_needed(&legacy, &target, "test");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "NEW",
            "既存データがあれば移行 skip (冪等)"
        );

        // 2 回目: 何度呼んでも変わらない
        migrate_dir_if_needed(&legacy, &target, "test");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "NEW"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_copies_to_empty_target() {
        // VP-192: 新パスが空 (or 不在) なら旧データをコピーし、旧データは残す
        let tmp = std::env::temp_dir().join(format!("vp192_migc_{}", std::process::id()));
        let legacy = tmp.join("legacy");
        let target = tmp.join("target");
        let _ = std::fs::remove_dir_all(&tmp);

        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("data.txt"), "OLD").unwrap();
        // target は不在

        migrate_dir_if_needed(&legacy, &target, "test");

        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "OLD",
            "新パスへコピーされる"
        );
        assert!(
            legacy.join("data.txt").exists(),
            "旧データは残る (move ではなく copy)"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_same_path_noop() {
        // legacy == target なら何もしない
        let tmp = std::env::temp_dir().join(format!("vp192_migs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("data.txt"), "X").unwrap();
        // パニックしないこと、データが壊れないことを確認
        migrate_dir_if_needed(&tmp, &tmp, "test");
        assert_eq!(std::fs::read_to_string(tmp.join("data.txt")).unwrap(), "X");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_missing_legacy_noop() {
        // 旧パスが存在しなければ何もしない
        let tmp = std::env::temp_dir().join(format!("vp192_migm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let legacy = tmp.join("nonexistent");
        let target = tmp.join("target");
        migrate_dir_if_needed(&legacy, &target, "test");
        assert!(!target.exists(), "旧データ不在なら新パスは作られない");
    }

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
        // VP-188: projects は #[serde(skip_serializing)] で config.toml に書き出されない
        // (= SSOT は projects.kdl)。 serialize → parse 後は空になるのが正しい。
        assert!(
            parsed.projects.is_empty(),
            "projects は config.toml に serialize されないはず (VP-188)"
        );
        assert!(
            !toml.contains("[[projects]]"),
            "config.toml に [[projects]] が出てはいけない (VP-188)"
        );
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
