//! `vp file` サブコマンド
//!
//! ファイル監視の開始/停止を CLI から実行する。

use anyhow::Result;
use clap::Subcommand;

use crate::commands::process_client::{
    resolve_project_path_from_target, world_process_request_blocking,
};
use crate::config::Config;
use crate::file_watcher::{WatchConfig, WatchFormat, WatchStyle};

/// File サブコマンド
#[derive(Subcommand)]
pub enum FileCommands {
    /// ファイルを監視してペインにリアルタイム表示
    Watch {
        /// 監視するファイルパス
        path: String,
        /// 表示先ペインID
        pane_id: String,
        /// ログ形式: json_lines（デフォルト）, plain
        #[arg(long)]
        format: Option<String>,
        /// レベルフィルタ正規表現（例: "INFO|WARN|ERROR"）
        #[arg(long)]
        filter: Option<String>,
        /// ペインタブのタイトル
        #[arg(long)]
        title: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ファイル監視を停止
    Unwatch {
        /// 監視を停止するペインID
        pane_id: String,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
}

/// `vp file` を実行
pub fn execute(cmd: FileCommands, config: &Config) -> Result<()> {
    match cmd {
        FileCommands::Watch {
            path,
            pane_id,
            format,
            filter,
            title,
            target,
        } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;

            let watch_format = match format.as_deref() {
                Some("plain") => WatchFormat::Plain,
                _ => WatchFormat::JsonLines,
            };

            let watch_config = WatchConfig {
                path: path.clone(),
                pane_id: pane_id.clone(),
                format: watch_format,
                filter,
                exclude_targets: vec![],
                title,
                style: WatchStyle::Terminal,
            };

            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "watch_file",
                serde_json::to_value(&watch_config)?,
            )?;
            println!("Watching '{}' → pane '{}'", path, pane_id);
            Ok(())
        }
        FileCommands::Unwatch { pane_id, target } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "unwatch_file",
                serde_json::json!({ "pane_id": pane_id }),
            )?;
            println!("Stopped watching pane '{}'", pane_id);
            Ok(())
        }
    }
}
