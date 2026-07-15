//! `vp pane` サブコマンド
//!
//! ペイン操作（コンテンツ表示・クリア・分割・閉じる・トグル）を CLI から実行する。

use anyhow::Result;
use clap::Subcommand;

use crate::commands::process_client::{
    resolve_project_path_from_target, world_process_request_blocking,
};
use crate::config::Config;
use crate::protocol::{Content, ProcessMessage, SplitDirection};

/// Pane サブコマンド
#[derive(Subcommand)]
pub enum PaneCommands {
    /// ペインにコンテンツを表示
    Show {
        /// 表示するコンテンツ
        content: String,
        /// コンテンツ形式: markdown（デフォルト）, html, log, url
        #[arg(long, short, default_value = "markdown")]
        format: String,
        /// 表示先ペインID（デフォルト: main）
        #[arg(long)]
        pane_id: Option<String>,
        /// 既存コンテンツに追記
        #[arg(long)]
        append: bool,
        /// ペインタブのタイトル
        #[arg(long)]
        title: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインをクリア
    Clear {
        /// クリアするペインID（デフォルト: main）
        #[arg(long)]
        pane_id: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインを分割
    Split {
        /// 分割方向: horizontal（デフォルト）, vertical
        #[arg(long, short, default_value = "horizontal")]
        direction: String,
        /// 分割元のペインID（デフォルト: main）
        #[arg(long)]
        source: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインを閉じる
    Close {
        /// 閉じるペインID
        pane_id: String,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// パネルの表示/非表示を切り替え
    Toggle {
        /// トグルするペインID（left, right）
        pane_id: String,
        /// 明示的に表示/非表示を指定
        #[arg(long)]
        visible: Option<bool>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
}

/// `vp pane` を実行
pub fn execute(cmd: PaneCommands, config: &Config) -> Result<()> {
    match cmd {
        PaneCommands::Show {
            content,
            format,
            pane_id,
            append,
            title,
            target,
        } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let pane_id = pane_id.unwrap_or_else(|| "main".to_string());

            let content_enum = match format.as_str() {
                "html" => Content::Html(content),
                "log" => Content::Log(content),
                "url" => Content::Url(content),
                _ => Content::Markdown(content),
            };

            let msg = ProcessMessage::Show {
                pane_id: pane_id.clone(),
                content: content_enum,
                append,
                title,
                // CLI 実行 cwd の Lane を stamp（performer lane dir からならその PP に届く）
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
                scope: None,
            };
            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "show",
                serde_json::to_value(&msg)?,
            )?;

            println!("Content displayed in pane '{}'", pane_id);
            Ok(())
        }
        PaneCommands::Clear { pane_id, target } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let pane_id = pane_id.unwrap_or_else(|| "main".to_string());

            let msg = ProcessMessage::Clear {
                pane_id: pane_id.clone(),
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
                scope: None,
            };
            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "show",
                serde_json::to_value(&msg)?,
            )?;
            println!("Pane '{}' cleared", pane_id);
            Ok(())
        }
        PaneCommands::Split {
            direction,
            source,
            target,
        } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let source_pane_id = source.unwrap_or_else(|| "main".to_string());

            let dir = match direction.to_lowercase().as_str() {
                "vertical" | "v" => SplitDirection::Vertical,
                _ => SplitDirection::Horizontal,
            };

            let new_pane_id = uuid::Uuid::new_v4().to_string();
            let new_pane_id = new_pane_id.split('-').next().unwrap_or(&new_pane_id);
            let new_pane_id = format!("pane-{}", new_pane_id);

            let msg = ProcessMessage::Split {
                pane_id: source_pane_id.clone(),
                direction: dir,
                new_pane_id: new_pane_id.clone(),
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
            };
            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "split_pane",
                serde_json::to_value(&msg)?,
            )?;
            println!(
                "Pane '{}' split. New pane ID: '{}'",
                source_pane_id, new_pane_id
            );
            Ok(())
        }
        PaneCommands::Close { pane_id, target } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let msg = ProcessMessage::Close {
                pane_id: pane_id.clone(),
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
            };
            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "close_pane",
                serde_json::to_value(&msg)?,
            )?;
            println!("Pane '{}' closed", pane_id);
            Ok(())
        }
        PaneCommands::Toggle {
            pane_id,
            visible,
            target,
        } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let msg = ProcessMessage::TogglePane {
                pane_id: pane_id.clone(),
                visible,
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
            };
            world_process_request_blocking(
                crate::cli::world_port(),
                &project_path,
                "toggle_pane",
                serde_json::to_value(&msg)?,
            )?;

            let state = match visible {
                Some(true) => "shown",
                Some(false) => "hidden",
                None => "toggled",
            };
            println!("Pane '{}' {}", pane_id, state);
            Ok(())
        }
    }
}
