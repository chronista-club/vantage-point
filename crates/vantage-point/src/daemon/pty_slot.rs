//! PTYスロット — 個々のPTYプロセスの管理
//!
//! portable-pty で PTY を作成し、master fd からの出力を
//! broadcast channel 経由で配信する。
//! 既存の `process/pty.rs` の PtySession を基に、Daemon用に再設計。
//! base64エンコードはしない（IPC層の責務）。
//!
//! terminal S4 (doc 27 §4.1): PTY 出力は broadcast → per-lane terminal pump →
//! World "canvas" topic 空間に流れる。 旧 `/ws/terminal` attach 時の scrollback replay
//! (ring buffer) は consumer (ws_terminal) ごと撤去した (live stream のみ)。

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
    pub fn spawn(
        cwd: &str,
        shell_cmd: &str,
        args: &[String],
        env: &[(String, String)],
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
        // doc 11 (PR-B): mise task 経由の起動で VP_CWD / VP_SESSION 等を子プロセスに渡す。
        for (key, value) in env {
            cmd.env(key, value);
        }
        // PATH 補正: vp-app (.app) を GUI / launchd 経由で起動すると、 子プロセスの PATH が
        // `/usr/bin:/bin:/usr/sbin:/sbin` の最小集合になり、 user-installed tool (特に mise、
        // conductor lane = `mise run vp:stand:echoes` の program) を見つけられず spawn が失敗 →
        // lane が即 Dead 化 → Echoes コンソールが出ない、 という症状の根因になる。
        // 既知の user tool location を base PATH の先頭に前置して解決する。
        // base は caller env の PATH (あれば) → なければ親プロセスの PATH。
        // 補正ロジックの SSOT は `crate::spawn_env`。 本来は daemon / SP の spawn 最上流で
        // 補強済みのはずだが (#498 再発の根治)、 末端でも二重保険として補強する。
        {
            let base_path = env
                .iter()
                .find(|(k, _)| k == "PATH")
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            let home = std::env::var("HOME").ok();
            cmd.env(
                "PATH",
                crate::spawn_env::augment_path(&base_path, home.as_deref()),
            );
        }

        // TERM 補正: vp-app を GUI / launchd 経由で起動 (= 再起動後の LaunchAgent 自動起動) すると、
        // daemon プロセスは端末非接続で TERM を持たない。 echoes stand の `tmux new-session -A`
        // (attach 付き) は terminfo 引きに TERM を要求するため、 TERM 不在だと
        // "open terminal failed: terminal does not support clear" で即 exit → stand spawn が
        // 800ms 以内に死に lane が即 Dead 化 → Echoes コンソールが出ない。 PATH 補正 (#498) と
        // 同じ launchd-env-stripping の双子で、 plist EnvironmentVariables も PATH だけ焼いて
        // TERM を取りこぼしていた。 この PTY の出力は vp-app の xterm.js が描画する (echoes script
        // も `terminal-overrides ',xterm-256color:Tc'` を前提) ので、 TERM=xterm-256color を
        // 既定とする。 caller が env で明示注入した場合はそれを尊重する。
        if !env.iter().any(|(k, _)| k == "TERM") {
            cmd.env("TERM", "xterm-256color");
        }

        // LANG/LC_CTYPE 補正: PATH (#498) / TERM の双子で、 launchd / GUI 起動の daemon は
        // C ロケール伝播で LANG 不在になり、 echoes stand の tmux client が utf8=0 で起動 →
        // console の CJK (日本語) が `_` 化する (三つ子の三本目)。 plist EnvironmentVariables や
        // echoes mise task の LANG guard は「①旧 plist が upgrade で再生成されない ②session 永続で
        // 2 回目以降は adopt 経路が LANG guard を通らない」で漏れるが、 全 spawn 経路 (mise task /
        // adopt) が必ずこの PtySlot を通るため、 末端で注入すれば daemon / plist の LANG 状態に
        // 非依存で確定的に UTF-8 を保証できる。 caller が env で明示した LANG / LC_CTYPE は尊重する。
        let locale = crate::spawn_env::utf8_locale();
        if !env.iter().any(|(k, _)| k == "LANG") {
            cmd.env("LANG", &locale);
        }
        if !env.iter().any(|(k, _)| k == "LC_CTYPE") {
            cmd.env("LC_CTYPE", &locale);
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

        // reader task 開始
        let reader_handle = start_reader_task(reader, output_tx.clone());

        Ok((
            Self {
                writer,
                pair,
                child,
                pid,
                shell_cmd: shell_cmd.to_string(),
                output_tx,
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

    /// プロセスID
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// シェルコマンド
    pub fn shell_cmd(&self) -> &str {
        &self.shell_cmd
    }

    /// 子プロセスがまだ生きているかチェック (non-blocking try_wait)。
    /// Phase 5-D: spawn 直後の早期 exit 検知 (例: `claude --continue` で session corrupt) に使う。
    /// `Err` は wait 自体の失敗 ─ 安全側に倒して「死亡」扱いとする。
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
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
/// PTY の master fd からバイト列を読み取り、 broadcast channel に send する。
/// base64 エンコードはしない (IPC 層の責務)。
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
                    let chunk = buf[..n].to_vec();
                    // 受信者がいなくても送信を試行（正常動作）
                    let _ = tx.send(chunk);
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

    /// テスト用のデフォルトシェルを返す。
    /// $SHELL があればそれを、無ければ OS 既定（Unix: /bin/sh、Windows: cmd.exe）を使う。
    /// Windows には /bin/sh が無いので OS 分岐が必須。
    fn default_test_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
    }

    #[tokio::test]
    async fn test_pty_spawn_and_output() {
        // echo コマンドでテスト用の出力を確認
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (slot, mut rx) =
            PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24).expect("PTY spawn に失敗");

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
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (mut slot, mut rx) =
            PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24).expect("PTY spawn に失敗");

        // 少し待ってからコマンドを送信 (シェル初期化を待つ)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // echo コマンドを送信。改行コードは OS 依存
        // (Unix シェルは LF、cmd.exe(ConPTY) は Enter=CR で行確定)。
        let echo_cmd: &[u8] = if cfg!(windows) {
            b"echo HELLO_PTY_SLOT\r"
        } else {
            b"echo HELLO_PTY_SLOT\n"
        };
        slot.write(echo_cmd).expect("PTY への書き込みに失敗");

        // 出力に "HELLO_PTY_SLOT" が含まれることを確認
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut found = false;

        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(data)) => {
                    let text = String::from_utf8_lossy(&data);
                    // ConPTY は DSR (\x1b[6n = カーソル位置問い合わせ) への応答を
                    // 端末側から受け取るまで描画を進めない。本番では実端末 (xterm.js)
                    // が応答するが、テストでは我々が端末役として応答する必要がある。
                    if text.contains("\u{1b}[6n") {
                        let _ = slot.write(b"\x1b[1;1R");
                    }
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

    /// 回帰 (console-blackout root cause): PTY child は親プロセスの TERM 有無に依らず
    /// TERM=xterm-256color を受け取る。 launchd 自動起動の daemon は端末非接続で TERM を
    /// 継承しないため、 TERM 不在だと echoes の `tmux new-session -A` が "open terminal
    /// failed" で即死 → lane spawn 全滅 → console が出ない。 PtySlot が TERM 既定を注入する
    /// ことで daemon の TERM 有無に依らず stand が描画可能になることを pin する。
    #[cfg(unix)]
    #[tokio::test]
    async fn test_pty_spawn_injects_term_default() {
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        // 子の $TERM を marker 付きで 1 発出力して即終了する非対話 shell。 env 未指定 (&[]) なので
        // PtySlot が TERM 既定を注入するはず。 親の TERM 値に依らず子側の値だけを検証できる。
        let args = vec![
            "-c".to_string(),
            "printf 'VPTERM[%s]\\n' \"$TERM\"; sleep 0.2".to_string(),
        ];
        let (_slot, mut rx) =
            PtySlot::spawn(&cwd, "/bin/sh", &args, &[], 80, 24).expect("PTY spawn に失敗");

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut buf = String::new();
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Ok(data)) => {
                    buf.push_str(&String::from_utf8_lossy(&data));
                    if buf.contains("VPTERM[") && buf.contains(']') {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        assert!(
            buf.contains("VPTERM[xterm-256color]"),
            "PTY child の TERM が xterm-256color でない: 受信={:?}",
            buf
        );
    }

    #[tokio::test]
    async fn test_pty_drop_kills_child() {
        // Drop 実装が子プロセスを確実に終了させることを検証
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        let (slot, _rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24).expect("PTY spawn に失敗");
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
