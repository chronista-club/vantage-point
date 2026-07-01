//! `vp projects` subcommand — 登録 project の管理（World daemon に直接 Unison RPC）
//!
//! ## control plane 一元化 (creo `mem_1CbmWjCGNi9z49s3r21TwQ`)
//!
//! projects は World 権威 (db/world) なので、 CLI は projects.kdl を直接書かず、 World daemon の
//! "world-control" Unison channel に **直接** RPC する (= SP を経由しない、 tier 構造で
//! CLI=executor が World=権威 に依頼)。 World 不在なら操作できない (= projects 操作は
//! daemon 起動が前提、 既知の制約)。

use anyhow::Result;
use clap::Subcommand;

use crate::cli::world_port;
use crate::daemon::client::WorldControlClient;

#[derive(Subcommand, Debug)]
pub enum ProjectsCommands {
    /// 登録 project 一覧を表示
    #[command(alias = "ls")]
    List,
    /// project を追加 (path は dir、 省略時は cwd)
    Add {
        /// 表示名
        name: String,
        /// プロジェクトディレクトリ (省略時は cwd)
        path: Option<String>,
    },
    /// project を削除 (path で特定)
    #[command(alias = "rm")]
    Remove { path: String },
    /// project 名を変更
    Rename { path: String, name: String },
    /// project の SP 自動起動を有効化
    Enable { path: String },
    /// project の SP 自動起動を無効化
    Disable { path: String },
    /// 並び順を変更 (path を順に列挙)
    Reorder { paths: Vec<String> },
}

/// `vp projects` のエントリポイント。 async (Unison client は async) なので
/// caller (main.rs) は per-command Runtime で `block_on` する。
pub async fn execute(cmd: ProjectsCommands) -> Result<()> {
    let client = WorldControlClient::connect(world_port(), 3)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "World daemon (port {}) に接続できません。 `vp daemon` で起動してください: {}",
                world_port(),
                e
            )
        })?;

    match cmd {
        ProjectsCommands::List => {
            let list = client.projects_list().await?;
            if list.is_empty() {
                println!("(登録 project なし)");
            } else {
                for p in &list {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let path = p.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    // enabled は省略時 true (= 有効)。
                    let enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    let status = p
                        .get("process_status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let mark = if enabled { "●" } else { "○" };
                    let run = if status.eq_ignore_ascii_case("running") {
                        " [running]"
                    } else {
                        ""
                    };
                    println!("{} {}{}  {}", mark, name, run, path);
                }
            }
            Ok(())
        }
        ProjectsCommands::Add { name, path } => {
            let path = resolve_dir(path)?;
            client.projects_add(&name, &path).await?;
            println!("追加: {} → {}", name, path);
            Ok(())
        }
        ProjectsCommands::Remove { path } => {
            client.projects_remove(&path).await?;
            println!("削除: {}", path);
            Ok(())
        }
        ProjectsCommands::Rename { path, name } => {
            client.projects_rename(&path, &name).await?;
            println!("名称変更: {} → {}", path, name);
            Ok(())
        }
        ProjectsCommands::Enable { path } => {
            client.projects_set_enabled(&path, true).await?;
            println!("有効化: {}", path);
            Ok(())
        }
        ProjectsCommands::Disable { path } => {
            client.projects_set_enabled(&path, false).await?;
            println!("無効化: {}", path);
            Ok(())
        }
        ProjectsCommands::Reorder { paths } => {
            client.projects_reorder(&paths).await?;
            println!("並び替え: {} 件", paths.len());
            Ok(())
        }
    }
}

/// `path` (省略時 cwd) を絶対パスに解決する。 実在 dir なら canonicalize、 失敗時は
/// そのまま送って World 側の `is_dir` チェックにエラーを委ねる (= 二重バリデーション回避)。
fn resolve_dir(path: Option<String>) -> Result<String> {
    let raw = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    Ok(std::fs::canonicalize(&raw)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| raw.to_string_lossy().to_string()))
}
