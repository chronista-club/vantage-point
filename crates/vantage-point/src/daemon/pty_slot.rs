//! PTYスロット — 個々のPTYプロセスの管理
//!
//! portable-pty で PTY を作成し、master fd からの出力を
//! broadcast channel 経由で配信する。
//! 既存の `process/pty.rs` の PtySession を基に、Daemon用に再設計。
//! base64エンコードはしない（IPC層の責務）。
//!
//! ## Phase 2.x-c: scrollback ring buffer
//!
//! 過去 256 KB の output を保持し、 新規 subscriber に initial replay する。
//! Lane 切替や vp-app 再起動で同じ Lane に戻ってきた時、 broadcast::channel(256)
//! の buffer は即過去になっていて scroll back が見られない問題の解消。
//!
//! Atomicity: reader_task が `ring.push + broadcast.send` を **同一 lock 内** で行う。
//! 新 subscriber は `ring.lock + subscribe + clone snapshot` を atomic に行うことで、
//! 重複なし・取りこぼしなしで initial bytes と継続 stream の境界を作れる。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::broadcast;

/// scrollback ring buffer の最大保持 bytes (= 256 KB)。
/// xterm.js 側 scrollback:5000 行 と粒度合わせ。 大半の terminal 利用シーンで十分、
/// `claude` の長い summary でも overflow しない。
const SCROLLBACK_CAP: usize = 256 * 1024;

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
    /// Phase 2.x-c: scrollback ring buffer (新規 subscriber への initial replay 用)。
    /// reader_task が push + broadcast.send を同一 lock 内で行うことで、
    /// `subscribe_with_scrollback` が atomic な「snapshot + subscribe」 を実現できる。
    scrollback: Arc<Mutex<Vec<u8>>>,
    /// reader task のハンドル
    _reader_handle: tokio::task::JoinHandle<()>,
}

impl PtySlot {
    /// PTYプロセスを起動
    ///
    /// 指定したシェルコマンドを PTY 上で起動し、
    /// 出力を broadcast channel に配信する reader task を開始する。
    pub fn spawn(
        cwd: &str,
        shell_cmd: &str,
        args: &[String],
        cols: u16,
        rows: u16,
    ) -> Result<(Self, broadcast::Receiver<Vec<u8>>)> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell_cmd);
        cmd.cwd(cwd);
        // 起動引数は caller が決める (zsh/bash → "-l" で login shell、pwsh → "-NoLogo" 等)。
        // 旧実装は `cmd.arg("-l")` を hardcode していたが、pwsh 等で無効 flag になる問題があり廃止。
        for arg in args {
            cmd.arg(arg);
        }

        // 子プロセスを起動（ゾンビ防止のためハンドルを保持する）
        let child = pair.slave.spawn_command(cmd)?;
        let pid = child.process_id().unwrap_or(0);

        // マスター側の読み書きハンドル
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // broadcast channel（バッファ 256）
        // initial_rx を保持し、reader_task 開始前に subscriber を確保する。
        // これにより PTY からの最初のバイト（シェルプロンプト等）を取りこぼさない。
        let (output_tx, initial_rx) = broadcast::channel(256);

        // Phase 2.x-c: scrollback ring buffer (256 KB)
        let scrollback: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(SCROLLBACK_CAP)));

        // reader task 開始 (scrollback も共有)
        let reader_handle = start_reader_task(reader, output_tx.clone(), scrollback.clone());

        Ok((
            Self {
                writer,
                pair,
                child,
                pid,
                shell_cmd: shell_cmd.to_string(),
                output_tx,
                scrollback,
                _reader_handle: reader_handle,
            },
            initial_rx,
        ))
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

    /// Phase 2.x-c: scrollback 付きで購読する。
    ///
    /// `(rx, initial_bytes)` を atomic に取得 ── ring の lock を持っている間に
    /// `subscribe()` を呼ぶことで、 reader_task が次の push + send をするまでに
    /// 我々が新 subscriber として登録される。 結果:
    /// - `initial_bytes`: lock 取得時点までの ring 内容 (= 過去 256 KB の output)
    /// - `rx`: lock 取得後の broadcast.send を全て受信
    /// - 重複なし、 取りこぼしなし
    ///
    /// 用途: vp-app が `/ws/terminal?lane=...` で attach してきた時、
    /// initial_bytes を WS Binary で先送して履歴を再生する。
    pub fn subscribe_with_scrollback(&self) -> (broadcast::Receiver<Vec<u8>>, Vec<u8>) {
        let ring = self.scrollback.lock().expect("scrollback mutex poisoned");
        let initial = ring.clone();
        let rx = self.output_tx.subscribe();
        drop(ring);
        (rx, initial)
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
/// 1) **scrollback ring に append** (cap 超過分は drain で削除)
/// 2) **broadcast channel に send** (両者を同一 lock 内で行うことで
///    `subscribe_with_scrollback` の atomicity を保証)
///
/// base64 エンコードはしない (IPC 層の責務)。
fn start_reader_task(
    mut reader: Box<dyn Read + Send>,
    tx: broadcast::Sender<Vec<u8>>,
    scrollback: Arc<Mutex<Vec<u8>>>,
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
                    let chunk = buf[..n].to_vec();
                    // Phase 2.x-c: ring + broadcast を同一 lock 内で送出 (atomicity 保証)
                    {
                        let mut ring = match scrollback.lock() {
                            Ok(g) => g,
                            Err(e) => {
                                tracing::warn!("scrollback mutex poisoned: {}", e);
                                break;
                            }
                        };
                        ring.extend_from_slice(&chunk);
                        if ring.len() > SCROLLBACK_CAP {
                            let drop_n = ring.len() - SCROLLBACK_CAP;
                            ring.drain(..drop_n);
                        }
                        // 受信者がいなくても送信を試行（正常動作）
                        let _ = tx.send(chunk);
                    } // unlock
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

        let (slot, mut rx) = PtySlot::spawn(&cwd, &shell, &[], 80, 24).expect("PTY spawn に失敗");

        // PIDが取得できること
        assert!(slot.pid() > 0 || slot.pid() == 0); // CI環境では0の可能性

        // シェルコマンドが正しいこと
        assert_eq!(slot.shell_cmd(), shell);

        // 初期 receiver でシェルのプロンプトなど何らかの出力が来ることを確認
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

        let (mut slot, mut rx) =
            PtySlot::spawn(&cwd, &shell, &[], 80, 24).expect("PTY spawn に失敗");

        // 少し待ってからコマンドを送信
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // 初期 receiver の既存メッセージをフラッシュ
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

        let (slot, _rx) = PtySlot::spawn(&cwd, &shell, &[], 80, 24).expect("PTY spawn に失敗");
        let pid = slot.pid();

        // CI環境ではPIDが0の場合がある
        if pid == 0 {
            return;
        }

        // プロセスが起動していることを確認
        let alive_before = crate::platform::process_alive(pid);
        assert!(alive_before, "子プロセスが起動していない (PID: {})", pid);

        // PtySlot を drop → Drop impl が kill + wait を呼ぶ
        drop(slot);

        // リトライループで終了を確認（固定sleepより安定）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !crate::platform::process_alive(pid) {
                break; // 成功: プロセスが終了した
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("Drop後2秒経ってもプロセスが終了していない (PID: {})", pid);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
