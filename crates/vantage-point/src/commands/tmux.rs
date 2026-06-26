//! `vp tmux` サブコマンド
//!
//! tmux ペイン操作を CLI から実行する。
//! L0 portless: World process-proxy ask (`tmux_*`) 経由で SP の TmuxActor を操作する
//! (旧 SP HTTP `/api/tmux/*` 直結は撤去)。 split / dashboard / status / deploy は MCP tool
//! (`tmux_split` / `tmux_dashboard` / `tmux_agent_deploy`) と重複のため CLI からは撤去済。

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::commands::process_client::{
    resolve_pane_via_world, resolve_project_path_from_target, world_process_request_blocking,
};
use crate::config::Config;

/// tmux サブコマンド
#[derive(Subcommand)]
pub enum TmuxCommands {
    /// ペイン内容をキャプチャ
    Capture {
        /// キャプチャ対象のペイン ID またはエージェント label（省略で全ペイン）
        #[arg(long)]
        pane: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインにキー入力を送信
    SendKeys {
        /// 送信先ペイン ID またはエージェント label
        pane: String,
        /// 送信するテキスト
        text: String,
        /// 末尾に Enter を付与しない
        #[arg(long)]
        no_enter: bool,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
    /// ペインを閉じる
    Close {
        /// 閉じるペイン ID またはエージェント label
        pane: String,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
    },
}

/// レスポンスの "error" フィールドをチェックし、エラーなら bail する
fn check_error(resp: &serde_json::Value) -> Result<()> {
    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        bail!("tmux エラー: {}", err);
    }
    Ok(())
}

/// `vp tmux` を実行
pub fn execute(cmd: TmuxCommands, config: &Config) -> Result<()> {
    match cmd {
        TmuxCommands::Capture { pane, target } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            // 単一ペイン (pane 指定) と全ペイン (省略) で dispatch method が分かれる:
            // `tmux_capture` は pane_id 必須、 `tmux_capture_all` は全 pane。
            match &pane {
                Some(q) => {
                    let (pane_id, display) = resolve_pane_via_world(&project_path, q)?;
                    let resp = world_process_request_blocking(
                        crate::cli::WORLD_PORT,
                        &project_path,
                        "tmux_capture",
                        serde_json::json!({ "pane_id": pane_id }),
                    )?;
                    check_error(&resp)?;
                    if let Some(content) = resp.get("content").and_then(|v| v.as_str()) {
                        println!("=== Pane {} ===", display);
                        println!("{}", content);
                    }
                }
                None => {
                    let resp = world_process_request_blocking(
                        crate::cli::WORLD_PORT,
                        &project_path,
                        "tmux_capture_all",
                        serde_json::json!({}),
                    )?;
                    check_error(&resp)?;
                    // PaneCapture: { pane: TmuxPane, content, agent? }
                    if let Some(captures) = resp.get("captures").and_then(|v| v.as_array()) {
                        for cap in captures {
                            let pane_id = cap
                                .pointer("/pane/id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let cmd_str = cap
                                .pointer("/pane/command")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let label = cap.pointer("/agent/label").and_then(|v| v.as_str());
                            let content = cap.get("content").and_then(|v| v.as_str()).unwrap_or("");
                            let header = match label {
                                Some(l) => format!("=== {} ({}) [{}] ===", l, pane_id, cmd_str),
                                None => format!("=== Pane {} ({}) ===", pane_id, cmd_str),
                            };
                            println!("{}", header);
                            println!("{}", content);
                            println!();
                        }
                    }
                }
            }
            Ok(())
        }

        TmuxCommands::SendKeys {
            pane,
            text,
            no_enter,
            target,
        } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let (pane_id, display) = resolve_pane_via_world(&project_path, &pane)?;
            let resp = world_process_request_blocking(
                crate::cli::WORLD_PORT,
                &project_path,
                "tmux_send_keys",
                serde_json::json!({ "pane_id": pane_id, "text": text, "enter": !no_enter }),
            )?;
            check_error(&resp)?;
            println!("送信完了: {} → {}", display, text);
            Ok(())
        }

        TmuxCommands::Close { pane, target } => {
            let project_path = resolve_project_path_from_target(target.as_deref(), config)?;
            let (pane_id, display) = resolve_pane_via_world(&project_path, &pane)?;
            let resp = world_process_request_blocking(
                crate::cli::WORLD_PORT,
                &project_path,
                "tmux_close",
                serde_json::json!({ "pane_id": pane_id }),
            )?;
            check_error(&resp)?;
            println!("ペイン {} を閉じました", display);
            Ok(())
        }
    }
}
