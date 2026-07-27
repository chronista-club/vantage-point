//! ターゲット解決モジュール
//!
//! 全コマンド共通のrepo解決ロジックを提供する。
//! CLI 引数（target）から実行対象のrepoを特定し、
//! ポート番号やrepo名を解決する。

use anyhow::{Result, bail};

use crate::config::Config;

/// ターゲット解決結果
pub enum ResolvedTarget {
    /// 実行中の Process
    Running {
        port: u16,
        name: String,
        repo_dir: String,
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
/// 2. 数値文字列 → repoインデックス（後方互換、1始まり）
/// 3. 文字列 → repo名検索
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

/// cwd からrepoを解決
fn resolve_from_cwd(config: &Config) -> Result<ResolvedTarget> {
    let cwd = std::env::current_dir()?;
    let cwd_str = Config::normalize_path(&cwd);

    // 1. 稼働中 Process を検索（daemon API → HTTP スキャンフォールバック）
    if let Some(running) = crate::discovery::find_for_cwd_blocking() {
        let name = repo_name_from_path(&running.repo_dir, config);
        return Ok(ResolvedTarget::Running {
            port: running.port,
            name,
            repo_dir: running.repo_dir,
        });
    }

    // 2. config でrepoを検索（完全一致）
    if let Some(idx) = config.find_repo_index(&cwd_str) {
        let repo = &config.repos[idx];
        return Ok(ResolvedTarget::Configured {
            name: repo.name.clone(),
            path: cwd_str,
            index: idx,
        });
    }

    // 3. config でサブディレクトリマッチ（最も具体的なパスを優先）
    let best_match = config
        .repos
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let normalized = Config::normalize_path(std::path::Path::new(&p.path));
            cwd_str.starts_with(&format!("{}/", normalized))
        })
        .max_by_key(|(_, p)| Config::normalize_path(std::path::Path::new(&p.path)).len());

    if let Some((idx, repo)) = best_match {
        let path = Config::normalize_path(std::path::Path::new(&repo.path));
        return Ok(ResolvedTarget::Configured {
            name: repo.name.clone(),
            path,
            index: idx,
        });
    }

    // 4. 未登録ディレクトリ
    Ok(ResolvedTarget::Cwd { path: cwd_str })
}

/// repoインデックスから解決（1始まり、後方互換）
fn resolve_by_index(index: usize, config: &Config) -> Result<ResolvedTarget> {
    if index == 0 || index > config.repos.len() {
        bail!(
            "Invalid repo index {}. Use `vp config` to list repos (1\u{2013}{}).",
            index,
            config.repos.len()
        );
    }

    let repo = &config.repos[index - 1];
    let path = Config::normalize_path(std::path::Path::new(&repo.path));

    // 実行中かチェック
    if let Some(running) = crate::discovery::find_by_repo_blocking(&path) {
        return Ok(ResolvedTarget::Running {
            port: running.port,
            name: repo.name.clone(),
            repo_dir: path,
        });
    }

    Ok(ResolvedTarget::Configured {
        name: repo.name.clone(),
        path,
        index: index - 1,
    })
}

/// repo名から解決
fn resolve_by_name(name: &str, config: &Config) -> Result<ResolvedTarget> {
    let found = config
        .repos
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == name);

    match found {
        Some((idx, repo)) => {
            let path = Config::normalize_path(std::path::Path::new(&repo.path));

            // 実行中かチェック
            if let Some(running) = crate::discovery::find_by_repo_blocking(&path) {
                return Ok(ResolvedTarget::Running {
                    port: running.port,
                    name: repo.name.clone(),
                    repo_dir: path,
                });
            }

            Ok(ResolvedTarget::Configured {
                name: repo.name.clone(),
                path,
                index: idx,
            })
        }
        None => bail!(
            "Repo '{}' not found. Use `vp config` to list registered repos.",
            name
        ),
    }
}

/// パスからrepo名を取得（config になければディレクトリ名）
pub fn repo_name_from_path(repo_dir: &str, config: &Config) -> String {
    for repo in &config.repos {
        let normalized = Config::normalize_path(std::path::Path::new(&repo.path));
        if normalized == repo_dir {
            return repo.name.clone();
        }
    }

    // ディレクトリ名をフォールバック
    std::path::Path::new(repo_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 正規化済み path から登録 repo 名を **repo 非依存 (config のみ)** に解決する純関数
/// (wire identity SSOT)。完全一致 → longest-prefix サブディレクトリ一致 → `None`。
///
/// `resolve_from_cwd` の config 経路だけを抜き出した純 config lookup。discovery / daemon を
/// 引かないので、自 repo の repo が落ちていても conductor の canonical address
/// (`agent@<repo>`) を確定できる。I/O なし (cwd は呼び出し側で正規化して渡す) で単体 test 可能。
///
/// 戻り値は登録 repo の `name` (= [`repo_name_from_path`] が登録 path に返すのと同じ値、
/// SSOT は `config.repos[].name`)。未登録 path は `None` → 呼び出し側で fail-closed する
/// (誤 identity で wire を送らない)。`repo_name_from_path` と違い **basename fallback しない**
/// — basename 衝突による silent-wrong-identity を避けるため。
pub(crate) fn match_repo_name_for_path(normalized_path: &str, config: &Config) -> Option<String> {
    // 1. 完全一致
    if let Some(idx) = config.find_repo_index(normalized_path) {
        return Some(config.repos[idx].name.clone());
    }

    // 2. longest-prefix サブディレクトリ一致 (最も具体的な path を優先)
    config
        .repos
        .iter()
        .filter(|p| {
            let normalized = Config::normalize_path(std::path::Path::new(&p.path));
            normalized_path.starts_with(&format!("{}/", normalized))
        })
        .max_by_key(|p| Config::normalize_path(std::path::Path::new(&p.path)).len())
        .map(|p| p.name.clone())
}

// doc 44 P1 PR4 (DB 統合): `repo_slug` / `fnv1a_64` を撤去。
//
// slug は repo 名を **DB ディレクトリ名 `db/sp_{slug}/`** に落とすためだけに存在し
// （VP-165 / doc 17 決定B。旧永続化レイヤー退役後は db dir が唯一の用途だった）、`fnv1a_64` は
// その永続 key を Rust バージョン間で安定させるための決定的 hash だった。
// DB 統合でディレクトリ分離が消え、repo 次元が table の `repo_path` 列に移った結果、
// **slug という概念そのものが宙に浮いた**（production の呼び出し元が 0 になった）。

#[cfg(test)]
mod tests {
    use super::*;

    // --- match_repo_name_for_path (wiremsg identity SSOT) ---

    fn cfg_with(repos: &[(&str, &str)]) -> Config {
        use crate::config::RepoConfig;
        Config {
            repos: repos
                .iter()
                .map(|(name, path)| RepoConfig {
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
    fn match_repo_exact_root() {
        // 存在しない絶対 path は normalize_path が lexical fallback するので test 安定。
        let cfg = cfg_with(&[("club-unison", "/Users/x/repos/club-unison")]);
        assert_eq!(
            match_repo_name_for_path("/Users/x/repos/club-unison", &cfg).as_deref(),
            Some("club-unison")
        );
    }

    #[test]
    fn match_repo_longest_prefix_subdir() {
        // conductor は subdir から動く → longest-prefix で親 repo を引く
        let cfg = cfg_with(&[("club-unison", "/Users/x/repos/club-unison")]);
        assert_eq!(
            match_repo_name_for_path("/Users/x/repos/club-unison/clients/typescript", &cfg)
                .as_deref(),
            Some("club-unison")
        );
    }

    #[test]
    fn match_repo_picks_most_specific() {
        // nested 登録: より深い (具体的) path を優先
        let cfg = cfg_with(&[
            ("outer", "/Users/x/repos/outer"),
            ("inner", "/Users/x/repos/outer/inner"),
        ]);
        assert_eq!(
            match_repo_name_for_path("/Users/x/repos/outer/inner/sub", &cfg).as_deref(),
            Some("inner")
        );
    }

    #[test]
    fn match_repo_unregistered_is_none() {
        // 未登録 cwd → None (= conductor fail-closed)。sibling 名前共有 (basename 衝突) でも誤マッチしない
        let cfg = cfg_with(&[("vp", "/Users/x/repos/vp")]);
        assert_eq!(match_repo_name_for_path("/Users/x/repos/other", &cfg), None);
        // basename だけ一致する別 root は prefix マッチしない (basename fallback しない証明)
        assert_eq!(match_repo_name_for_path("/tmp/elsewhere/vp", &cfg), None);
    }

    #[test]
    fn match_repo_uses_config_name_not_basename() {
        // name != basename (vp repos rename 済) でも config 名を返す
        let cfg = cfg_with(&[("renamed-name", "/Users/x/repos/orig-dir")]);
        assert_eq!(
            match_repo_name_for_path("/Users/x/repos/orig-dir", &cfg).as_deref(),
            Some("renamed-name")
        );
    }
}
