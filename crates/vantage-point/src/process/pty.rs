//! PTYセッション管理モジュール
//!
//! portable-pty を使ってシェルセッションを管理し、
//! WebSocket経由でブラウザターミナルと接続する。

use std::io::{Read, Write};

use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::broadcast;

use crate::protocol::ProcessMessage;

/// PTYセッション
pub struct PtySession {
    /// PTY への書き込みハンドル
    writer: Box<dyn Write + Send>,
    /// PTY ペア（リサイズ用に保持）
    pair: portable_pty::PtyPair,
}

impl PtySession {
    /// シェルプロセスを起動してPTYセッションを開始
    ///
    /// # Arguments
    /// * `cwd` - 作業ディレクトリ
    /// * `cols` - 初期カラム数
    /// * `rows` - 初期行数
    pub fn spawn(cwd: &str, cols: u16, rows: u16) -> Result<(Self, Box<dyn Read + Send>)> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        // $SHELL からシェルを取得、フォールバックは /bin/zsh
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        // ログインシェルとして起動
        cmd.arg("-l");
        // ターミナルエミュレータとしてクリーンな環境を提供
        // CLAUDECODE が残ると cc がネスト検出で起動拒否する
        cmd.env_remove("CLAUDECODE");

        // 子プロセスを起動
        let _child = pair.slave.spawn_command(cmd)?;

        // マスター側の読み書きハンドル
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

/// PTY出力をWebSocket経由でブロードキャストするタスクを開始
///
/// PTYリーダーからバイナリを読み取り、base64エンコードして
/// ProcessMessage::TerminalOutput として配信する。
pub fn start_pty_reader_task(
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<ProcessMessage>,
) -> tokio::task::JoinHandle<()> {
    // PTY読み取りはブロッキングI/Oなので spawn_blocking を使用
    tokio::task::spawn_blocking(move || {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // PTYがクローズされた
                    tracing::info!("PTY reader: EOF");
                    break;
                }
                Ok(n) => {
                    let encoded = engine.encode(&buf[..n]);
                    match tx.send(ProcessMessage::TerminalOutput { data: encoded }) {
                        Ok(_) => {}
                        Err(broadcast::error::SendError(_)) => {
                            // 受信者がいない場合（正常：クライアント未接続時）
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

/// PTYセッションマネージャー（AppState用のラッパー）
pub struct PtyManager {
    session: Option<PtySession>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self { session: None }
    }

    /// PTYセッションを開始してリーダータスクを起動
    pub fn start(
        &mut self,
        cwd: &str,
        cols: u16,
        rows: u16,
        tx: broadcast::Sender<ProcessMessage>,
    ) -> Result<()> {
        let (session, reader) = PtySession::spawn(cwd, cols, rows)?;
        self.session = Some(session);

        // TerminalReady を通知
        let _ = tx.send(ProcessMessage::TerminalReady);

        // リーダータスクを開始
        start_pty_reader_task(reader, tx);

        tracing::info!("PTY session started ({}x{})", cols, rows);
        Ok(())
    }

    /// PTY に入力を書き込む
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut session) = self.session {
            session.write(data)?;
        }
        Ok(())
    }

    /// PTY をリサイズ
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if let Some(ref session) = self.session {
            session.resize(cols, rows)?;
        }
        Ok(())
    }

    /// セッションが開始済みか
    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }
}
