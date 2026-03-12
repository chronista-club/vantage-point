//! プロジェクトコンテキスト
//!
//! プロジェクトごとの独立した状態（PTY、ブリッジ、Canvas）を管理する。
//! マルチプロジェクト TUI の中核データ構造。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::terminal::state::TerminalState;

use super::bridge::{BridgeCommand, BridgeEvent, spawn_terminal_bridge};
use super::canvas_state::{CanvasState, spawn_canvas_receiver};
use super::session::SessionMode;

/// AI がアイドル状態とみなすまでの閾値
const IDLE_THRESHOLD: Duration = Duration::from_millis(800);

/// プロジェクトごとの独立した状態
pub struct ProjectContext {
    pub name: String,
    pub dir: String,
    pub port: u16,
    pub term_state: Arc<Mutex<TerminalState>>,
    pub cmd_tx: std::sync::mpsc::Sender<BridgeCommand>,
    event_rx: std::sync::mpsc::Receiver<BridgeEvent>,
    pub sessions: Vec<String>,
    pub current_session_idx: usize,
    pub canvas_state: Arc<Mutex<CanvasState>>,
    pub bridge_status: String,
    pub last_pty_output: Instant,
    /// 非アクティブ時の未読通知数
    pub notifications: u32,
    /// 接続切断フラグ
    pub disconnected: bool,
}

impl ProjectContext {
    /// 新しいプロジェクトコンテキストを作成し、ブリッジを起動する
    pub fn new(
        name: String,
        dir: String,
        port: u16,
        session_mode: SessionMode,
        pty_cols: usize,
        pty_lines: usize,
    ) -> Result<Self> {
        let term_state = Arc::new(Mutex::new(TerminalState::new(pty_cols, pty_lines)));

        // Health API から認証トークンを取得
        let terminal_token =
            crate::discovery::fetch_terminal_token_blocking(port).ok_or_else(|| {
                anyhow::anyhow!(
                    "Terminal token not found for port {}. Process may not be fully started.",
                    port
                )
            })?;

        // Unison ブリッジ起動
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<BridgeCommand>();
        let (event_tx, event_rx) = std::sync::mpsc::channel::<BridgeEvent>();
        spawn_terminal_bridge(port, terminal_token, cmd_rx, event_tx);

        // セッション作成リクエスト
        let claude_cmd = build_claude_command(&session_mode);
        let _ = cmd_tx.send(BridgeCommand::CreateSession {
            cols: pty_cols as u16,
            rows: pty_lines as u16,
            command: claude_cmd,
        });

        // Canvas レシーバー起動
        let canvas_state = Arc::new(Mutex::new(CanvasState::default()));
        let _canvas_handle = spawn_canvas_receiver(port, Arc::clone(&canvas_state));

        Ok(Self {
            name,
            dir,
            port,
            term_state,
            cmd_tx,
            event_rx,
            sessions: Vec::new(),
            current_session_idx: 0,
            canvas_state,
            bridge_status: "接続中...".to_string(),
            last_pty_output: Instant::now(),
            notifications: 0,
            disconnected: false,
        })
    }

    /// ブリッジイベントをポーリングして状態を更新。変更があれば true を返す。
    pub fn poll_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(evt) = self.event_rx.try_recv() {
            changed = true;
            match evt {
                BridgeEvent::Output(bytes) => {
                    let mut state = self.term_state.lock().unwrap();
                    state.feed_bytes(&bytes);
                    self.last_pty_output = Instant::now();
                }
                BridgeEvent::SessionCreated { session_id } => {
                    tracing::info!("TUI: セッション作成完了: {}", session_id);
                    self.sessions.push(session_id);
                    self.current_session_idx = self.sessions.len() - 1;
                    self.bridge_status = "接続済み".to_string();
                }
                BridgeEvent::SessionSwitched { session_id } => {
                    tracing::info!("TUI: セッション切替完了: {}", session_id);
                    if let Some(idx) = self.sessions.iter().position(|s| s == &session_id) {
                        self.current_session_idx = idx;
                    }
                    // 画面クリア
                    let mut state = self.term_state.lock().unwrap();
                    let cols = state.cols();
                    let rows = state.lines();
                    *state = TerminalState::new(cols, rows);
                }
                BridgeEvent::Error(e) => {
                    tracing::error!("TUI bridge error: {}", e);
                    self.bridge_status = format!("エラー: {}", e);
                }
                BridgeEvent::Disconnected => {
                    tracing::warn!("TUI: Process 接続切断 ({})", self.name);
                    self.disconnected = true;
                }
            }
        }
        changed
    }

    /// AI がビジー状態か
    pub fn is_ai_busy(&self) -> bool {
        self.last_pty_output.elapsed() < IDLE_THRESHOLD
    }

    /// PTY リサイズ
    pub fn resize(&self, cols: usize, lines: usize) {
        {
            let mut state = self.term_state.lock().unwrap();
            state.resize(cols, lines);
        }
        let _ = self.cmd_tx.send(BridgeCommand::Resize {
            cols: cols as u16,
            rows: lines as u16,
        });
    }
}

/// Claude CLI コマンドを構築
fn build_claude_command(session_mode: &SessionMode) -> Vec<String> {
    let mut cmd = vec![
        "claude".to_string(),
        "--dangerously-skip-permissions".to_string(),
    ];

    match session_mode {
        SessionMode::Continue => {
            cmd.push("--continue".to_string());
        }
        SessionMode::New => {}
        SessionMode::Resume(id) => {
            cmd.push("--resume".to_string());
            cmd.push(id.clone());
        }
    }

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_claude_command_continue() {
        let cmd = build_claude_command(&SessionMode::Continue);
        assert_eq!(
            cmd,
            vec!["claude", "--dangerously-skip-permissions", "--continue"]
        );
    }

    #[test]
    fn build_claude_command_new() {
        let cmd = build_claude_command(&SessionMode::New);
        assert_eq!(cmd, vec!["claude", "--dangerously-skip-permissions"]);
    }

    #[test]
    fn build_claude_command_resume() {
        let cmd = build_claude_command(&SessionMode::Resume("abc-123".to_string()));
        assert_eq!(
            cmd,
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--resume",
                "abc-123"
            ]
        );
    }
}
