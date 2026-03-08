//! PTYセッション管理モジュール
//!
//! portable-pty を使って複数の PTY セッションを管理する。
//! 各セッションは独自の broadcast チャネルを持ち、出力を分離する。

use std::collections::HashMap;
use std::io::{Read, Write};

use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::protocol::ProcessMessage;

/// セッション ID
pub type SessionId = String;

/// セッション情報（外部公開用）
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub command: String,
    pub cols: u16,
    pub rows: u16,
}

/// PTYセッション（内部管理用）
struct ManagedSession {
    session: PtySession,
    /// セッション固有の PTY 出力 broadcast
    tx: broadcast::Sender<ProcessMessage>,
    info: SessionInfo,
}

/// PTYセッション
pub struct PtySession {
    /// PTY への書き込みハンドル
    writer: Box<dyn Write + Send>,
    /// PTY ペア（リサイズ用に保持）
    pair: portable_pty::PtyPair,
}

impl PtySession {
    /// シェルプロセスを起動してPTYセッションを開始
    pub fn spawn(cwd: &str, cols: u16, rows: u16) -> Result<(Self, Box<dyn Read + Send>)> {
        Self::spawn_command(cwd, cols, rows, None)
    }

    /// 指定コマンドで PTY を起動（None ならデフォルトシェル）
    pub fn spawn_command(
        cwd: &str,
        cols: u16,
        rows: u16,
        command: Option<&[&str]>,
    ) -> Result<(Self, Box<dyn Read + Send>)> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = if let Some(args) = command {
            let mut c = CommandBuilder::new(args[0]);
            for arg in &args[1..] {
                c.arg(arg);
            }
            c
        } else {
            // $SHELL からシェルを取得、フォールバックは /bin/zsh
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let mut c = CommandBuilder::new(&shell);
            c.arg("-l");
            c
        };

        cmd.cwd(cwd);
        // CLAUDECODE が残ると cc がネスト検出で起動拒否する
        cmd.env_remove("CLAUDECODE");

        let _child = pair.slave.spawn_command(cmd)?;

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        Ok((Self { writer, pair }, reader))
    }

    /// PTY に入力を書き込む
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// PTY をリサイズする
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }
}

/// PTY出力を broadcast するタスクを開始
pub fn start_pty_reader_task(
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<ProcessMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    tracing::info!("PTY reader: EOF — 子プロセス終了");
                    // 終了通知を送信（サーバー側で session_ended を発火させる）
                    let _ = tx.send(ProcessMessage::TerminalExited);
                    break;
                }
                Ok(n) => {
                    let encoded = engine.encode(&buf[..n]);
                    match tx.send(ProcessMessage::TerminalOutput { data: encoded }) {
                        Ok(_) => {}
                        Err(broadcast::error::SendError(_)) => {
                            tracing::debug!("PTY broadcast: no receivers");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("PTY reader error: {}", e);
                    break;
                }
            }
        }
    })
}

/// 複数 PTY セッションマネージャー
pub struct PtyManager {
    sessions: HashMap<SessionId, ManagedSession>,
    counter: u32,
    /// デフォルトの作業ディレクトリ
    project_dir: String,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            counter: 0,
            project_dir: String::new(),
        }
    }

    /// プロジェクトディレクトリを設定
    pub fn set_project_dir(&mut self, dir: &str) {
        self.project_dir = dir.to_string();
    }

    /// 新しいセッション ID を生成
    fn next_id(&mut self) -> SessionId {
        self.counter += 1;
        format!("s-{:04}", self.counter)
    }

    /// セッションを作成して PTY を起動
    ///
    /// command が None ならデフォルトシェル、Some ならそのコマンドを実行。
    /// 返り値: (session_id, broadcast::Receiver) — Receiver でこのセッションの出力を購読。
    pub fn create_session(
        &mut self,
        cols: u16,
        rows: u16,
        command: Option<&[&str]>,
    ) -> Result<(SessionId, broadcast::Sender<ProcessMessage>)> {
        let id = self.next_id();
        let cwd = &self.project_dir;

        let cmd_display = command
            .map(|c| c.join(" "))
            .unwrap_or_else(|| "$SHELL".to_string());

        let (session, reader) = PtySession::spawn_command(cwd, cols, rows, command)?;

        // セッション固有の broadcast チャネル
        let (tx, _) = broadcast::channel(10000);

        // TerminalReady を通知
        let _ = tx.send(ProcessMessage::TerminalReady);

        // リーダータスクを開始
        start_pty_reader_task(reader, tx.clone());

        let info = SessionInfo {
            id: id.clone(),
            command: cmd_display,
            cols,
            rows,
        };

        self.sessions.insert(
            id.clone(),
            ManagedSession {
                session,
                tx: tx.clone(),
                info,
            },
        );

        tracing::info!("PTY session created: {} ({}x{})", id, cols, rows);
        Ok((id, tx))
    }

    /// セッション一覧
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions.values().map(|s| s.info.clone()).collect()
    }

    /// セッションの broadcast sender を取得（出力購読用）
    pub fn get_session_tx(&self, id: &str) -> Option<broadcast::Sender<ProcessMessage>> {
        self.sessions.get(id).map(|s| s.tx.clone())
    }

    /// PTY に入力を書き込む
    pub fn write(&mut self, session_id: &str, data: &[u8]) -> Result<()> {
        if let Some(managed) = self.sessions.get_mut(session_id) {
            managed.session.write(data)?;
        }
        Ok(())
    }

    /// PTY をリサイズ
    pub fn resize(&mut self, session_id: &str, cols: u16, rows: u16) -> Result<()> {
        if let Some(managed) = self.sessions.get_mut(session_id) {
            managed.session.resize(cols, rows)?;
            managed.info.cols = cols;
            managed.info.rows = rows;
        }
        Ok(())
    }

    /// セッションを閉じる
    pub fn close_session(&mut self, session_id: &str) -> bool {
        self.sessions.remove(session_id).is_some()
    }

    /// セッションが存在するか
    pub fn has_session(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// いずれかのセッションが開始済みか（後方互換）
    pub fn is_active(&self) -> bool {
        !self.sessions.is_empty()
    }
}
