//! TmuxActor — tmux ペイン管理の Actor
//!
//! Process 内で tokio タスクとして動作し、tmux のペイン状態を管理する。
//! 外部からは TmuxHandle 経由でコマンドを送信し、結果を受け取る。
//!
//! ```text
//! TUI / MCP
//!   │ (Unison "process" channel)
//!   ▼
//! AppState.tmux_handle
//!   │ (mpsc::Sender<TmuxCommand>)
//!   ▼
//! TmuxActor (tokio task)
//!   │ (std::process::Command)
//!   ▼
//! tmux CLI
//! ```

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// tmux ペイン情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxPane {
    pub id: String,
    pub active: bool,
    pub width: u32,
    pub height: u32,
    pub command: String,
}

/// Actor へのコマンド
enum TmuxCommand {
    /// ペイン分割
    Split {
        horizontal: bool,
        command: Option<String>,
        reply: oneshot::Sender<Result<TmuxPane, String>>,
    },
    /// ペイン一覧取得
    List {
        reply: oneshot::Sender<Vec<TmuxPane>>,
    },
    /// ペイン閉鎖
    Close {
        pane_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 状態をリフレッシュ（tmux から再取得）
    Refresh {
        reply: oneshot::Sender<Vec<TmuxPane>>,
    },
}

/// Actor への外部インターフェース（Clone 可能）
#[derive(Clone)]
pub struct TmuxHandle {
    tx: mpsc::Sender<TmuxCommand>,
}

impl TmuxHandle {
    /// ペインを分割して新しいペイン情報を返す
    pub async fn split(
        &self,
        horizontal: bool,
        command: Option<String>,
    ) -> Result<TmuxPane, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(TmuxCommand::Split {
                horizontal,
                command,
                reply,
            })
            .await
            .map_err(|_| "TmuxActor stopped".to_string())?;
        rx.await
            .map_err(|_| "TmuxActor reply dropped".to_string())?
    }

    /// 現在のペイン一覧を返す（キャッシュ済み状態）
    pub async fn list(&self) -> Vec<TmuxPane> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(TmuxCommand::List { reply }).await.is_err() {
            return vec![];
        }
        rx.await.unwrap_or_default()
    }

    /// 指定ペインを閉じる
    pub async fn close(&self, pane_id: &str) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(TmuxCommand::Close {
                pane_id: pane_id.to_string(),
                reply,
            })
            .await
            .map_err(|_| "TmuxActor stopped".to_string())?;
        rx.await
            .map_err(|_| "TmuxActor reply dropped".to_string())?
    }

    /// tmux から状態を再取得してキャッシュを更新
    pub async fn refresh(&self) -> Vec<TmuxPane> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(TmuxCommand::Refresh { reply }).await.is_err() {
            return vec![];
        }
        rx.await.unwrap_or_default()
    }
}

/// TmuxActor の内部状態
struct TmuxActor {
    session_name: String,
    panes: Vec<TmuxPane>,
    rx: mpsc::Receiver<TmuxCommand>,
}

impl TmuxActor {
    /// Actor メインループ
    async fn run(mut self) {
        // 初期状態を tmux から取得
        let session = self.session_name.clone();
        self.panes = tokio::task::spawn_blocking(move || Self::query_panes(&session))
            .await
            .unwrap_or_default();
        tracing::info!(
            "TmuxActor 起動: session={}, panes={}",
            self.session_name,
            self.panes.len()
        );

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                TmuxCommand::Split {
                    horizontal,
                    command,
                    reply,
                } => {
                    let session = self.session_name.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        Self::do_split(&session, horizontal, command.as_deref())
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {}", e)));

                    if let Ok(ref pane) = result {
                        tracing::info!(
                            "tmux ペイン作成: {} (session={})",
                            pane.id,
                            self.session_name
                        );
                    }
                    // 状態を更新
                    let session = self.session_name.clone();
                    self.panes = tokio::task::spawn_blocking(move || Self::query_panes(&session))
                        .await
                        .unwrap_or_default();
                    let _ = reply.send(result);
                }
                TmuxCommand::List { reply } => {
                    let _ = reply.send(self.panes.clone());
                }
                TmuxCommand::Close { pane_id, reply } => {
                    let pid = pane_id.clone();
                    let result = tokio::task::spawn_blocking(move || Self::do_close(&pid))
                        .await
                        .unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {}", e)));

                    if result.is_ok() {
                        tracing::info!("tmux ペイン閉鎖: {}", pane_id);
                    }
                    // 状態を更新
                    let session = self.session_name.clone();
                    self.panes = tokio::task::spawn_blocking(move || Self::query_panes(&session))
                        .await
                        .unwrap_or_default();
                    let _ = reply.send(result);
                }
                TmuxCommand::Refresh { reply } => {
                    let session = self.session_name.clone();
                    self.panes = tokio::task::spawn_blocking(move || Self::query_panes(&session))
                        .await
                        .unwrap_or_default();
                    let _ = reply.send(self.panes.clone());
                }
            }
        }

        tracing::info!("TmuxActor 終了: session={}", self.session_name);
    }

    /// ペイン分割を実行（ブロッキング — spawn_blocking 内で呼ぶ）
    fn do_split(
        session_name: &str,
        horizontal: bool,
        command: Option<&str>,
    ) -> Result<TmuxPane, String> {
        let flag = if horizontal { "-v" } else { "-h" };
        let format_str =
            "#{pane_id}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_current_command}";
        let mut args = vec![
            "split-window",
            flag,
            "-t",
            session_name,
            "-P",
            "-F",
            format_str,
        ];
        if let Some(cmd) = command {
            args.push(cmd);
        }

        let output = std::process::Command::new("tmux")
            .args(&args)
            .output()
            .map_err(|e| format!("tmux split-window 失敗: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tmux split-window エラー: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_pane_line(stdout.trim())
            .ok_or_else(|| "新しいペインの情報を解析できません".to_string())
    }

    /// ペインを閉じる（ブロッキング — spawn_blocking 内で呼ぶ）
    fn do_close(pane_id: &str) -> Result<(), String> {
        let status = std::process::Command::new("tmux")
            .args(["kill-pane", "-t", pane_id])
            .status()
            .map_err(|e| format!("tmux kill-pane 失敗: {}", e))?;

        if !status.success() {
            return Err(format!("tmux kill-pane エラー: pane_id={}", pane_id));
        }
        Ok(())
    }

    /// tmux list-panes でペイン一覧を取得（ブロッキング — spawn_blocking 内で呼ぶ）
    fn query_panes(session_name: &str) -> Vec<TmuxPane> {
        let output = std::process::Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                session_name,
                "-F",
                "#{pane_id}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_current_command}",
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(Self::parse_pane_line)
                .collect(),
            _ => vec![],
        }
    }

    /// タブ区切りの1行をパース
    fn parse_pane_line(line: &str) -> Option<TmuxPane> {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 5 {
            Some(TmuxPane {
                id: parts[0].to_string(),
                active: parts[1] == "1",
                width: parts[2].parse().unwrap_or(0),
                height: parts[3].parse().unwrap_or(0),
                command: parts[4].to_string(),
            })
        } else {
            None
        }
    }
}

/// TmuxActor を起動し、Handle を返す
///
/// tmux 未使用環境では None を返す。
pub fn spawn(session_name: &str) -> Option<TmuxHandle> {
    if !crate::tmux::is_inside_tmux() {
        return None;
    }

    let (tx, rx) = mpsc::channel(32);

    let actor = TmuxActor {
        session_name: session_name.to_string(),
        panes: vec![],
        rx,
    };

    tokio::spawn(actor.run());

    Some(TmuxHandle { tx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pane_line() {
        let line = "%0\t1\t120\t40\tzsh";
        let pane = TmuxActor::parse_pane_line(line).unwrap();
        assert_eq!(pane.id, "%0");
        assert!(pane.active);
        assert_eq!(pane.width, 120);
        assert_eq!(pane.height, 40);
        assert_eq!(pane.command, "zsh");
    }

    #[test]
    fn test_parse_pane_line_invalid() {
        assert!(TmuxActor::parse_pane_line("invalid").is_none());
        assert!(TmuxActor::parse_pane_line("").is_none());
    }

    #[test]
    fn test_parse_pane_line_inactive() {
        let line = "%3\t0\t80\t24\tclaude";
        let pane = TmuxActor::parse_pane_line(line).unwrap();
        assert_eq!(pane.id, "%3");
        assert!(!pane.active);
        assert_eq!(pane.command, "claude");
    }
}
