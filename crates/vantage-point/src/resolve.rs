//! ターゲット解決モジュール
//!
//! 全コマンド共通のプロジェクト解決ロジックを提供する。
//! CLI 引数（target）から実行対象のプロジェクトを特定し、
//! ポート番号やプロジェクト名を解決する。

use anyhow::{Result, bail};

use crate::cli::{PORT_RANGE_END, PORT_RANGE_START};
use crate::config::{Config, RunningProcesses};

/// ターゲット解決結果
pub enum ResolvedTarget {
    /// 実行中の Process
    Running {
        port: u16,
        name: String,
        project_dir: String,
    },
    /// 設定済みだが未起動
    Configured {
        name: String,
        path: String,
        index: usize,
    },
    /// 未登録ディレクトリ（cwd から検出）
    Cwd { path: String },
}

/// target 解決
///
/// 優先順位:
/// 1. None → cwd から running.json/config を検索
/// 2. 数値文字列 → プロジェクトインデックス（後方互換、1始まり）
/// 3. 文字列 → プロジェクト名検索
pub fn resolve_target(target: Option<&str>, config: &Config) -> Result<ResolvedTarget> {
    match target {
        None => resolve_from_cwd(config),
        Some(s) => {
            if let Ok(idx) = s.parse::<usize>() {
                resolve_by_index(idx, config)
            } else {
                resolve_by_name(s, config)
            }
        }
    }
}

/// cwd からプロジェクトを解決
fn resolve_from_cwd(config: &Config) -> Result<ResolvedTarget> {
    let cwd = std::env::current_dir()?;
    let cwd_str = Config::normalize_path(&cwd);

    // 1. running.json で実行中の Process を検索（サブディレクトリもマッチ）
    if let Some(running) = RunningProcesses::find_for_cwd() {
        let name = project_name_from_path(&running.project_dir, config);
        return Ok(ResolvedTarget::Running {
            port: running.port,
            name,
            project_dir: running.project_dir,
        });
    }

    // 2. config でプロジェクトを検索（完全一致）
    if let Some(idx) = config.find_project_index(&cwd_str) {
        let project = &config.projects[idx];
        return Ok(ResolvedTarget::Configured {
            name: project.name.clone(),
            path: cwd_str,
            index: idx,
        });
    }

    // 3. config でサブディレクトリマッチ（最も具体的なパスを優先）
    let best_match = config
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let normalized = Config::normalize_path(std::path::Path::new(&p.path));
            cwd_str.starts_with(&format!("{}/", normalized))
        })
        .max_by_key(|(_, p)| Config::normalize_path(std::path::Path::new(&p.path)).len());

    if let Some((idx, project)) = best_match {
        let path = Config::normalize_path(std::path::Path::new(&project.path));
        return Ok(ResolvedTarget::Configured {
            name: project.name.clone(),
            path,
            index: idx,
        });
    }

    // 4. 未登録ディレクトリ
    Ok(ResolvedTarget::Cwd { path: cwd_str })
}

/// プロジェクトインデックスから解決（1始まり、後方互換）
fn resolve_by_index(index: usize, config: &Config) -> Result<ResolvedTarget> {
    if index == 0 || index > config.projects.len() {
        bail!(
            "Invalid project index {}. Use `vp config` to list projects (1\u{2013}{}).",
            index,
            config.projects.len()
        );
    }

    let project = &config.projects[index - 1];
    let path = Config::normalize_path(std::path::Path::new(&project.path));

    // 実行中かチェック
    if let Some(running) = RunningProcesses::find_by_project(&path) {
        return Ok(ResolvedTarget::Running {
            port: running.port,
            name: project.name.clone(),
            project_dir: path,
        });
    }

    Ok(ResolvedTarget::Configured {
        name: project.name.clone(),
        path,
        index: index - 1,
    })
}

/// プロジェクト名から解決
fn resolve_by_name(name: &str, config: &Config) -> Result<ResolvedTarget> {
    let found = config
        .projects
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == name);

    match found {
        Some((idx, project)) => {
            let path = Config::normalize_path(std::path::Path::new(&project.path));

            // 実行中かチェック
            if let Some(running) = RunningProcesses::find_by_project(&path) {
                return Ok(ResolvedTarget::Running {
                    port: running.port,
                    name: project.name.clone(),
                    project_dir: path,
                });
            }

            Ok(ResolvedTarget::Configured {
                name: project.name.clone(),
                path,
                index: idx,
            })
        }
        None => bail!(
            "Project '{}' not found. Use `vp config` to list registered projects.",
            name
        ),
    }
}

/// パスからプロジェクト名を取得（config になければディレクトリ名）
pub fn project_name_from_path(project_dir: &str, config: &Config) -> String {
    for project in &config.projects {
        let normalized = Config::normalize_path(std::path::Path::new(&project.path));
        if normalized == project_dir {
            return project.name.clone();
        }
    }

    // ディレクトリ名をフォールバック
    std::path::Path::new(project_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 空きポートを同期的に検索（running.json + ポートバインド確認）
pub fn find_available_port() -> Option<u16> {
    let used_ports: std::collections::HashSet<u16> = RunningProcesses::list()
        .into_iter()
        .map(|s| s.port)
        .collect();

    (PORT_RANGE_START..=PORT_RANGE_END)
        .find(|port| !used_ports.contains(port) && is_port_available(*port))
}

/// ポートが利用可能かバインドして確認
fn is_port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Configured ターゲットのポートを決定
///
/// config にプロジェクト固有のポート設定があればそれを使い、
/// なければ index ベースで割り当てる（33000 + index）
pub fn port_for_configured(index: usize, config: &Config) -> Result<u16> {
    // プロジェクト固有のポート設定を優先
    if let Some(project) = config.projects.get(index)
        && let Some(port) = project.port
    {
        return Ok(port);
    }

    let max_projects = (PORT_RANGE_END - PORT_RANGE_START + 1) as usize;
    if index >= max_projects {
        bail!(
            "Project index {} exceeds port range. Max {} projects supported.",
            index,
            max_projects
        );
    }
    Ok(PORT_RANGE_START + index as u16)
}
