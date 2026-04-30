//! `vp tmux` サブコマンド
//!
//! tmux ペイン操作を CLI から実行する。
//! Process の HTTP API (`/api/tmux/*`) を経由して TmuxActor にコマンドを送信する。

use anyhow::{Result, bail};
use clap::Subcommand;

use crate::commands::process_client::ProcessClient;
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
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// ペインを分割
    Split {
        /// 水平分割（デフォルト: 垂直）
        #[arg(long, short = 'h')]
        horizontal: bool,
        /// 分割先で実行するコマンド
        #[arg(long, short)]
        command: Option<String>,
        /// コンテンツタイプ（agent, shell, canvas）
        #[arg(long)]
        content_type: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
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
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// ペイン一覧（ダッシュボード表示）
    Dashboard {
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// エージェントステータス確認
    Status {
        /// 対象ペイン ID またはエージェント label（省略で全ペイン）
        #[arg(long)]
        pane: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// エージェントをペインにデプロイ
    Deploy {
        /// 実行するコマンド
        command: String,
        // TODO: --label でエージェントメタデータ設定（/api/tmux/set-agent-meta 追加後に実装）
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// ペインを閉じる
    Close {
        /// 閉じるペイン ID またはエージェント label
        pane: String,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
}

/// HTTP レスポンスの "error" フィールドをチェックし、エラーなら bail する
fn check_error(resp: &serde_json::Value) -> Result<()> {
    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        bail!("tmux エラー: {}", err);
    }
    Ok(())
}

pub fn execute(cmd: TmuxCommands, config: &Config) -> Result<()> {
    match cmd {
        TmuxCommands::Capture { pane, target, port } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;
            // label → (pane_id, display) 解決
            let resolved = match &pane {
                Some(q) => Some(client.resolve_pane(q)?),
                None => None,
            };
            let resolved_pane = resolved.as_ref().map(|(id, _)| id.as_str());
            let resp = client.post(
                "/api/tmux/capture",
                &serde_json::json!({ "pane_id": resolved_pane }),
            )?;
            check_error(&resp)?;

            // 単一ペインの場合
            if let Some(content) = resp.get("content").and_then(|v| v.as_str()) {
                let display = resolved.as_ref().map(|(_, d)| d.as_str()).unwrap_or("?");
                println!("=== Pane {} ===", display);
                println!("{}", content);
                return Ok(());
            }

            // 全ペインの場合（PaneCapture: { pane: TmuxPane, content, agent? }）
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
            Ok(())
        }

        TmuxCommands::Split {
            horizontal,
            command,
            content_type,
            target,
            port,
        } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;
            let resp = client.post(
                "/api/tmux/split",
                &serde_json::json!({
                    "horizontal": horizontal,
                    "command": command,
                    "content_type": content_type,
                }),
            )?;
            check_error(&resp)?;

            if let Some(pane) = resp.get("pane") {
                let id = pane.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                println!("ペイン作成: {}", id);
            }
            Ok(())
        }

        TmuxCommands::SendKeys {
            pane,
            text,
            no_enter,
            target,
            port,
        } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;
            let (pane_id, display) = client.resolve_pane(&pane)?;
            let resp = client.post(
                "/api/tmux/send-keys",
                &serde_json::json!({
                    "pane_id": pane_id,
                    "text": text,
                    "enter": !no_enter,
                }),
            )?;
            check_error(&resp)?;
            println!("送信完了: {} → {}", display, text);
            Ok(())
        }

        TmuxCommands::Dashboard { target, port } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;

            // ペイン一覧取得
            let list_resp = client.get("/api/tmux/list")?;
            let panes = list_resp
                .get("panes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            if panes.is_empty() {
                println!("tmux ペインなし");
                return Ok(());
            }

            println!("┌──────┬──────────────────┬─────────┬─────┬──────────────────────┐");
            println!(
                "│ {:>4} │ {:<16} │ {:^7} │ {:^3} │ {:<20} │",
                "ID", "Label", "Size", "Act", "Command"
            );
            println!("├──────┼──────────────────┼─────────┼─────┼──────────────────────┤");

            for pane in &panes {
                let id = pane.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let w = pane.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
                let h = pane.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
                let active = pane
                    .get("active")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let cmd = pane.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let active_mark = if active { "*" } else { " " };
                let size = format!("{}x{}", w, h);
                // エージェント label を list レスポンスから取得（N+1 回避）
                let label = pane
                    .pointer("/agent/label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_string();
                // label が長い場合は切り詰め（マルチバイト安全）
                let label_display = if label.chars().count() > 16 {
                    let truncated: String = label.chars().take(15).collect();
                    format!("{}…", truncated)
                } else {
                    label
                };
                println!(
                    "│ {:>4} │ {:<16} │ {:>7} │  {}  │ {:<20} │",
                    id, label_display, size, active_mark, cmd
                );
            }
            println!("└──────┴──────────────────┴─────────┴─────┴──────────────────────┘");
            println!("  port {}", client.port());

            // エージェントメタデータを表示（list レスポンスの agent フィールドから取得、N+1 回避）
            let mut has_agents = false;
            for pane in &panes {
                if let Some(agent) = pane.get("agent")
                    && !agent.is_null()
                {
                    if !has_agents {
                        println!("\n  Agents:");
                        has_agents = true;
                    }
                    let id = pane.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let label = agent
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no label)");
                    let status = agent
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let task = agent.get("task").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  {} [{}] {} — {}", id, status, label, task);
                }
            }
            Ok(())
        }

        TmuxCommands::Status { pane, target, port } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;
            let list_resp = client.get("/api/tmux/list")?;
            let panes = list_resp
                .get("panes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // label → pane_id 解決
            let resolved = match &pane {
                Some(q) => Some(client.resolve_pane(q)?),
                None => None,
            };

            // フィルタ対象を決定
            let target_panes: Vec<&serde_json::Value> = match &resolved {
                Some((id, _)) => panes
                    .iter()
                    .filter(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
                    .collect(),
                None => panes.iter().collect(),
            };

            for p in target_panes {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let cmd = p.get("command").and_then(|v| v.as_str()).unwrap_or("");

                // list レスポンスの agent フィールドから取得（N+1 回避）
                let meta_info = if let Some(agent) = p.get("agent") {
                    if !agent.is_null() {
                        let label = agent
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(no label)");
                        let status = agent
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let task = agent.get("task").and_then(|v| v.as_str()).unwrap_or("");
                        format!("[{}] {} — {}", status, label, task)
                    } else {
                        "(no agent)".to_string()
                    }
                } else {
                    "(no agent)".to_string()
                };

                println!("{} ({}) {}", id, cmd, meta_info);
            }
            Ok(())
        }

        TmuxCommands::Deploy {
            command,
            target,
            port,
        } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;

            // ペイン分割してコマンド実行
            let split_resp = client.post(
                "/api/tmux/split",
                &serde_json::json!({
                    "horizontal": true,
                    "command": command,
                }),
            )?;
            check_error(&split_resp)?;

            let pane_id = split_resp
                .pointer("/pane/id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");

            println!("デプロイ: {} → {}", pane_id, command);
            Ok(())
        }

        TmuxCommands::Close { pane, target, port } => {
            let client = ProcessClient::connect(target.as_deref(), port, config)?;
            let (pane_id, display) = client.resolve_pane(&pane)?;
            let resp = client.post(
                "/api/tmux/close",
                &serde_json::json!({ "pane_id": pane_id }),
            )?;
            check_error(&resp)?;
            println!("ペイン {} を閉じました", display);
            Ok(())
        }
    }
}
