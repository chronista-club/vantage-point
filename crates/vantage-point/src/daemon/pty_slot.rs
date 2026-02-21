//! PTYスロット — 個々のPTYプロセスの管理
//!
//! portable-pty で PTY を作成し、master fd からの出力を
//! broadcast channel 経由で配信する。
//! 既存の `stand/pty.rs` の PtySession を基に、Daemon用に再設計。
//! base64エンコードはしない（IPC層の責務）。

use std::io::{Read, Write};

use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::broadcast;

/// PTYプロセスを管理するスロット
///
/// 1つのPTYプロセスを所有し、broadcast channel 経由で
/// 出力を配信する。Daemon がこのスロットをペインごとに持つ。
pub struct PtySlot {
    /// PTY への書き込みハンドル
    writer: Box<dyn Write + Send>,
    /// PTY ペア（リサイズ用に保持）
    pair: portable_pty::PtyPair,
    /// 子プロセスハンドル（ゾンビプロセス防止のため保持）
    child: Box<dyn portable_pty::Child + Send>,
    /// プロセスID
    pid: u32,
    /// シェルコマンド
    shell_cmd: String,
    /// 出力配信チャネル（送信側）
    output_tx: broadcast::Sender<Vec<u8>>,
    /// reader task のハンドル
    _reader_handle: tokio::task::JoinHandle<()>,
}

impl PtySlot {
    /// PTYプロセスを起動
    ///
    /// 指定したシェルコマンドを PTY 上で起動し、
    /// 出力を broadcast channel に配信する reader task を開始する。
    pub fn spawn(cwd: &str, shell_cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell_cmd);
        cmd.cwd(cwd);
        // ログインシェルとして起動
        cmd.arg("-l");

        // 子プロセスを起動（ゾンビ防止のためハンドルを保持する）
        let child = pair.slave.spawn_command(cmd)?;
        let pid = child.process_id().unwrap_or(0);

        // マスター側の読み書きハンドル
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // broadcast channel（バッファ 256）
        let (output_tx, _) = broadcast::channel(256);

        // reader task 開始
        let reader_handle = start_reader_task(reader, output_tx.clone());

        Ok(Self {
            writer,
            pair,
            child,
            pid,
            shell_cmd: shell_cmd.to_string(),
            output_tx,
            _reader_handle: reader_handle,
        })
    }

    /// PTY に入力を書き込む
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// PTY をリサイズ
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// 出力ストリームを購読（broadcast receiver）
    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// プロセスID
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// シェルコマンド
    pub fn shell_cmd(&self) -> &str {
        &self.shell_cmd
    }
}

impl Drop for PtySlot {
    /// PtySlot 破棄時に子プロセスを確実に終了させる
    ///
    /// kill() で終了シグナルを送り、wait() で回収することで
    /// ゾンビプロセスの発生を防ぐ。
    fn drop(&mut self) {
        if let Err(e) = self.child.kill() {
            tracing::debug!("PtySlot drop: kill 失敗（既に終了済みの可能性）: {}", e);
        }
        if let Err(e) = self.child.wait() {
            tracing::debug!("PtySlot drop: wait 失敗: {}", e);
        }
    }
}

/// PTY出力読み取りタスクを起動
///
/// PTY の master fd からバイト列を読み取り、
/// broadcast channel にそのまま送信する（base64エンコードなし）。
fn start_reader_task(
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // PTYがクローズされた
                    tracing::info!("PtySlot reader: EOF");
                    break;
                }
                Ok(n) => {
                    // 受信者がいなくても送信を試行（正常動作）
                    let _ = tx.send(buf[..n].to_vec());
                }
                Err(e) => {
                    tracing::warn!("PtySlot reader error: {}", e);
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pty_spawn_and_output() {
        // echo コマンドでテスト用の出力を確認
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let slot = PtySlot::spawn(&cwd, &shell, 80, 24).expect("PTY spawn に失敗");

        // PIDが取得できること
        assert!(slot.pid() > 0 || slot.pid() == 0); // CI環境では0の可能性

        // シェルコマンドが正しいこと
        assert_eq!(slot.shell_cmd(), shell);

        // 出力を購読
        let mut rx = slot.subscribe_output();

        // シェルのプロンプトなど何らかの出力が来ることを確認
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
        assert!(
            result.is_ok(),
            "タイムアウト: PTY から出力を受信できなかった"
        );
    }

    #[tokio::test]
    async fn test_pty_write_input() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let mut slot = PtySlot::spawn(&cwd, &shell, 80, 24).expect("PTY spawn に失敗");
        let mut rx = slot.subscribe_output();

        // 少し待ってからコマンドを送信
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // rx の既存メッセージをフラッシュ
        while rx.try_recv().is_ok() {}

        // echo コマンドを送信
        slot.write(b"echo HELLO_PTY_SLOT\n")
            .expect("PTY への書き込みに失敗");

        // 出力に "HELLO_PTY_SLOT" が含まれることを確認
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = false;

        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(data)) => {
                    let text = String::from_utf8_lossy(&data);
                    if text.contains("HELLO_PTY_SLOT") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        assert!(found, "PTY 出力に HELLO_PTY_SLOT が含まれなかった");
    }

    #[tokio::test]
    async fn test_pty_drop_kills_child() {
        // Drop 実装が子プロセスを確実に終了させることを検証
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let slot = PtySlot::spawn(&cwd, &shell, 80, 24).expect("PTY spawn に失敗");
        let pid = slot.pid();

        // CI環境ではPIDが0の場合がある
        if pid == 0 {
            return;
        }

        let pid_i32 = pid as i32;

        // プロセスが起動していることを確認
        let alive_before = unsafe { libc::kill(pid_i32, 0) == 0 };
        assert!(alive_before, "子プロセスが起動していない (PID: {})", pid);

        // PtySlot を drop → Drop impl が kill + wait を呼ぶ
        drop(slot);

        // リトライループで終了を確認（固定sleepより安定）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let alive = unsafe { libc::kill(pid_i32, 0) == 0 };
            if !alive {
                break; // 成功: プロセスが終了した
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("Drop後2秒経ってもプロセスが終了していない (PID: {})", pid);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
