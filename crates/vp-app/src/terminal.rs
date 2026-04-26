//! Terminal pane — portable-pty で shell を起動し、wry IPC で xterm.js と双方向ブリッジ
//!
//! ## パイプライン
//! ```text
//!  xterm.js (user input) ── window.ipc.postMessage ──► Rust ipc_handler ──► PTY writer
//!  PTY reader (stdout/stderr) ──► EventLoopProxy ──► UserEvent ──► evaluate_script ──► xterm.js.write
//! ```
//!
//! Phase W2 MVP: vp-app プロセス内で直接 PTY spawn (local PTY)。
//! 後続 Phase で daemon (TheWorld) の WebSocket PTY channel 経由に差し替え予定。

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use tao::event_loop::EventLoopProxy;

/// EventLoop に送る app 全体のイベント
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// PTY から読み取った出力バイト列
    Output(Vec<u8>),
    /// xterm.js 側から ready 通知 (IPC 経由で届いたら main thread に伝える)
    XtermReady,
    /// TheWorld から project list 取得成功
    ProjectsLoaded(Vec<crate::client::ProjectInfo>),
    /// TheWorld への接続失敗
    ProjectsError(String),
    /// VP-95: Activity widget の定期更新 payload
    ActivityUpdate(crate::pane::ActivitySnapshot),
    /// VP-95: sidebar webview からの IPC メッセージ (JSON 文字列、main loop でパース)
    SidebarIpc(String),
    /// VP-100 γ-light: main area の active pane slot 矩形通知。
    ///
    /// Phase 2 時点では受け取って store するだけ。Phase 4+ で native pane が
    /// 追加された時に native widget の `set_position` 同期に使う想定。
    /// 詳細は memory:vp_app_native_overlay_resize_ghost.md。
    SlotRect {
        pane_id: Option<String>,
        kind: String,
        rect: crate::main_area::SlotRect,
    },
    /// VP-100 follow-up: muda メニュー項目クリック (developer mode toggle / open devtools 等)
    MenuClicked(muda::MenuId),
    /// Clone 先フォルダ picker で選択された path を sidebar JS に push (キャンセル時は None)
    ClonePathPicked(Option<String>),
}

/// PTY セッションのハンドル
///
/// writer と pair を保持し、ipc_handler から `write` / `resize` が呼ばれる。
/// Arc<Mutex<>> で wrap して clone 可能にしている。
pub struct PtyHandle {
    inner: Arc<Mutex<PtyInner>>,
}

struct PtyInner {
    writer: Box<dyn std::io::Write + Send>,
    pair: PtyPair,
}

impl PtyHandle {
    pub fn write(&self, data: &[u8]) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY mutex poisoned"))?;
        inner
            .writer
            .write_all(data)
            .context("PTY writer write_all")?;
        inner.writer.flush().context("PTY writer flush")?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY mutex poisoned"))?;
        inner
            .pair
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("PTY resize")?;
        Ok(())
    }
}

impl Clone for PtyHandle {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// シェルを PTY 上で起動し、reader thread で出力を EventLoop に送り続ける
///
/// 戻り値: xterm.js とのブリッジに使う `PtyHandle` (write / resize)
pub fn spawn_shell(
    cwd: Option<&str>,
    cols: u16,
    rows: u16,
    proxy: EventLoopProxy<AppEvent>,
) -> Result<PtyHandle> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    // シェルコマンド + 引数を決定
    let shell = detect_shell();
    let shell_args = detect_shell_args();
    let mut cmd = CommandBuilder::new(&shell);
    for arg in &shell_args {
        cmd.arg(arg);
    }
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    } else if let Some(home) = dirs_home() {
        cmd.cwd(&home);
    }

    let _child = pair.slave.spawn_command(cmd).context("spawn shell")?;
    if shell_args.is_empty() {
        tracing::info!("PTY shell 起動: {} ({}x{})", shell, cols, rows);
    } else {
        tracing::info!(
            "PTY shell 起動: {} {} ({}x{})",
            shell,
            shell_args.join(" "),
            cols,
            rows
        );
    }

    // reader thread: PTY master → EventLoopProxy
    let reader = pair.master.try_clone_reader().context("clone reader")?;
    thread::Builder::new()
        .name("vp-app-pty-reader".into())
        .spawn(move || reader_loop(reader, proxy))
        .context("spawn reader thread")?;

    let writer = pair.master.take_writer().context("take writer")?;

    Ok(PtyHandle {
        inner: Arc::new(Mutex::new(PtyInner { writer, pair })),
    })
}

/// PTY reader ループ。EOF / エラーで終了。
fn reader_loop(mut reader: Box<dyn Read + Send>, proxy: EventLoopProxy<AppEvent>) {
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::info!("PTY reader: EOF (total={} bytes)", total);
                break;
            }
            Ok(n) => {
                total += n;
                let hex: String = buf[..n].iter().map(|b| format!("{:02x}", b)).collect();
                let ascii: String = buf[..n]
                    .iter()
                    .map(|&b| {
                        if (0x20..=0x7e).contains(&b) {
                            b as char
                        } else {
                            '.'
                        }
                    })
                    .collect();
                tracing::debug!(
                    "PTY reader: {} bytes [hex={} ascii={:?}] total={}",
                    n,
                    hex,
                    ascii,
                    total
                );
                if proxy
                    .send_event(AppEvent::Output(buf[..n].to_vec()))
                    .is_err()
                {
                    tracing::info!("EventLoop 終了、reader_loop 終了");
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("PTY reader error: {}", e);
                break;
            }
        }
    }
}

/// プラットフォーム別のデフォルトシェル
///
/// Windows: PATH から pwsh (PowerShell 7+) → powershell (5.x) → cmd の順で
/// 最初に見つかったものを採用。`VP_SHELL` env var があればそれを優先。
/// Unix: `$SHELL` → `/bin/bash` フォールバック。
///
/// ## WSL / claude 連携例
/// ```bash
/// VP_SHELL=wsl.exe VP_SHELL_ARGS="--cd /home/mito/repos/vantage-point -- claude"
/// ```
fn detect_shell() -> String {
    if let Ok(explicit) = std::env::var("VP_SHELL") {
        return explicit;
    }
    #[cfg(windows)]
    {
        for candidate in &["pwsh.exe", "powershell.exe", "cmd.exe"] {
            if is_on_path(candidate) {
                return (*candidate).to_string();
            }
        }
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

/// シェル起動時の引数 (`VP_SHELL_ARGS` env var、POSIX シェル風クォート対応)
///
/// 例: `--cd /path -- bash -l -c "claude --continue || claude"` は
/// ["--cd", "/path", "--", "bash", "-l", "-c", "claude --continue || claude"] に分割。
fn detect_shell_args() -> Vec<String> {
    std::env::var("VP_SHELL_ARGS")
        .ok()
        .and_then(|s| shell_words::split(&s).ok())
        .unwrap_or_default()
}

/// 簡易 `which`: PATH を走査して実行可能ファイルの存在を確認
#[cfg(windows)]
fn is_on_path(name: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let candidate = std::path::Path::new(dir).join(name);
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

/// HOME ディレクトリ (PTY cwd のデフォルト)
fn dirs_home() -> Option<String> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok()
    }
    #[cfg(unix)]
    {
        std::env::var("HOME").ok()
    }
}

/// xterm.js から IPC で送られてきた JSON メッセージを処理
///
/// 期待する形式:
/// - `{"t":"in","d":"..."}` — ユーザ入力 (string)
/// - `{"t":"resize","cols":N,"rows":N}` — リサイズ
/// - `{"t":"ready"}` — xterm.js 初期化完了 → UserEvent::XtermReady を main thread に送る
pub fn handle_ipc_message(msg: &str, pty: &TerminalHandle, proxy: &EventLoopProxy<AppEvent>) {
    let parsed: serde_json::Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("terminal IPC JSON パース失敗: {}", e);
            return;
        }
    };

    match parsed.get("t").and_then(|v| v.as_str()) {
        Some("in") => {
            if let Some(data) = parsed.get("d").and_then(|v| v.as_str()) {
                tracing::debug!("IPC in: {} bytes", data.len());
                if let Err(e) = pty.write(data.as_bytes()) {
                    tracing::warn!("PTY write 失敗: {}", e);
                }
            }
        }
        Some("resize") => {
            let cols = parsed.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
            let rows = parsed.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            tracing::info!("IPC resize: {}x{}", cols, rows);
            if let Err(e) = pty.resize(cols, rows) {
                tracing::warn!("PTY resize 失敗: {}", e);
            }
        }
        Some("ready") => {
            tracing::info!("xterm.js ready → flush buffered PTY output");
            let _ = proxy.send_event(AppEvent::XtermReady);
        }
        Some("copy") => {
            // navigator.clipboard が使えなかった時の fallback: arboard で OS clipboard 直書き
            if let Some(data) = parsed.get("d").and_then(|v| v.as_str()) {
                match arboard::Clipboard::new() {
                    Ok(mut cb) => match cb.set_text(data) {
                        Ok(_) => {
                            tracing::info!("[clipboard] copy via arboard: {} chars", data.len())
                        }
                        Err(e) => tracing::warn!("[clipboard] arboard set_text failed: {}", e),
                    },
                    Err(e) => tracing::warn!("[clipboard] arboard init failed: {}", e),
                }
            }
        }
        Some("debug") => {
            if let Some(msg) = parsed.get("msg").and_then(|v| v.as_str()) {
                tracing::info!("[xterm debug] {}", msg);
            }
        }
        // VP-100 γ-light: main area の active slot 矩形通知 (ResizeObserver から)
        Some("slot:rect") => {
            let pane_id = parsed
                .get("pane_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let kind = parsed
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("empty")
                .to_string();
            if let Some(rect_v) = parsed.get("rect") {
                let rect = crate::main_area::SlotRect {
                    x: rect_v.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    y: rect_v.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    w: rect_v.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    h: rect_v.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0),
                };
                let _ = proxy.send_event(AppEvent::SlotRect {
                    pane_id,
                    kind,
                    rect,
                });
            }
        }
        other => {
            tracing::debug!("terminal IPC: unknown type {:?}", other);
        }
    }
}

/// Terminal backend の統一ハンドル
///
/// local portable-pty (`PtyHandle`) と daemon WebSocket (`WsTerminalHandle`) を
/// 相互交換可能に wrap する。
#[derive(Clone)]
pub enum TerminalHandle {
    Local(PtyHandle),
    Daemon(crate::ws_terminal::WsTerminalHandle),
}

impl TerminalHandle {
    pub fn write(&self, data: &[u8]) -> Result<()> {
        match self {
            Self::Local(h) => h.write(data),
            Self::Daemon(h) => h.write(data),
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        match self {
            Self::Local(h) => h.resize(cols, rows),
            Self::Daemon(h) => h.resize(cols, rows),
        }
    }
}

/// Rust → xterm.js に PTY バイト列を送る JS スニペットを生成
///
/// base64 でエンコードして `window.onPtyData(b64)` を呼ぶ。
pub fn build_output_script(bytes: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("window.onPtyData('{}')", b64)
}
