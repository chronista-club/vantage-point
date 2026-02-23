//! `vp file` サブコマンド
//!
//! ファイル監視の開始/停止を CLI から実行する。

use anyhow::Result;
use clap::Subcommand;

use crate::commands::stand_client::StandClient;
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
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// ファイル監視を停止
    Unwatch {
        /// 監視を停止するペインID
        pane_id: String,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
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
            port,
        } => {
            let client = StandClient::connect(target.as_deref(), port, config)?;

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

            client.post("/api/watch-file", &watch_config)?;
            println!("Watching '{}' → pane '{}'", path, pane_id);
            Ok(())
        }
        FileCommands::Unwatch {
            pane_id,
            target,
            port,
        } => {
            let client = StandClient::connect(target.as_deref(), port, config)?;
            client.post(
                "/api/unwatch-file",
                &serde_json::json!({"pane_id": pane_id}),
            )?;
            println!("Stopped watching pane '{}'", pane_id);
            Ok(())
        }
    }
}
