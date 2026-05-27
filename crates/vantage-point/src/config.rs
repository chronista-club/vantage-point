//! Configuration management
//!
//! ## VP-189: config.toml → config.kdl 統一
//!
//! VP の設定ファイルは元々 TOML だったが、 projects.kdl (VP-188) / lane の
//! wing-files.kdl 等、 周辺の設定は既に KDL に揃っていた。 config 本体だけ
//! TOML で取り残されていたのを KDL に統一し、 club-kdl 資産を一本化する。
//!
//! - config.kdl は **人間が編集する read-only な global 設定**。 VP 自身は
//!   書き戻さない (= `KdlSerialize` 不要、 `KdlDeserialize` のみ)。
//! - registered projects は projects.kdl が SSOT (VP-188)。 config.kdl には出さない。
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
//! | config | `$XDG_CONFIG_HOME`        | `~/.config/vp/`          | 人が編集 (config.kdl / projects.kdl / addresses.toml) |
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

/// VP の config zone (XDG `$XDG_CONFIG_HOME/vp/`、 default `~/.config/vp/`)。
///
/// 人が編集する設定 (config.kdl / projects.kdl / addresses.toml) の置き場。
/// `XDG_CONFIG_HOME` 環境変数を優先、 未設定なら `$HOME/.config/vp/`。 macOS
/// でも `~/Library/Application Support/` は使わない (= dotfile 一極集中方針)。
pub fn vp_config_dir() -> PathBuf {
    xdg_base("XDG_CONFIG_HOME", ".config")
}

/// VP の data zone (XDG `$XDG_DATA_HOME/vp/`、 default `~/.local/share/vp/`)。
///
/// 永続 data store (SurrealDB の `db/`、 Whitesnake `discs/`)。 失っても再生成
/// される類の cache ではなく、 失えない user data を置く。
pub fn vp_data_dir() -> PathBuf {
    xdg_base("XDG_DATA_HOME", ".local/share")
}

/// VP の state zone (XDG `$XDG_STATE_HOME/vp/`、 default `~/.local/state/vp/`)。
///
/// runtime state + log。 vp-app UI state (`session.json`)、 TUI SessionManager
/// (`sessions/{port}.json`)、 全 process の log (`log/`) を置く。 XDG spec で
/// log は state zone 配下が standard。
pub fn vp_state_dir() -> PathBuf {
    xdg_base("XDG_STATE_HOME", ".local/state")
}

/// log 出力先 (`vp_state_dir()/log/`)。 daemon / SP / vp-app の全 log を集約。
pub fn vp_log_dir() -> PathBuf {
    vp_state_dir().join("log")
}

/// TUI SessionManager の per-port state file 置き場 (`vp_state_dir()/sessions/`)。
pub fn vp_sessions_dir() -> PathBuf {
    vp_state_dir().join("sessions")
}

/// XDG base directory 解決ヘルパー。
///
/// `$env_name` 環境変数が absolute path なら採用、 そうでなければ
/// `$HOME/{home_relative}` を使う。 いずれも末尾に `vp` を join する。
/// `$HOME` 未取得時は `.` fallback (= test/sandbox 用)。
fn xdg_base(env_name: &str, home_relative: &str) -> PathBuf {
    if let Some(v) = std::env::var_os(env_name) {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            return p.join("vp");
        }
    }
    dirs::home_dir()
        .map(|h| h.join(home_relative))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vp")
}

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

/// 旧 path から XDG 新 path への冪等な migration + 廃止物 cleanup。
///
/// VP は過去、 path 体系が複数世代並存していた:
/// - 世代 1: `~/.config/vp/` 直書き
/// - 世代 2: `dirs::config_dir()/vantage/`
/// - 世代 3 (VP-192): macOS `~/Library/Application Support/vp/` + `~/Library/Logs/Vantage/`
/// - 世代 4 (= 本 fn): XDG 統一 `~/.config/vp/` + `~/.local/share/vp/` + `~/.local/state/vp/`
///
/// 本 fn は世代 1〜3 から世代 4 への移行を 1 pass で行い、 同時に廃止 file/dir
/// (running.json / vantage.db / config.toml / lanes/ / scripts/ 等) を delete する。
///
/// 設計:
/// - **move (rename)** で旧データを新位置へ。 同一 device なら atomic、 跨ぐなら copy+remove。
/// - **冪等**。 新 path に既にデータがあれば旧側を skip + 空なら delete。
/// - **廃止物 delete** は code 参照ゼロ確認済 entry のみ (= safe)。
/// - 失敗しても起動阻害しない (warn ログのみ)。
///
/// main 初期化の早い段階で 1 回呼ぶこと。
pub fn migrate_legacy_paths() {
    let new_config = vp_config_dir();
    let new_data = vp_data_dir();
    let new_state = vp_state_dir();
    let new_log = vp_log_dir();

    // 世代 3: macOS `~/Library/Application Support/vp/` から各 zone へ拡散 move。
    if let Some(home) = dirs::home_dir() {
        let mac_app_support = home.join("Library/Application Support/vp");
        if mac_app_support.is_dir() {
            // config zone へ
            move_file_if_exists(&mac_app_support.join("config.kdl"), &new_config.join("config.kdl"));
            move_file_if_exists(&mac_app_support.join("projects.kdl"), &new_config.join("projects.kdl"));
            move_file_if_exists(&mac_app_support.join("addresses.toml"), &new_config.join("addresses.toml"));
            // data zone へ
            move_dir_if_exists(&mac_app_support.join("db"), &new_data.join("db"));
            move_dir_if_exists(&mac_app_support.join("discs"), &new_data.join("discs"));
            // state zone へ (rename: session-state.json → session.json)
            move_file_if_exists(&mac_app_support.join("session-state.json"), &new_state.join("session.json"));
            // TUI SessionManager: state/{port}.json → sessions/{port}.json
            migrate_state_subdir(&mac_app_support.join("state"), &vp_sessions_dir());
            // log: logs/debug.log → log/debug.log
            move_file_if_exists(&mac_app_support.join("logs/debug.log"), &new_log.join("debug.log"));

            // 廃止 file delete
            for legacy in [
                "config.toml",       // KDL 統合済
                "running.json",      // discovery 移行済
                "vantage.db",        // code 参照ゼロ
            ] {
                delete_file_if_exists(&mac_app_support.join(legacy));
            }
            for legacy_dir in ["lanes", "scripts", "logs"] {
                delete_dir_if_exists(&mac_app_support.join(legacy_dir));
            }
            // 空になった元 dir も掃除
            delete_dir_if_empty(&mac_app_support);
        }

        // 世代 3: macOS `~/Library/Logs/Vantage/` から log zone へ
        let mac_log_dir = home.join("Library/Logs/Vantage");
        if mac_log_dir.is_dir() {
            move_file_if_exists(&mac_log_dir.join("app.kdl.log"), &new_log.join("app.kdl.log"));
            move_file_if_exists(&mac_log_dir.join("app.stdout.log"), &new_log.join("app.stdout.log"));
            move_file_if_exists(&mac_log_dir.join("daemon.kdl.log"), &new_log.join("daemon.kdl.log"));
            move_file_if_exists(&mac_log_dir.join("daemon.stdout.log"), &new_log.join("daemon.stdout.log"));
            // 廃止 (rename 前の遺物)
            for legacy in ["vp-app.kdl.log", "vp-app.stdout.log", "vp-world.kdl.log"] {
                delete_file_if_exists(&mac_log_dir.join(legacy));
            }
            delete_dir_if_empty(&mac_log_dir);
        }
    }

    // 世代 1: 旧 `~/.config/vp/` 直書き世代と現 XDG `~/.config/vp/` は **同一 path**。
    // 既に新 path なので何もしない。

    // 世代 2: `dirs::config_dir()/vantage/` → 新 data zone
    if let Some(cfg) = dirs::config_dir() {
        let legacy_data = cfg.join("vantage");
        if legacy_data.is_dir() && legacy_data != new_data {
            move_dir_contents(&legacy_data, &new_data, "vantage→data");
            delete_dir_if_empty(&legacy_data);
        }
    }
}

/// `state/{port}.json` は TUI SessionManager 用 = 新 `sessions/{port}.json` へ。
/// `state/{port}-panes.json` / `state/{port}-canvas-layout.json` は D11 廃止遺物 = delete。
fn migrate_state_subdir(legacy_state: &std::path::Path, new_sessions: &std::path::Path) {
    if !legacy_state.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(legacy_state) else { return };
    for entry in entries.flatten() {
        let from = entry.path();
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else { continue };
        if name.ends_with("-panes.json") || name.ends_with("-canvas-layout.json") {
            delete_file_if_exists(&from);
        } else if name.ends_with(".json") {
            // {port}.json は active な SessionManager state
            move_file_if_exists(&from, &new_sessions.join(name));
        }
    }
    delete_dir_if_empty(legacy_state);
}

fn move_file_if_exists(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_file() {
        return;
    }
    if to.exists() {
        // 新側に既にあれば旧は黙って消す (= 冪等、 新側 SSOT 維持)
        delete_file_if_exists(from);
        return;
    }
    if let Some(parent) = to.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "path migration: parent create 失敗 ({}): {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = std::fs::rename(from, to) {
        // 跨デバイス等で rename 失敗 → copy + remove fallback
        if std::fs::copy(from, to).is_ok() {
            let _ = std::fs::remove_file(from);
            tracing::info!(
                "path migration (file copy+remove): {} → {}",
                from.display(),
                to.display()
            );
        } else {
            tracing::warn!(
                "path migration 失敗 ({} → {}): {e}",
                from.display(),
                to.display()
            );
        }
    } else {
        tracing::info!(
            "path migration (file move): {} → {}",
            from.display(),
            to.display()
        );
    }
}

fn move_dir_if_exists(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_dir() {
        return;
    }
    if to.exists() && dir_has_entries(to) {
        // 新側にデータあり → 旧側を削除
        let _ = std::fs::remove_dir_all(from);
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::rename(from, to) {
        // fallback: 再帰コピー + 削除
        if copy_dir_recursive(from, to).is_ok() {
            let _ = std::fs::remove_dir_all(from);
            tracing::info!(
                "path migration (dir copy+remove): {} → {}",
                from.display(),
                to.display()
            );
        } else {
            tracing::warn!(
                "path migration 失敗 ({} → {}): {e}",
                from.display(),
                to.display()
            );
        }
    } else {
        tracing::info!(
            "path migration (dir move): {} → {}",
            from.display(),
            to.display()
        );
    }
}

/// 中身を子単位で新 dir に move (= dir 自体は残す方針)。
fn move_dir_contents(from: &std::path::Path, to: &std::path::Path, label: &str) {
    if !from.is_dir() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(to) {
        tracing::warn!(
            "path migration ({label}) parent create 失敗: {e}"
        );
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else { return };
    for entry in entries.flatten() {
        let child_from = entry.path();
        let Some(name) = child_from.file_name() else { continue };
        let child_to = to.join(name);
        if child_from.is_dir() {
            move_dir_if_exists(&child_from, &child_to);
        } else if child_from.is_file() {
            move_file_if_exists(&child_from, &child_to);
        }
    }
}

fn delete_file_if_exists(path: &std::path::Path) {
    if path.is_file()
        && let Err(e) = std::fs::remove_file(path)
    {
        tracing::warn!("path cleanup 失敗 ({}): {e}", path.display());
    }
}

fn delete_dir_if_exists(path: &std::path::Path) {
    if path.is_dir()
        && let Err(e) = std::fs::remove_dir_all(path)
    {
        tracing::warn!("path cleanup 失敗 ({}): {e}", path.display());
    }
}

fn delete_dir_if_empty(path: &std::path::Path) {
    if path.is_dir() && !dir_has_entries(path) {
        let _ = std::fs::remove_dir(path);
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

/// Config file path (`config_dir()/config.kdl` ── VP-189 で config.toml から移行)
fn config_file_path() -> PathBuf {
    config_dir().join("config.kdl")
}

/// Vantage Process configuration
///
/// config.kdl は document スタイルの KDL。 各 scalar 設定は document 直下の
/// 単一引数 node (`default-port 33000` 等)、 section は子 node (`network { ... }`)。
#[derive(Debug, Clone, Serialize, Deserialize, Default, KdlDeserialize)]
#[kdl(document)]
pub struct Config {
    /// Default project directory for Claude agent
    #[serde(default)]
    #[kdl(child, name = "default-project-dir", unwrap_arg)]
    pub default_project_dir: Option<String>,

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

    /// Lane 作成時の default Stand 名 (例: "echoes" / "shell" / "tmux")。
    ///
    /// `mise run vp:stand:{name}` の `name` 部分を指定。 None なら "echoes" fallback
    /// (`Config::default_stand_or_echoes()` 経由)。
    ///
    /// doc 11 §3 (Stand init_script system / mise task 路線)、 PR-B 対応。
    /// PR-pre2 (VP-118): "hd" → "echoes" rename (Stand metaphor + identifier sweep)。
    #[serde(default)]
    #[kdl(child, name = "default-stand", unwrap_arg)]
    pub default_stand: Option<String>,

    /// Projects configuration
    ///
    /// VP-188: SSOT は `~/.config/vp/projects.kdl`。 `Config::load()` が projects.kdl を
    /// 読んで本 field を populate する。 config.kdl には一切出さない (`#[kdl(skip)]`、
    /// = 二重 SSOT 防止)。 永続化は `persist_projects_kdl()`。
    #[serde(default, skip_serializing)]
    #[kdl(skip)]
    pub projects: Vec<ProjectConfig>,

    /// Port layout overrides (optional、default は PortLayout::default())
    ///
    /// VP-189: port layout の上書きは advanced 機能で dogfood でも未使用のため、
    /// config.kdl からは設定不可とした (`#[kdl(skip)]`)。 必要になった時点で
    /// 専用機構を足す (= config.kdl は「ミニマムな global 設定」に保つ方針)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[kdl(skip)]
    pub ports: Option<PortLayoutOverrides>,

    /// SP startup behavior — Wing spawn の concurrency 制限等 (I-b、 2026-04-30)
    #[serde(default)]
    #[kdl(child, default)]
    pub startup: StartupConfig,

    /// VP-154 PR-3.5: LAN networking config (= mDNS / hub federation の挙動を tweak)
    #[serde(default)]
    #[kdl(child, default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, KdlDeserialize)]
#[kdl(name = "network")]
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
    #[kdl(child, name = "advertise-hostname", unwrap_arg)]
    pub advertise_hostname: Option<String>,
}

/// SP startup behavior config (I-b、 2026-04-30)。
///
/// Mailbox actor (`lane-spawn@<project>`) で Wing spawn を Cmd 化した上で、
/// 内部 Semaphore で同時実行数を gate する。 `max_concurrent_lane_spawn` で
/// 制限値を tweak、 default は **1** (= 完全 sequential、 dogfood の視覚 pop 体験 +
/// Claude CLI rate-limit 安全)。 計測 log (`Lane spawn completed: ... elapsed=`) を
/// dogfood で集計して N 値を実証的に上げる方針。
#[derive(Debug, Clone, Serialize, Deserialize, KdlDeserialize)]
#[kdl(name = "startup")]
pub struct StartupConfig {
    /// 同時 Lane spawn 数の上限 (= Mailbox actor 内部 `Semaphore::new(N)`)。
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
    /// Load config from XDG config file (`~/.config/vp/config.kdl`)
    ///
    /// VP-189: config 形式を KDL に統一。 config.kdl が無い / 空なら `Config::default()`。
    ///
    /// VP-188: registered projects の SSOT は `~/.config/vp/projects.kdl`。
    /// config.kdl をパースした後、 projects.kdl が存在すれば `projects` field を
    /// **projects.kdl の内容で populate** する。 これで `config.projects` を読む全
    /// caller (resolve / TUI / lane / reload_config) が projects.kdl を SSOT として
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

        // VP-188: projects.kdl が SSOT。 存在すれば config.projects を populate。
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

    /// VP-189: 全 section を含む config.kdl が正しく parse される
    #[test]
    fn test_full_config_kdl_parses() {
        let kdl = r#"
default-project-dir "/home/user/projects/main"
default-port 33001
claude-cli-path "/opt/claude/bin/claude"
default-stand "echoes"
network {
    advertise-hostname "mito-mac"
}
startup {
    max-concurrent-lane-spawn 3
}
"#;
        let config: Config = club_kdl::from_str(kdl).expect("config.kdl parse");
        assert_eq!(
            config.default_project_dir.as_deref(),
            Some("/home/user/projects/main")
        );
        assert_eq!(config.default_port, 33001);
        assert_eq!(
            config.claude_cli_path.as_deref(),
            Some("/opt/claude/bin/claude")
        );
        assert_eq!(config.default_stand.as_deref(), Some("echoes"));
        assert_eq!(
            config.network.advertise_hostname.as_deref(),
            Some("mito-mac")
        );
        assert_eq!(config.startup.max_concurrent_lane_spawn, 3);
        // projects は config.kdl に出さない (#[kdl(skip)]、 SSOT は projects.kdl)
        assert!(config.projects.is_empty());
    }

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

    /// VP-189: 実運用で最も多い形 — network section だけの最小 config.kdl
    #[test]
    fn test_minimal_config_kdl_parses() {
        let kdl = r#"
network {
    advertise-hostname "mito-mac"
}
"#;
        let config: Config = club_kdl::from_str(kdl).expect("minimal config.kdl parse");
        assert_eq!(
            config.network.advertise_hostname.as_deref(),
            Some("mito-mac")
        );
        assert!(config.default_project_dir.is_none());
        // default-port node 不在 → KDL field default は 0 (load の post-process で 33000)
        assert_eq!(config.default_port, 0);
        // startup node 不在 → StartupConfig::default() で max=1
        assert_eq!(config.startup.max_concurrent_lane_spawn, 1);
    }

    /// VP-189: section を 1 つも持たない空 config.kdl でも parse できる
    #[test]
    fn test_comment_only_config_kdl_parses() {
        let config: Config = club_kdl::from_str("// 空 config\n").expect("comment-only parse");
        assert!(config.default_project_dir.is_none());
        assert!(config.network.advertise_hostname.is_none());
        assert!(config.projects.is_empty());
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
