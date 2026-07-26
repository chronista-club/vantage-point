//! ターゲット解決モジュール
//!
//! 全コマンド共通のプロジェクト解決ロジックを提供する。
//! CLI 引数（target）から実行対象のプロジェクトを特定し、
//! ポート番号やプロジェクト名を解決する。

use anyhow::{Result, bail};

use crate::config::Config;

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

    // 1. 稼働中 Process を検索（daemon API → HTTP スキャンフォールバック）
    if let Some(running) = crate::discovery::find_for_cwd_blocking() {
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
    if let Some(running) = crate::discovery::find_by_project_blocking(&path) {
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
            if let Some(running) = crate::discovery::find_by_project_blocking(&path) {
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

/// 正規化済み path から登録 project 名を **SP 非依存 (config のみ)** に解決する純関数
/// (wire identity SSOT)。完全一致 → longest-prefix サブディレクトリ一致 → `None`。
///
/// `resolve_from_cwd` の config 経路だけを抜き出した純 config lookup。discovery / daemon を
/// 引かないので、自 project の SP が落ちていても conductor の canonical address
/// (`agent@<project>`) を確定できる。I/O なし (cwd は呼び出し側で正規化して渡す) で単体 test 可能。
///
/// 戻り値は登録 project の `name` (= [`project_name_from_path`] が登録 path に返すのと同じ値、
/// SSOT は `config.projects[].name`)。未登録 path は `None` → 呼び出し側で fail-closed する
/// (誤 identity で wire を送らない)。`project_name_from_path` と違い **basename fallback しない**
/// — basename 衝突による silent-wrong-identity を避けるため。
pub(crate) fn match_project_name_for_path(
    normalized_path: &str,
    config: &Config,
) -> Option<String> {
    // 1. 完全一致
    if let Some(idx) = config.find_project_index(normalized_path) {
        return Some(config.projects[idx].name.clone());
    }

    // 2. longest-prefix サブディレクトリ一致 (最も具体的な path を優先)
    config
        .projects
        .iter()
        .filter(|p| {
            let normalized = Config::normalize_path(std::path::Path::new(&p.path));
            normalized_path.starts_with(&format!("{}/", normalized))
        })
        .max_by_key(|p| Config::normalize_path(std::path::Path::new(&p.path)).len())
        .map(|p| p.name.clone())
}

// doc 44 P1 PR4 (DB 統合): `project_slug` / `fnv1a_64` を撤去。
//
// slug は project 名を **DB ディレクトリ名 `db/sp_{slug}/`** に落とすためだけに存在し
// （VP-165 / doc 17 決定B。旧永続化レイヤー退役後は db dir が唯一の用途だった）、`fnv1a_64` は
// その永続 key を Rust バージョン間で安定させるための決定的 hash だった。
// DB 統合でディレクトリ分離が消え、project 次元が table の `project_path` 列に移った結果、
// **slug という概念そのものが宙に浮いた**（production の呼び出し元が 0 になった）。

#[cfg(test)]
mod tests {
    use super::*;

    // --- match_project_name_for_path (wiremsg identity SSOT) ---

    fn cfg_with(projects: &[(&str, &str)]) -> Config {
        use crate::config::ProjectConfig;
        Config {
            projects: projects
                .iter()
                .map(|(name, path)| ProjectConfig {
                    name: name.to_string(),
                    path: path.to_string(),
                    port: None,
                    enabled: true,
                    slot: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn match_project_exact_root() {
        // 存在しない絶対 path は normalize_path が lexical fallback するので test 安定。
        let cfg = cfg_with(&[("club-unison", "/Users/x/repos/club-unison")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/club-unison", &cfg).as_deref(),
            Some("club-unison")
        );
    }

    #[test]
    fn match_project_longest_prefix_subdir() {
        // conductor は subdir から動く → longest-prefix で親 project を引く
        let cfg = cfg_with(&[("club-unison", "/Users/x/repos/club-unison")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/club-unison/clients/typescript", &cfg)
                .as_deref(),
            Some("club-unison")
        );
    }

    #[test]
    fn match_project_picks_most_specific() {
        // nested 登録: より深い (具体的) path を優先
        let cfg = cfg_with(&[
            ("outer", "/Users/x/repos/outer"),
            ("inner", "/Users/x/repos/outer/inner"),
        ]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/outer/inner/sub", &cfg).as_deref(),
            Some("inner")
        );
    }

    #[test]
    fn match_project_unregistered_is_none() {
        // 未登録 cwd → None (= conductor fail-closed)。sibling 名前共有 (basename 衝突) でも誤マッチしない
        let cfg = cfg_with(&[("vp", "/Users/x/repos/vp")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/other", &cfg),
            None
        );
        // basename だけ一致する別 root は prefix マッチしない (basename fallback しない証明)
        assert_eq!(match_project_name_for_path("/tmp/elsewhere/vp", &cfg), None);
    }

    #[test]
    fn match_project_uses_config_name_not_basename() {
        // name != basename (vp projects rename 済) でも config 名を返す
        let cfg = cfg_with(&[("renamed-name", "/Users/x/repos/orig-dir")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/orig-dir", &cfg).as_deref(),
            Some("renamed-name")
        );
    }
}
