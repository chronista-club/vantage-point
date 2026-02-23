//! `vp canvas` サブコマンド
//!
//! Canvas ウィンドウの操作（open/close/capture）を CLI から実行する。

use anyhow::Result;
use clap::Subcommand;

use crate::commands::stand_client::StandClient;
use crate::config::Config;

/// Canvas サブコマンド
#[derive(Subcommand)]
pub enum CanvasCommands {
    /// Canvas ウィンドウを開く
    Open {
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// Canvas ウィンドウを閉じる
    Close {
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// Canvas のスクリーンショットを撮影
    Capture {
        /// 保存先パス（省略時は /tmp/vp-canvas-{timestamp}.png）
        #[arg(long)]
        path: Option<String>,
        /// 特定ペインのみキャプチャ
        #[arg(long)]
        pane_id: Option<String>,
        /// 接続先プロジェクト名またはインデックス
        #[arg(long)]
        target: Option<String>,
        /// 接続先ポート番号
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stand が内部的に spawn する Canvas プロセス（非表示）
    #[command(hide = true)]
    Internal {
        /// 接続先の Stand ポート番号
        #[arg(short, long)]
        port: u16,
    },
}

/// `vp canvas` を実行
pub fn execute(cmd: CanvasCommands, config: &Config) -> Result<()> {
    match cmd {
        CanvasCommands::Open { target, port } => {
            let client = StandClient::connect(target.as_deref(), port, config)?;
            client.post("/api/canvas/open", &serde_json::json!({}))?;
            println!("Canvas opened (port {})", client.port());
            Ok(())
        }
        CanvasCommands::Close { target, port } => {
            let client = StandClient::connect(target.as_deref(), port, config)?;
            client.post("/api/canvas/close", &serde_json::json!({}))?;
            println!("Canvas closed");
            Ok(())
        }
        CanvasCommands::Capture {
            path,
            pane_id,
            target,
            port,
        } => {
            let client = StandClient::connect(target.as_deref(), port, config)?;
            let resp = client.post(
                "/api/canvas/capture",
                &serde_json::json!({
                    "path": path,
                    "pane_id": pane_id,
                }),
            )?;

            let saved_path = resp
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let width = resp.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
            let height = resp.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
            let size_bytes = resp.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);

            println!(
                "Screenshot saved: {}\nSize: {}x{} ({} bytes)",
                saved_path, width, height, size_bytes
            );
            Ok(())
        }
        CanvasCommands::Internal { port } => crate::commands::canvas::execute(port),
    }
}
