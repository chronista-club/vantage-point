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

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, mpsc};
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
    /// TheWorld から Process list 取得成功 (Architecture v4: 旧 ProjectsLoaded)
    ProcessesLoaded(Vec<crate::client::ProcessInfo>),
    /// TheWorld への接続失敗 (Architecture v4: 旧 ProjectsError)
    ProcessesError(String),
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
    /// Phase A4-3b: SP (= Runtime Process) の `/api/lanes` を fetch して Lane list を main thread に通知
    /// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Process recursive)
    LanesLoaded {
        process_path: String,
        lanes: Vec<crate::client::LaneInfo>,
    },
    /// Phase A4-3b: Lane fetch 失敗 (SP 未起動 / 接続失敗)
    LanesError {
        process_path: String,
        message: String,
    },
}

/// PTY セッションのハンドル
///
/// IPC handler から `write` / `resize` が呼ばれる。**fire-and-forget で常時高速**:
/// - `write` は `mpsc::Sender::send` のみ → 瞬時 return (sync block ゼロ)。
///   実際の `writer.write_all` + `flush` は背景 writer thread が drain して実行。
///   → tao IPC handler が瞬時 return → JS event loop が常時 responsive →
///   rapid typing で keystroke drop が起きない。
/// - `resize` は低頻度なので `Arc<Mutex<PtyPair>>` 経由で OK。
///
/// 関連: `mem_1CaSpUi6cz9abzcEU3d6KC` (VP I/O Pipeline — Push Primitives + Non-blocking Internals)
pub struct PtyHandle {
    /// 背景 writer thread に bytes を fire-and-forget で送る送信側
    write_tx: mpsc::Sender<Vec<u8>>,
    /// resize 用に master を保持。低頻度操作なので Mutex で十分
    pair: Arc<Mutex<PtyPair>>,
}

impl PtyHandle {
    /// PTY に書き込む。fire-and-forget — 瞬時 return。
    /// 実際の write/flush は背景 writer thread が drain。
    pub fn write(&self, data: &[u8]) -> Result<()> {
        self.write_tx
            .send(data.to_vec())
            .map_err(|_| anyhow::anyhow!("PTY writer thread closed"))?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let pair = self
            .pair
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY pair mutex poisoned"))?;
        pair.master
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
            write_tx: self.write_tx.clone(),
            pair: self.pair.clone(),
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

    // シェルコマンド + 引数を決定 (shell_detect モジュールに集約、Mode 2 と共有)
    let shell = crate::shell_detect::detect_shell();
    let shell_args = crate::shell_detect::detect_shell_args(&shell);
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

    // writer thread: fire-and-forget で PTY write を背景で drain
    // (mem_1CaSpUi6cz9abzcEU3d6KC: input は瞬時 return → JS event loop が常時 responsive)
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
    thread::Builder::new()
        .name("vp-app-pty-writer".into())
        .spawn(move || writer_loop(writer, write_rx))
        .context("spawn writer thread")?;

    Ok(PtyHandle {
        write_tx,
        pair: Arc::new(Mutex::new(pair)),
    })
}

/// PTY writer ループ。`mpsc::Receiver` から bytes を受け取って sync write + flush。
///
/// sender drop (= 全 PtyHandle drop) で `for` iterator が終了 → thread exit。
/// fire-and-forget の対極にある背景処理: ここで syscall wait しても、
/// PtyHandle::write の caller (IPC handler、JS event loop) は影響を受けない。
fn writer_loop(mut writer: Box<dyn Write + Send>, rx: mpsc::Receiver<Vec<u8>>) {
    for bytes in rx {
        if let Err(e) = writer.write_all(&bytes) {
            tracing::warn!("PTY writer write_all failed: {}", e);
            break;
        }
        if let Err(e) = writer.flush() {
            tracing::warn!("PTY writer flush failed: {}", e);
            break;
        }
    }
    tracing::info!("PTY writer thread 終了 (sender drop)");
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

// detect_shell / detect_shell_args / is_on_path は `crate::shell_detect` に集約済み。
// Mode 1 (この file の spawn_shell) と Mode 2 (ws_terminal::connect_daemon_terminal) で
// 同じ判定ロジックを共有する。

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
/// Phase 2.5 (per-Lane instance): `in` / `resize` は Lane WebSocket が browser native で
/// SP に直接送信するので、 Rust 経路は使わない (silent no-op)。
/// `ready` / `copy` / `debug` / `slot:rect` は引き続き Rust 側で処理。
pub fn handle_ipc_message(msg: &str, proxy: &EventLoopProxy<AppEvent>) {
    let parsed: serde_json::Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("terminal IPC JSON パース失敗: {}", e);
            return;
        }
    };

    match parsed.get("t").and_then(|v| v.as_str()) {
        Some("in") | Some("resize") => {
            // Phase 2.5: Lane WS が直接 SP に送信するので Rust 経路は使わない。
            // 旧 single-term の互換のため受け取りは続けるが silent no-op。
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
