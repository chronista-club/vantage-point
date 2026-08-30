//! Configuration management
//!
//! ## VP-189: config.toml → config.kdl 統一
//!
//! VP の設定ファイルは元々 TOML だったが、 repos.kdl (VP-188) / lane の
//! sub-files.kdl 等、 周辺の設定は既に KDL に揃っていた。 config 本体だけ
//! TOML で取り残されていたのを KDL に統一し、 club-kdl 資産を一本化する。
//!
//! - config.kdl が受け持つのは **環境層だけ**（= このマシンの事実。claude-cli-path /
//!   hub-addr。doc 59 の 3 層モデル）。人間が編集する read-only で VP 自身は書き戻さない
//!   (= `KdlSerialize` 不要、 `KdlDeserialize` のみ)。
//! - **user の「好み」（既定 agent × model / theme / アイドル時間 / ログ詳細度）は
//!   settings.kdl** — daemon が所有して書く（doc 59 §3）。config.kdl とはキーを重複させない。
//! - registered repos は repos.kdl が SSOT (VP-188)。 config.kdl には出さない。
//! - kebab-case のキー名 (`default-port` 等) を採用。
//!
//! ## persistence restructure: XDG Base Directory 準拠 (全 OS 統一)
//!
//! 旧 VP-192 では `dirs` クレートで OS 別判定し、 macOS は `~/Library/Application
//! Support/vp/`、 Linux は `~/.config/vp/`、 Windows は `%APPDATA%\vp\` と分裂していた。
//! user 指示 = 「global は XDG (= 出来るだけ minimum)、 proj 関連は `.vp/` 活用」 で
//! XDG Base Directory Specification 準拠の 3 zone に統一する。 macOS の Application
//! Support / Library/Logs path は撤去 (= dotfile への露出移管)。
//!
//! | zone   | 環境変数                  | default                  | 用途 |
//! |--------|---------------------------|--------------------------|------|
//! | config | `$XDG_CONFIG_HOME`        | `~/.config/vp/`          | 設定 3 層 (doc 59): 環境 = config.kdl (人だけが書く) / 好み = settings.kdl (daemon が書く) / 作業 = repos.kdl (VP が書く) |
//! | data   | `$XDG_DATA_HOME`          | `~/.local/share/vp/`     | 永続 data store (db / discs) |
//! | state  | `$XDG_STATE_HOME`         | `~/.local/state/vp/`     | runtime state + log (session.json / sessions/ / log/) |
//!
//! 旧 path (= Application Support / Library/Logs / `~/.config/vp/` etc.) からの
//! 移行は [`migrate_legacy_paths`] が起動時に 1 回だけ冪等に行う。 廃止 file
//! (running.json / vantage.db / config.toml / lanes/ / scripts/ / state/ 内の
//! port-prefix JSON 等) は同じ pass で delete する。

use anyhow::Result;
use club_kdl::KdlDeserialize;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// === path / zone 解決は vp-paths crate に SSOT 化 (Stage 3) ===
//
// 旧構造: 本 module が zone 解決 + legacy migration を直書きし、
// vp-app/src/paths.rs がそれを手動同期で複製していた (= drift 源)。 軽量
// `vp-paths` crate に一本化し、 vantage-point と vp-app の双方がそれに依存
// する (循環なし、 重量 dep を vp-app に持ち込まない)。 zone 表・migration
// 仕様は `vp_paths` の module doc が canonical。
pub use vp_paths::{
    migrate_legacy_paths, vp_config_dir, vp_data_dir, vp_log_dir, vp_sessions_dir, vp_state_dir,
};

/// Config directory for vp。
///
/// 既存 caller 互換用 wrapper — 実体は [`vp_config_dir`] (= `~/.config/vp/`)。
pub fn config_dir() -> PathBuf {
    vp_config_dir()
}

/// Data directory for vp。
///
/// 既存 caller 互換用 wrapper — 実体は [`vp_data_dir`] (= `~/.local/share/vp/`)。
pub fn data_dir() -> PathBuf {
    vp_data_dir()
}

/// Scripts directory for Lua scripts (= `vp_config_dir()/scripts/`)。
pub fn scripts_dir() -> PathBuf {
    vp_config_dir().join("scripts")
}

/// Config file path (`config_dir()/config.kdl` ── VP-189 で config.toml から移行)
fn config_file_path() -> PathBuf {
    config_dir().join("config.kdl")
}

/// Vantage Process configuration
///
/// config.kdl は document スタイルの KDL。 各 scalar 設定は document 直下の
/// 単一引数 node (`default-port 33000` 等)、 section は子 node (`startup { ... }`)。
#[derive(Debug, Clone, Serialize, Deserialize, Default, KdlDeserialize)]
#[kdl(document)]
pub struct Config {
    /// Default repo directory for Claude agent
    #[serde(default)]
    #[kdl(child, name = "default-repo-dir", unwrap_arg)]
    pub default_repo_dir: Option<String>,

    /// Default port for vp
    ///
    /// 注: KDL derive の field-level `default` は型の `Default` 固定 (u16 → 0)。
    /// config.kdl に `default-port` node が無いと 0 になるため、 `Config::load()`
    /// が 0 を検出して `default_port()` (33000) に補正する。
    #[serde(default = "default_port")]
    #[kdl(child, name = "default-port", unwrap_arg, default)]
    pub default_port: u16,

    /// Claude CLIのフルパス（mise/asdf等のGUI非対応環境用）
    /// 例: "/Users/user/.local/share/mise/installs/node/22.21.1/bin/claude"
    #[serde(default)]
    #[kdl(child, name = "claude-cli-path", unwrap_arg)]
    pub claude_cli_path: Option<String>,

    /// Lane 作成時の default agent 名 (例: "claude" / "shell" / "tmux")。
    ///
    /// `mise run vp:agent:{name}` の `name` 部分を指定。 None なら "claude" fallback
    /// (`Config::default_agent_or_claude()` 経由)。
    ///
    /// doc 11 §3 (Agent init_script system / mise task 路線)、 PR-B 対応。
    /// PR-pre2 (VP-118): "hd" → "claude" rename (Agent metaphor + identifier sweep)。
    #[serde(default)]
    #[kdl(child, name = "default-agent", unwrap_arg)]
    pub default_agent: Option<String>,

    /// sub lane 追加時の既定 claude model alias（`--model` 未指定時に registry へ記録）。
    ///
    /// **未設定なら記録しない = engine 側の user 既定に委ねる**（doc 54 §8-11、mako 2026-07-25
    /// 「Opus のところはユーザ設定に任せる」。旧: Opus を強制 record して claude の user 既定を
    /// 上書きしていた）。mcp / cli / sidebar(GUI) の全 sub 追加経路が共有し、
    /// tui(TUI console) / gui(chat engine) 両方に効く（model の SSOT は registry の
    /// [`crate::lane::session_registry::SessionEntry::model`]。旧 per-lane 1 file store
    /// （`engine_models/`）は 2026-07-27 退役 — [`crate::lane::engine_model`] は語彙検証のみ）。
    /// 例: config.kdl に `default-lane-model "claude-sonnet-5"` で VP 側の既定を固定可。
    #[serde(default)]
    #[kdl(child, name = "default-lane-model", unwrap_arg)]
    pub default_lane_model: Option<String>,

    /// chronista-hub の Unison surface addr（federation opt-in、例: "hub.chronista.club:12879"）。
    ///
    /// 未設定 = federation off（machine-local 動作）。env `CHRONISTA_HUB_ADDR` が設定されて
    /// いればそちらが優先（dev override）— 解決は `daemon::hub_client::hub_addr()` が担う。
    /// launchd (LaunchAgent) 起動の daemon は shell env を持たないため、常設運用は
    /// この config.kdl 側が SSOT（TERM/PATH/LANG と同じ launchd env 問題の構造的回避）。
    #[serde(default)]
    #[kdl(child, name = "hub-addr", unwrap_arg)]
    pub hub_addr: Option<String>,

    /// Repos configuration
    ///
    /// VP-188: SSOT は `~/.config/vp/repos.kdl`。 `Config::load()` が repos.kdl を
    /// 読んで本 field を populate する。 config.kdl には一切出さない (`#[kdl(skip)]`、
    /// = 二重 SSOT 防止)。 永続化は `persist_repos_kdl()`。
    #[serde(default, skip_serializing)]
    #[kdl(skip)]
    pub repos: Vec<RepoConfig>,

    /// Port layout overrides (optional、default は PortLayout::default())
    ///
    /// VP-189: port layout の上書きは advanced 機能で dogfood でも未使用のため、
    /// config.kdl からは設定不可とした (`#[kdl(skip)]`)。 必要になった時点で
    /// 専用機構を足す (= config.kdl は「ミニマムな global 設定」に保つ方針)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[kdl(skip)]
    pub ports: Option<PortLayoutOverrides>,

    /// repo startup behavior — Sub spawn の concurrency 制限等 (I-b、 2026-04-30)
    #[serde(default)]
    #[kdl(child, default)]
    pub startup: StartupConfig,
}

/// repo startup behavior config (I-b、 2026-04-30)。
///
/// [`LaneSpawnActor`](crate::repo::lane_spawn_actor) が Sub spawn を Cmd 化
/// (in-process channel) した上で、 内部 Semaphore で同時実行数を gate する。 `max_concurrent_lane_spawn` で
/// 制限値を tweak、 default は **1** (= 完全 sequential、 dogfood の視覚 pop 体験 +
/// Claude CLI rate-limit 安全)。 計測 log (`Lane spawn completed: ... elapsed=`) を
/// dogfood で集計して N 値を実証的に上げる方針。
#[derive(Debug, Clone, Serialize, Deserialize, KdlDeserialize)]
#[kdl(name = "startup")]
pub struct StartupConfig {
    /// 同時 Lane spawn 数の上限 (= LaneSpawnActor 内部 `Semaphore::new(N)`)。
    /// default 1 = sequential。
    ///
    /// 注: `default-port` と同様、 KDL field-level `default` は u32 → 0 になる。
    /// `Config::load()` が 0 を検出して 1 に補正する。
    #[serde(default = "default_max_concurrent_lane_spawn")]
    #[kdl(child, name = "max-concurrent-lane-spawn", unwrap_arg, default)]
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
    pub daemon_port: Option<u16>,
    pub repo_slot_base: Option<u16>,
    pub repo_slot_size: Option<u16>,
    pub max_repos: Option<u16>,
    pub lane_base_offset: Option<u16>,
    pub lane_size: Option<u16>,
    #[serde(default)]
    pub roles: Option<std::collections::BTreeMap<String, u16>>,
}

fn default_port() -> u16 {
    33000
}

/// Repo-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Repo name (for display)
    pub name: String,
    /// Repo directory path
    pub path: String,
    /// Preferred port for this repo (optional)
    pub port: Option<u16>,
    /// repo 自動起動の有効/無効（デフォルト: true）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Port slot (VP Port Management Phase 1, deterministic layout 用)
    /// 永続 assign: 一度割り当てたら repo の port は常にこの slot から計算
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u16>,
}

fn default_enabled() -> bool {
    true
}

impl Config {
    /// Load config from XDG config file (`~/.config/vp/config.kdl`)
    ///
    /// VP-189: config 形式を KDL に統一。 config.kdl が無い / 空なら `Config::default()`。
    ///
    /// VP-188: registered repos の SSOT は `~/.config/vp/repos.kdl`。
    /// config.kdl をパースした後、 repos.kdl が存在すれば `repos` field を
    /// **repos.kdl の内容で populate** する。 これで `config.repos` を読む全
    /// caller (resolve / TUI / lane / reload_config) が repos.kdl を SSOT として
    /// 参照できる。
    pub fn load() -> Result<Self> {
        let path = config_file_path();

        let mut config: Config = if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            if content.trim().is_empty() {
                Self::default()
            } else {
                club_kdl::from_str(&content)
                    .map_err(|e| anyhow::anyhow!("config.kdl パース失敗: {}", e))?
            }
        } else {
            Self::default()
        };

        config.apply_load_defaults();

        // VP-188: repos.kdl が SSOT。 存在すれば config.repos を populate。
        if crate::repos_file::repos_file_path().exists() {
            let repos_file = crate::repos_file::ReposFile::load()
                .map_err(|e| anyhow::anyhow!("repos.kdl 読み込み失敗: {}", e))?;
            config.repos = repos_file
                .repos
                .iter()
                .map(|e| RepoConfig {
                    name: e.name.clone(),
                    path: e.path.clone(),
                    // port は port_layout が slot から deterministic に計算する。
                    // enabled / slot は repos.kdl の値 (= repos.kdl が SSOT)。
                    port: None,
                    enabled: e.is_enabled(),
                    slot: e.slot,
                })
                .collect();
        }

        Ok(config)
    }

    /// KDL field-level `default` の限界 (型 `Default` 固定 = 0) を補正する。
    ///
    /// club-kdl の `#[kdl(..., default)]` は node 不在時に **型の `Default`** を使う。
    /// `default_port: u16` / `max_concurrent_lane_spawn: u32` は本来 33000 / 1 が
    /// default だが、 KDL parse 直後は 0 になる。 `Config::load()` が KDL parse 後に
    /// 本メソッドで 0 を検出して意味のある値に補正する。
    fn apply_load_defaults(&mut self) {
        if self.default_port == 0 {
            self.default_port = default_port();
        }
        if self.startup.max_concurrent_lane_spawn == 0 {
            self.startup.max_concurrent_lane_spawn = default_max_concurrent_lane_spawn();
        }
    }

    /// `config.repos` を repos.kdl に書き出す (VP-188)。
    ///
    /// VP-165 の slot 永続化 (= `resolve::port_for_repo` の `ensure_slot`)
    /// 等、 `config.repos` を mutate した後に呼ぶ。 repos の SSOT は
    /// repos.kdl なので、 `Config::save()` (config.toml) ではなく本 helper を使う。
    pub fn persist_repos_kdl(&self) -> Result<()> {
        let pf = crate::repos_file::ReposFile {
            repos: self
                .repos
                .iter()
                .map(|p| crate::repos_file::RepoEntry {
                    name: p.name.clone(),
                    path: p.path.clone(),
                    enabled: if p.enabled { None } else { Some(false) },
                    slot: p.slot,
                })
                .collect(),
        };
        pf.save()
    }

    /// Get config file path (for display)
    pub fn config_path() -> PathBuf {
        config_file_path()
    }

    /// Default agent 名 (config 未指定なら "claude" fallback)。
    ///
    /// `mise run vp:agent:{name}` の `name` 部分。 lane 作成時 (sidebar UI / HTTP API /
    /// LanePool::with_root 等) で agent 指定が無い場合の選択値。
    ///
    /// PR-pre2 (VP-118): rename `default_stand_or_hd` → `default_agent_or_claude`、
    /// fallback "hd" → "claude" (HD → Echoes rename の一環)。
    pub fn default_agent_or_claude(&self) -> &str {
        self.default_agent.as_deref().unwrap_or("claude")
    }

    /// sub 追加時の既定 model alias（config 未指定 or 形式外なら **None = 記録しない**）。
    ///
    /// None のとき engine_model file は書かれず `--model` も注入されない = engine 側の
    /// user 既定（claude なら ~/.claude 設定）が効く（doc 54 §8-11）。形式外の値は record 時に
    /// 弾かれ lane 作成を壊すため、ここで is_valid_model を通し不正なら None へ degrade する
    /// （config typo で lane が作れなくなる事故を防ぐ）。
    pub fn default_lane_model(&self) -> Option<&str> {
        self.default_lane_model
            .as_deref()
            .filter(|m| crate::lane::engine_model::is_valid_model(m))
    }

    /// Resolve repo directory from various sources
    /// Priority: CLI flag > cwd > config default
    /// 相対パスは絶対パスに変換される
    pub fn resolve_repo_dir(cli_repo_dir: Option<&str>, config: &Config) -> String {
        let path = if let Some(dir) = cli_repo_dir {
            // 1. CLI flag (--repo-dir)
            std::path::PathBuf::from(dir)
        } else if let Ok(cwd) = std::env::current_dir() {
            // 2. Current working directory
            cwd
        } else if let Some(ref dir) = config.default_repo_dir {
            // 3. Config default（最終フォールバック）
            std::path::PathBuf::from(dir)
        } else {
            // 4. どれも使えない場合は "."
            std::path::PathBuf::from(".")
        };

        // 相対パスを絶対パスに変換
        Self::normalize_path(&path)
    }

    /// 指定パスに一致するrepoの 0-based インデックスを返す
    ///
    /// CWD や --repo-dir で解決されたパスが config 内のどのrepoに
    /// 対応するかを検索し、ポート割り当てに使用する。
    pub fn find_repo_index(&self, resolved_dir: &str) -> Option<usize> {
        self.repos.iter().position(|p| {
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
            if let Some(v) = ov.daemon_port {
                layout.daemon_port = v;
            }
            if let Some(v) = ov.repo_slot_base {
                layout.repo_slot_base = v;
            }
            if let Some(v) = ov.repo_slot_size {
                layout.repo_slot_size = v;
            }
            if let Some(v) = ov.max_repos {
                layout.max_repos = v;
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

    /// パスを正規化（相対パス→絶対パス変換）
    pub fn normalize_path(path: &std::path::Path) -> String {
        let resolved = if path.is_absolute() {
            // 絶対パスはそのまま正規化を試みる
            dunce::canonicalize(path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string())
        } else {
            // 相対パスをcwdからの絶対パスに変換
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(path))
                .and_then(|p| dunce::canonicalize(p).ok())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| path.display().to_string())
        };
        // canonicalize 失敗時 (未実在 path) は入力をそのまま返しているので、 verbatim
        // prefix 付きの入力が素通りしないようここでも落とす。
        strip_verbatim_prefix(&resolved).to_string()
    }
}

/// Windows の verbatim path prefix (`\\?\`) を落とす。 pure string 操作で、 全 OS で同じ結果。
///
/// `std::fs::canonicalize` は Windows で `\\?\C:\...` を返す。 これを repos.kdl に保存したり
/// repo の spawn 引数 (`-C`) に渡すと、 見た目が汚れるだけでなく「同じディレクトリなのに文字列が
/// 違う」 重複 entry を生む。 新規の正規化は [`dunce::canonicalize`] が防ぐが、 既に保存済みの
/// `\\?\` 付き path は読み込み時にここで落とす (移行)。
///
/// `dunce::simplified` と同じく「剥がして安全な形」だけを対象にする:
///
/// - drive letter 形式 (`\\?\C:\...`) のみ。 `\\?\UNC\server\share` は prefix を落とすと別 path。
/// - `MAX_PATH` (260) 以内のみ。 それを超える path は verbatim prefix が無いと Win32 API から
///   開けないので、 剥がさず温存する。 非 ASCII を含む path では byte 長で測る分だけ保守的に
///   (= 剥がさない側に) 倒れるが、 verbatim のまま残っても動作は正しい。
pub fn strip_verbatim_prefix(path: &str) -> &str {
    let Some(rest) = path.strip_prefix(r"\\?\") else {
        return path;
    };
    if rest.len() > 260 {
        return path;
    }
    let mut chars = rest.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(drive), Some(':'), Some('\\')) if drive.is_ascii_alphabetic() => rest,
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// drive letter 形式の verbatim prefix だけを落とす (pure string なので全 OS で同じ結果)。
    #[test]
    fn test_strip_verbatim_prefix() {
        // 落とす: `\\?\C:\...` (旧 Windows の canonicalize 出力)
        assert_eq!(
            strip_verbatim_prefix(r"\\?\C:\Users\mito\repos\vantage-point"),
            r"C:\Users\mito\repos\vantage-point"
        );
        assert_eq!(strip_verbatim_prefix(r"\\?\d:\tmp"), r"d:\tmp");

        // 落とさない: UNC は prefix を剥がすと別 path になる
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share"),
            r"\\?\UNC\server\share"
        );
        // 落とさない: drive letter の形をしていない
        assert_eq!(strip_verbatim_prefix(r"\\?\C:"), r"\\?\C:");
        assert_eq!(strip_verbatim_prefix(r"\\?\1:\x"), r"\\?\1:\x");

        // 落とさない: MAX_PATH 超は verbatim prefix が無いと Win32 API から開けない
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(strip_verbatim_prefix(&long), long);

        // prefix なしはそのまま (Mac/Linux の通常 path を含む)
        assert_eq!(strip_verbatim_prefix(r"C:\Users\mito"), r"C:\Users\mito");
        assert_eq!(
            strip_verbatim_prefix("/Users/makoto/repos/vp"),
            "/Users/makoto/repos/vp"
        );
        assert_eq!(strip_verbatim_prefix(""), "");
    }

    /// `normalize_path` は canonicalize 失敗時 (未実在 path) も verbatim prefix を残さない。
    #[test]
    fn test_normalize_path_strips_verbatim_on_fallback() {
        let missing = r"\\?\C:\definitely\does\not\exist\vp-test";
        let normalized = Config::normalize_path(std::path::Path::new(missing));
        assert!(
            !normalized.starts_with(r"\\?\"),
            "verbatim prefix が残っている: {normalized}"
        );
    }

    /// VP-189: 全 section を含む config.kdl が正しく parse される
    #[test]
    fn test_full_config_kdl_parses() {
        let kdl = r#"
default-repo-dir "/home/user/repos/main"
default-port 33001
claude-cli-path "/opt/claude/bin/claude"
default-agent "claude"
hub-addr "hub.chronista.club:12879"
startup {
    max-concurrent-lane-spawn 3
}
"#;
        let config: Config = club_kdl::from_str(kdl).expect("config.kdl parse");
        assert_eq!(
            config.default_repo_dir.as_deref(),
            Some("/home/user/repos/main")
        );
        assert_eq!(config.default_port, 33001);
        assert_eq!(
            config.claude_cli_path.as_deref(),
            Some("/opt/claude/bin/claude")
        );
        assert_eq!(config.default_agent.as_deref(), Some("claude"));
        assert_eq!(config.hub_addr.as_deref(), Some("hub.chronista.club:12879"));
        assert_eq!(config.startup.max_concurrent_lane_spawn, 3);
        // repos は config.kdl に出さない (#[kdl(skip)]、 SSOT は repos.kdl)
        assert!(config.repos.is_empty());
    }

    #[test]
    fn test_vp_config_dir_ends_with_vp() {
        // VP-192: config dir は OS によらず末尾が app_dir_name (= profile 準拠: brew "vp" / dev "vp-dev")。
        // "vp" 固定 assert だと VP_PROFILE=dev 環境 (lane 内 dogfood) の cargo test で偽陽性に落ちる。
        let dir = vp_config_dir();
        assert!(
            dir.ends_with(vp_paths::app_dir_name()),
            "vp_config_dir は app_dir_name で終わるべき: {}",
            dir.display()
        );
    }

    #[test]
    fn test_vp_data_dir_ends_with_vp() {
        // VP-192: data dir も末尾が app_dir_name (profile 準拠、上の test と同旨)
        let dir = vp_data_dir();
        assert!(
            dir.ends_with(vp_paths::app_dir_name()),
            "vp_data_dir は app_dir_name で終わるべき: {}",
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

    /// VP-189: 実運用で最も多い形 — 単一 section だけの最小 config.kdl
    #[test]
    fn test_minimal_config_kdl_parses() {
        let kdl = r#"
startup {
    max-concurrent-lane-spawn 3
}
"#;
        let config: Config = club_kdl::from_str(kdl).expect("minimal config.kdl parse");
        assert_eq!(config.startup.max_concurrent_lane_spawn, 3);
        assert!(config.default_repo_dir.is_none());
        // hub-addr node 不在 → None (= federation off、 machine-local 動作)
        assert!(config.hub_addr.is_none());
        // default-port node 不在 → KDL field default は 0 (load の post-process で 33000)
        assert_eq!(config.default_port, 0);
        // default-lane-model node 不在 → None（getter も None = 記録しない、doc 54 §8-11）
        assert!(config.default_lane_model.is_none());
        assert_eq!(config.default_lane_model(), None);
    }

    #[test]
    fn test_default_lane_model_kdl_parses() {
        let kdl = r#"
default-lane-model "claude-sonnet-5"
"#;
        let config: Config = club_kdl::from_str(kdl).expect("default-lane-model parse");
        assert_eq!(
            config.default_lane_model.as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(config.default_lane_model(), Some("claude-sonnet-5"));
    }

    /// VP-189: section を 1 つも持たない空 config.kdl でも parse できる
    #[test]
    fn test_comment_only_config_kdl_parses() {
        let config: Config = club_kdl::from_str("// 空 config\n").expect("comment-only parse");
        assert!(config.default_repo_dir.is_none());
        assert!(config.repos.is_empty());
    }

    /// VP-189: KDL field default (型 Default = 0) を意味のある値に補正する
    #[test]
    fn test_apply_load_defaults_corrects_zero_values() {
        let mut config = Config {
            default_port: 0,
            startup: StartupConfig {
                max_concurrent_lane_spawn: 0,
            },
            ..Config::default()
        };
        config.apply_load_defaults();
        assert_eq!(config.default_port, 33000);
        assert_eq!(config.startup.max_concurrent_lane_spawn, 1);
    }

    #[test]
    fn test_default_lane_model_none_defers_to_engine() {
        // doc 54 §8-11: 未設定 → None = 記録しない（engine 側の user 既定に委ねる。
        // 旧「Opus 強制」の再演をここで塞ぐ）
        let cfg = Config::default();
        assert_eq!(cfg.default_lane_model(), None);
        // 明示設定はそのまま採用
        let cfg = Config {
            default_lane_model: Some("claude-sonnet-5".to_string()),
            ..Config::default()
        };
        assert_eq!(cfg.default_lane_model(), Some("claude-sonnet-5"));
        // 形式外の値は record を壊すため None へ degrade（config typo で lane 作成が死なない）
        let cfg = Config {
            default_lane_model: Some("opus; rm -rf /".to_string()),
            ..Config::default()
        };
        assert_eq!(cfg.default_lane_model(), None);
    }

    /// VP-189: 既に有効な値が入っていれば apply_load_defaults は上書きしない
    #[test]
    fn test_apply_load_defaults_preserves_explicit_values() {
        let mut config = Config {
            default_port: 33005,
            startup: StartupConfig {
                max_concurrent_lane_spawn: 4,
            },
            ..Config::default()
        };
        config.apply_load_defaults();
        assert_eq!(config.default_port, 33005);
        assert_eq!(config.startup.max_concurrent_lane_spawn, 4);
    }
}
