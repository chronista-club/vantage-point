//! `vp pane` サブコマンド
//!
//! ペイン操作（コンテンツ表示・クリア・分割・閉じる・トグル）を CLI から実行する。

use anyhow::Result;
use clap::Subcommand;

use crate::commands::process_client::{
    daemon_repo_request_blocking, resolve_repo_path_from_target,
};
use crate::config::Config;
use crate::protocol::{Content, RepoMessage, SplitDirection};

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
        /// 接続先repo名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインをクリア
    Clear {
        /// クリアするペインID（デフォルト: main）
        #[arg(long)]
        pane_id: Option<String>,
        /// 接続先repo名またはインデックス
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
        /// 接続先repo名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインを閉じる
    Close {
        /// 閉じるペインID
        pane_id: String,
        /// 接続先repo名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// board の item を 1 枚消す（= GUI の thumbnail ✕ と同じ `board_delete_item`）。
    /// id は `vp pane show` の応答 `(id=…)` か MCP `read_board` で取る
    Delete {
        /// 消す board item の id
        item_id: String,
        /// 接続先repo名またはインデックス
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
        /// 接続先repo名またはインデックス
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
            let repo_path = resolve_repo_path_from_target(target.as_deref(), config)?;
            let pane_id = pane_id.unwrap_or_else(|| "main".to_string());

            let content_enum = match format.as_str() {
                "html" => Content::Html(content),
                "log" => Content::Log(content),
                "url" => Content::Url(content),
                _ => Content::Markdown(content),
            };

            let msg = RepoMessage::Show {
                pane_id: pane_id.clone(),
                content: content_enum,
                append,
                title,
                // CLI 実行 cwd の Lane を stamp（sub lane dir からならその board に届く）
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
                scope: None,
            };
            let res = daemon_repo_request_blocking(
                crate::cli::daemon_port(),
                &repo_path,
                "show",
                serde_json::to_value(&msg)?,
            )?;

            // 貼った item の id（repo 側が採番して返す）を出す — script / plugin の mod が
            // これを控えて MCP `update` で 1 枚を差し替える経路。
            match res.get("id").and_then(|v| v.as_str()) {
                Some(id) => println!("Content displayed in pane '{}' (id={})", pane_id, id),
                None => println!("Content displayed in pane '{}'", pane_id),
            }
            Ok(())
        }
        PaneCommands::Clear { pane_id, target } => {
            let repo_path = resolve_repo_path_from_target(target.as_deref(), config)?;
            let pane_id = pane_id.unwrap_or_else(|| "main".to_string());

            let msg = RepoMessage::Clear {
                pane_id: pane_id.clone(),
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
                scope: None,
            };
            daemon_repo_request_blocking(
                crate::cli::daemon_port(),
                &repo_path,
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
            let repo_path = resolve_repo_path_from_target(target.as_deref(), config)?;
            let source_pane_id = source.unwrap_or_else(|| "main".to_string());

            let dir = match direction.to_lowercase().as_str() {
                "vertical" | "v" => SplitDirection::Vertical,
                _ => SplitDirection::Horizontal,
            };

            let new_pane_id = uuid::Uuid::new_v4().to_string();
            let new_pane_id = new_pane_id.split('-').next().unwrap_or(&new_pane_id);
            let new_pane_id = format!("pane-{}", new_pane_id);

            let msg = RepoMessage::Split {
                pane_id: source_pane_id.clone(),
                direction: dir,
                new_pane_id: new_pane_id.clone(),
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
            };
            daemon_repo_request_blocking(
                crate::cli::daemon_port(),
                &repo_path,
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
            let repo_path = resolve_repo_path_from_target(target.as_deref(), config)?;
            let msg = RepoMessage::Close {
                pane_id: pane_id.clone(),
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
            };
            daemon_repo_request_blocking(
                crate::cli::daemon_port(),
                &repo_path,
                "close_pane",
                serde_json::to_value(&msg)?,
            )?;
            println!("Pane '{}' closed", pane_id);
            Ok(())
        }
        PaneCommands::Delete { item_id, target } => {
            let repo_path = resolve_repo_path_from_target(target.as_deref(), config)?;
            // payload は repo 側 `board::handle_board_delete_item` の形（item_id + lane。scope 省略 = lane board）。
            // lane は CLI 実行 cwd から stamp（`show` と同じ — sub lane dir からならその board）。
            let payload = serde_json::json!({
                "item_id": item_id,
                "lane": crate::mcp::SelfLane::detect().lane_name,
            });
            daemon_repo_request_blocking(
                crate::cli::daemon_port(),
                &repo_path,
                "board_delete_item",
                payload,
            )?;
            println!("Board item '{}' deleted", item_id);
            Ok(())
        }
        PaneCommands::Toggle {
            pane_id,
            visible,
            target,
        } => {
            let repo_path = resolve_repo_path_from_target(target.as_deref(), config)?;
            let msg = RepoMessage::TogglePane {
                pane_id: pane_id.clone(),
                visible,
                lane: Some(crate::mcp::SelfLane::detect().lane_name),
            };
            daemon_repo_request_blocking(
                crate::cli::daemon_port(),
                &repo_path,
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
