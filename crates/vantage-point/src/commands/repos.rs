//! `vp repos` subcommand — 登録 repo の管理（daemon に直接 Unison RPC）
//!
//! ## control plane 一元化 (creo `mem_1CbmWjCGNi9z49s3r21TwQ`)
//!
//! repos は daemon 権威 (db/machine) なので、 CLI は repos.kdl を直接書かず、 daemon の
//! "daemon-control" Unison channel に **直接** RPC する (= repo を経由しない、 tier 構造で
//! CLI=executor が Daemon=権威 に依頼)。 Daemon 不在なら操作できない (= repos 操作は
//! daemon 起動が前提、 既知の制約)。

use anyhow::Result;
use clap::Subcommand;

use crate::cli::daemon_port;
use crate::daemon::client::DaemonControlClient;

#[derive(Subcommand, Debug)]
pub enum ReposCommands {
    /// 登録 repo 一覧を表示
    #[command(alias = "ls")]
    List,
    /// repo を追加 (path は dir、 省略時は cwd)
    Add {
        /// 表示名
        name: String,
        /// repoディレクトリ (省略時は cwd)
        path: Option<String>,
    },
    /// repo を削除 (path で特定)
    #[command(alias = "rm")]
    Remove { path: String },
    /// repo 名を変更
    Rename { path: String, name: String },
    /// repo の repo 自動起動を有効化
    Enable { path: String },
    /// repo の repo 自動起動を無効化
    Disable { path: String },
    /// 並び順を変更 (path を順に列挙)
    Reorder { paths: Vec<String> },
    /// repo を起動する (旧 `vp sp start`)
    ///
    /// doc 44 P1 (fold-in): repo は daemon プロセス内で動くため、これは子プロセスの
    /// spawn ではなく daemon の registry への登録。既に起動済みなら no-op。
    Start {
        /// repo 名 (`vp repos list` の名前)
        name: String,
    },
    /// repo を停止する (旧 `vp sp stop`)
    Stop {
        /// repo 名
        name: String,
    },
}

/// `vp repos` のエントリポイント。 async (Unison client は async) なので
/// caller (main.rs) は per-command Runtime で `block_on` する。
pub async fn execute(cmd: ReposCommands) -> Result<()> {
    let client = DaemonControlClient::connect(daemon_port(), 3)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "daemon (port {}) に接続できません。 `vp daemon` で起動してください: {}",
                daemon_port(),
                e
            )
        })?;

    match cmd {
        ReposCommands::List => {
            let list = client.repos_list().await?;
            if list.is_empty() {
                println!("(登録 repo なし)");
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
        ReposCommands::Add { name, path } => {
            let path = resolve_dir(path)?;
            client.repos_add(&name, &path).await?;
            println!("追加: {} → {}", name, path);
            Ok(())
        }
        ReposCommands::Remove { path } => {
            client.repos_remove(&path).await?;
            println!("削除: {}", path);
            Ok(())
        }
        ReposCommands::Rename { path, name } => {
            client.repos_rename(&path, &name).await?;
            println!("名称変更: {} → {}", path, name);
            Ok(())
        }
        ReposCommands::Enable { path } => {
            client.repos_set_enabled(&path, true).await?;
            println!("有効化: {}", path);
            Ok(())
        }
        ReposCommands::Disable { path } => {
            client.repos_set_enabled(&path, false).await?;
            println!("無効化: {}", path);
            Ok(())
        }
        ReposCommands::Reorder { paths } => {
            client.repos_reorder(&paths).await?;
            println!("並び替え: {} 件", paths.len());
            Ok(())
        }
        ReposCommands::Start { name } => {
            let resp = client.repos_start(&name).await?;
            let path = resp.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            println!("起動: {} ({})", name, path);
            Ok(())
        }
        ReposCommands::Stop { name } => {
            client.repos_stop(&name).await?;
            println!("停止: {}", name);
            Ok(())
        }
    }
}

/// `path` (省略時 cwd) を絶対パスに解決する。 実在 dir なら canonicalize、 失敗時は
/// そのまま送って daemon 側の `is_dir` チェックにエラーを委ねる (= 二重バリデーション回避)。
///
/// canonicalize は `dunce` 経由。 std の方は Windows で `\\?\C:\...` (verbatim prefix) を返し、
/// それが repos.kdl と repo の spawn 引数まで伝播してしまう。
fn resolve_dir(path: Option<String>) -> Result<String> {
    let raw = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    Ok(dunce::canonicalize(&raw)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| raw.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `vp repos add` が repos.kdl に verbatim prefix 付き path を渡さない。
    /// Windows でのみ意味を持つ assertion (Mac/Linux では常に真)。
    #[test]
    fn resolve_dir_has_no_verbatim_prefix() {
        // cwd (実在 dir → canonicalize 成功パス)
        let cwd = resolve_dir(None).expect("cwd");
        assert!(!cwd.starts_with(r"\\?\"), "verbatim prefix が付いた: {cwd}");

        // 明示指定 (実在 dir)
        let explicit = resolve_dir(Some(".".to_string())).expect("explicit");
        assert!(
            !explicit.starts_with(r"\\?\"),
            "verbatim prefix が付いた: {explicit}"
        );
    }
}
