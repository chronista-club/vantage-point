//! Named persistent Lane links. Local file access is shared with vp-app; no live daemon required.
use crate::config::Config;
use clap::Subcommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Subcommand)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// 登録名で追加・更新。label 省略時は既存用途を保持、新規は name を用途にする
    Set {
        name: String,
        url: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// 永続登録されたリンク一覧（起動中サービス一覧ではない）
    List,
    /// 名前で削除。未登録なら何もしない
    Rm { name: String },
    /// 登録済み URL の接続状態を手動確認する
    Probe { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    pub repo: String,
    pub path: String,
    pub lane: String,
}

/// cwd is authoritative, not a possibly inherited VP_REPO/VP_LANE from another worktree.
pub fn resolve_target(
    config: &Config,
    cwd: &Path,
    requested: Option<&str>,
) -> Result<Target, String> {
    let cwd = Config::normalize_path(cwd);
    let current_repo = crate::resolve::match_repo_name_for_path(&cwd, config);
    let (repo_name, lane) = match requested {
        Some(value) => match value.split_once('/') {
            Some((repo, lane)) => (repo.to_string(), Some(lane.to_string())),
            None => (
                current_repo
                    .ok_or("現在の repo を確認できません。--lane repo/Lane で指定してください")?,
                Some(value.to_string()),
            ),
        },
        None => (
            current_repo.ok_or("現在の repo が未登録です。--lane repo/Lane で指定してください")?,
            None,
        ),
    };
    let repo = config
        .repos
        .iter()
        .find(|r| r.name == repo_name)
        .ok_or("指定 repo は登録されていません")?;
    let repo_path = Config::normalize_path(Path::new(&repo.path));
    let lane = lane.unwrap_or_else(|| {
        Path::new(&cwd)
            .strip_prefix(Path::new(&repo_path).join(".vp/lanes"))
            .ok()
            .and_then(|relative| relative.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_else(|| vp_paths::ROOT_LANE_NAME.into())
    });
    let lane = if lane == "root" {
        vp_paths::ROOT_LANE_NAME.to_string()
    } else {
        lane
    };
    if lane != vp_paths::ROOT_LANE_NAME {
        crate::lane::config::validate_existing_sub_name(&lane).map_err(|e| e.to_string())?;
        if !Path::new(&repo_path)
            .join(".vp/lanes")
            .join(&lane)
            .join(".git")
            .exists()
        {
            return Err(format!("Lane が見つかりません: {repo_name}/{lane}"));
        }
    }
    Ok(Target {
        repo: repo_name,
        path: repo_path,
        lane,
    })
}

pub async fn run(operation: Operation, lane: Option<&str>) -> Result<serde_json::Value, String> {
    let config = Config::load().map_err(|e| e.to_string())?;
    let target = resolve_target(
        &config,
        &std::env::current_dir().map_err(|e| e.to_string())?,
        lane,
    )?;
    execute(
        vp_paths::vp_state_dir().join("lane-local-urls.json"),
        target,
        operation,
    )
    .await
}

pub async fn execute(
    path: PathBuf,
    target: Target,
    operation: Operation,
) -> Result<serde_json::Value, String> {
    let scope = target.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        use vp_local_urls::{self as store, Action, Prepared};
        let (repo, lane) = (scope.path.as_str(), scope.lane.as_str());
        match operation {
            Operation::List => store::load(&path, repo, lane).map(Prepared::Entries),
            Operation::Set { name, url, label } => {
                store::set_named(&path, repo, lane, &name, &url, label.as_deref())
                    .map(Prepared::Entries)
            }
            Operation::Rm { name } => {
                store::remove_named(&path, repo, lane, &name).map(Prepared::Entries)
            }
            Operation::Probe { name } => {
                store::prepare(&path, repo, lane, Action::Probe { id: name })
            }
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut result = serde_json::to_value(target).map_err(|e| e.to_string())?;
    match prepared {
        vp_local_urls::Prepared::Entries(entries) => {
            result["entries"] = serde_json::to_value(entries).map_err(|e| e.to_string())?
        }
        vp_local_urls::Prepared::ProbeUrl(url) => {
            result["probe"] =
                serde_json::to_value(vp_local_urls::probe(&url).await).map_err(|e| e.to_string())?
        }
        vp_local_urls::Prepared::OpenUrl(_) => unreachable!("CLI/MCP have no open operation"),
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoConfig;
    #[test]
    fn resolves_nested_cwd_explicit_root_and_other_repo_but_rejects_unknown_lanes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(a.join(".vp/lanes/demo/.git")).unwrap();
        std::fs::create_dir_all(a.join(".vp/lanes/demo/src")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let config = Config {
            repos: vec![("a", &a), ("b", &b)]
                .into_iter()
                .map(|(name, path)| RepoConfig {
                    name: name.into(),
                    path: Config::normalize_path(path),
                    port: None,
                    enabled: true,
                    slot: None,
                })
                .collect(),
            ..Config::default()
        };
        let cwd = a.join(".vp/lanes/demo/src");
        assert_eq!(resolve_target(&config, &cwd, None).unwrap().lane, "demo");
        assert_eq!(
            resolve_target(&config, &cwd, Some("root")).unwrap().lane,
            vp_paths::ROOT_LANE_NAME
        );
        assert_eq!(
            resolve_target(&config, &cwd, Some("b/root")).unwrap().repo,
            "b"
        );
        assert!(resolve_target(&config, &cwd, Some("typo")).is_err());
        assert!(resolve_target(&config, &cwd, Some("../outside")).is_err());
        assert!(resolve_target(&config, dir.path(), None).is_err());
    }
}
