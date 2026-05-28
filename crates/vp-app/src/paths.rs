//! VP の config / data / state パス解決 (XDG Base Directory 準拠)。
//!
//! `vantage_point::config` の `vp_config_dir()` / `vp_data_dir()` / `vp_state_dir()`
//! / `vp_log_dir()` / `vp_sessions_dir()` / `migrate_legacy_paths()` と **同一定義**。
//! vp-app は `vantage-point` crate (SurrealDB / wry / midir 等を含む重量 crate) に
//! 意図的に依存しない方針 (`client.rs` の lite struct 参照) のため、 ここに複製する。
//!
//! ## パス方針 (canonical: `vantage_point::config` モジュール doc)
//!
//! | zone   | 環境変数            | default                  | 用途 |
//! |--------|---------------------|--------------------------|------|
//! | config | `$XDG_CONFIG_HOME`  | `~/.config/vp/`          | 人が編集 |
//! | data   | `$XDG_DATA_HOME`    | `~/.local/share/vp/`     | 永続 data store |
//! | state  | `$XDG_STATE_HOME`   | `~/.local/state/vp/`     | runtime state + log |

use std::path::PathBuf;

/// VP の config zone (XDG `$XDG_CONFIG_HOME/vp/`、 default `~/.config/vp/`)。
pub fn vp_config_dir() -> PathBuf {
    xdg_base("XDG_CONFIG_HOME", ".config")
}

/// VP の data zone (XDG `$XDG_DATA_HOME/vp/`、 default `~/.local/share/vp/`)。
pub fn vp_data_dir() -> PathBuf {
    xdg_base("XDG_DATA_HOME", ".local/share")
}

/// VP の state zone (XDG `$XDG_STATE_HOME/vp/`、 default `~/.local/state/vp/`)。
pub fn vp_state_dir() -> PathBuf {
    xdg_base("XDG_STATE_HOME", ".local/state")
}

/// log 出力先 (`vp_state_dir()/log/`)。
pub fn vp_log_dir() -> PathBuf {
    vp_state_dir().join("log")
}

/// TUI SessionManager per-port state (`vp_state_dir()/sessions/`)。
pub fn vp_sessions_dir() -> PathBuf {
    vp_state_dir().join("sessions")
}

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

/// 旧 path から XDG 新 path への冪等な migration + 廃止物 cleanup。
///
/// `vantage_point::config::migrate_legacy_paths` と同一ロジックを複製。 daemon
/// (vp-cli) 側と vp-app の双方が起動時に呼ぶが、 冪等設計なので二重実行で
/// 一方が先に旧 path を空にしてしまっても問題ない。
pub fn migrate_legacy_paths() {
    let new_config = vp_config_dir();
    let new_data = vp_data_dir();
    let new_state = vp_state_dir();
    let new_log = vp_log_dir();

    if let Some(home) = dirs::home_dir() {
        let mac_app_support = home.join("Library/Application Support/vp");
        if mac_app_support.is_dir() {
            move_file_if_exists(
                &mac_app_support.join("config.kdl"),
                &new_config.join("config.kdl"),
            );
            move_file_if_exists(
                &mac_app_support.join("projects.kdl"),
                &new_config.join("projects.kdl"),
            );
            move_file_if_exists(
                &mac_app_support.join("addresses.toml"),
                &new_config.join("addresses.toml"),
            );
            move_dir_if_exists(&mac_app_support.join("db"), &new_data.join("db"));
            move_dir_if_exists(&mac_app_support.join("discs"), &new_data.join("discs"));
            move_file_if_exists(
                &mac_app_support.join("session-state.json"),
                &new_state.join("session.json"),
            );
            migrate_state_subdir(&mac_app_support.join("state"), &vp_sessions_dir());
            move_file_if_exists(
                &mac_app_support.join("logs/debug.log"),
                &new_log.join("debug.log"),
            );

            for legacy in ["config.toml", "running.json", "vantage.db"] {
                delete_file_if_exists(&mac_app_support.join(legacy));
            }
            for legacy_dir in ["lanes", "scripts", "logs"] {
                delete_dir_if_exists(&mac_app_support.join(legacy_dir));
            }
            delete_dir_if_empty(&mac_app_support);
        }

        let mac_log_dir = home.join("Library/Logs/Vantage");
        if mac_log_dir.is_dir() {
            move_file_if_exists(
                &mac_log_dir.join("app.kdl.log"),
                &new_log.join("app.kdl.log"),
            );
            move_file_if_exists(
                &mac_log_dir.join("app.stdout.log"),
                &new_log.join("app.stdout.log"),
            );
            move_file_if_exists(
                &mac_log_dir.join("daemon.kdl.log"),
                &new_log.join("daemon.kdl.log"),
            );
            move_file_if_exists(
                &mac_log_dir.join("daemon.stdout.log"),
                &new_log.join("daemon.stdout.log"),
            );
            for legacy in ["vp-app.kdl.log", "vp-app.stdout.log", "vp-world.kdl.log"] {
                delete_file_if_exists(&mac_log_dir.join(legacy));
            }
            delete_dir_if_empty(&mac_log_dir);
        }
    }

    if let Some(cfg) = dirs::config_dir() {
        let legacy_data = cfg.join("vantage");
        if legacy_data.is_dir() && legacy_data != new_data {
            move_dir_contents(&legacy_data, &new_data);
            delete_dir_if_empty(&legacy_data);
        }
    }
}

fn migrate_state_subdir(legacy_state: &std::path::Path, new_sessions: &std::path::Path) {
    if !legacy_state.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(legacy_state) else {
        return;
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with("-panes.json") || name.ends_with("-canvas-layout.json") {
            delete_file_if_exists(&from);
        } else if name.ends_with(".json") {
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
        delete_file_if_exists(from);
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::rename(from, to).is_err() && std::fs::copy(from, to).is_ok() {
        let _ = std::fs::remove_file(from);
    }
}

fn move_dir_if_exists(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_dir() {
        return;
    }
    if to.exists() && dir_has_entries(to) {
        let _ = std::fs::remove_dir_all(from);
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::rename(from, to).is_err() && copy_dir_recursive(from, to).is_ok() {
        let _ = std::fs::remove_dir_all(from);
    }
}

fn move_dir_contents(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_dir() {
        return;
    }
    let _ = std::fs::create_dir_all(to);
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let child_from = entry.path();
        let Some(name) = child_from.file_name() else {
            continue;
        };
        let child_to = to.join(name);
        if child_from.is_dir() {
            move_dir_if_exists(&child_from, &child_to);
        } else if child_from.is_file() {
            move_file_if_exists(&child_from, &child_to);
        }
    }
}

fn delete_file_if_exists(path: &std::path::Path) {
    if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

fn delete_dir_if_exists(path: &std::path::Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn delete_dir_if_empty(path: &std::path::Path) {
    if path.is_dir() && !dir_has_entries(path) {
        let _ = std::fs::remove_dir(path);
    }
}

fn dir_has_entries(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vp_config_dir_ends_with_vp() {
        assert!(vp_config_dir().ends_with("vp"));
    }

    #[test]
    fn test_vp_data_dir_ends_with_vp() {
        assert!(vp_data_dir().ends_with("vp"));
    }

    #[test]
    fn test_vp_state_dir_ends_with_vp() {
        assert!(vp_state_dir().ends_with("vp"));
    }

    #[test]
    fn test_vp_log_dir_ends_with_log() {
        assert!(vp_log_dir().ends_with("log"));
    }
}
