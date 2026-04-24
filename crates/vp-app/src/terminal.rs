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
        Some("debug") => {
            if let Some(msg) = parsed.get("msg").and_then(|v| v.as_str()) {
                tracing::info!("[xterm debug] {}", msg);
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

/// Terminal pane の HTML (xterm.js + Creo UI tokens + init script を inline)
///
/// xterm.js のテーマ色は Creo UI tokens (mint-dark) から getComputedStyle で
/// 動的に取得して適用。token の変更が xterm にも伝播する。
pub const TERMINAL_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="en" data-theme="mint-dark">
<head>
<meta charset="utf-8">
<title>vp-app terminal</title>
<style>
"#,
    include_str!("../assets/creo-tokens.css"),
    r#"
</style>
<style>
"#,
    include_str!("../assets/xterm.min.css"),
    r#"
html,body,#t{margin:0;padding:0;height:100%;width:100%;background:var(--color-surface-bg-base);}
body{overflow:hidden;}
#t{padding:12px;}
/* xterm 内 scrollbar を Creo tokens で統一 */
.xterm-viewport::-webkit-scrollbar{width:8px;}
.xterm-viewport::-webkit-scrollbar-track{background:transparent;}
.xterm-viewport::-webkit-scrollbar-thumb{background:var(--color-surface-border);border-radius:4px;}
.xterm-viewport::-webkit-scrollbar-thumb:hover{background:var(--color-brand-primary-subtle);}
</style>
</head>
<body>
<div id="t"></div>
<script>
"#,
    include_str!("../assets/xterm.min.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-fit.min.js"),
    r#"
</script>
<script>
(function() {
  // Creo tokens から xterm.js theme を構築 (runtime で var() 解決)
  const css = getComputedStyle(document.documentElement);
  const v = (name, fallback) => (css.getPropertyValue(name).trim() || fallback);
  const theme = {
    background: v('--color-surface-bg-base', '#0F1128'),
    foreground: v('--color-text-primary', '#EDEEF4'),
    cursor: v('--color-brand-primary', '#7D6BC2'),
    cursorAccent: v('--color-surface-bg-base', '#0F1128'),
    selectionBackground: v('--color-brand-primary-subtle', '#2C2843')
  };
  const term = new Terminal({
    fontFamily: '"Cascadia Code", "Cascadia Mono", "SF Mono", Menlo, Consolas, monospace',
    fontSize: 13,
    lineHeight: 1.15,
    letterSpacing: 0,
    theme: theme,
    allowProposedApi: true,
    convertEol: true,
    scrollback: 5000,
    cursorBlink: true,
    cursorStyle: 'bar',
    cursorWidth: 2,
    smoothScrollDuration: 80,
    fontLigatures: true
  });
  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  term.open(document.getElementById('t'));
  fitAddon.fit();

  function sendResize() {
    window.ipc.postMessage(JSON.stringify({t:'resize', cols: term.cols, rows: term.rows}));
  }

  window.addEventListener('resize', () => { fitAddon.fit(); sendResize(); });

  term.onData(d => {
    window.ipc.postMessage(JSON.stringify({t:'in', d: d}));
  });

  // Rust から呼ばれる関数
  window.onPtyData = function(b64) {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    term.write(bytes);
  };

  // 初期化完了を Rust に通知 (resize 情報も同時に)
  window.ipc.postMessage(JSON.stringify({t:'ready'}));
  sendResize();

  // WebView2 + child WebView で focus が弱いので、click / pointerdown で明示 focus
  const container = document.getElementById('t');
  const focusTerm = () => {
    try { term.focus(); } catch (_) {}
    window.ipc.postMessage(JSON.stringify({t:'debug', msg: 'focus requested'}));
  };
  container.addEventListener('mousedown', focusTerm);
  container.addEventListener('click', focusTerm);
  window.addEventListener('focus', focusTerm);

  // 初期 focus も明示 (DOM ready 後)
  setTimeout(focusTerm, 100);
  setTimeout(focusTerm, 500);

  // ----- Copy / Paste -----
  // terminal 慣習: Ctrl+Shift+C で選択コピー、Ctrl+Shift+V でペースト
  // (Ctrl+C は SIGINT として shell に送る)。右クリックも paste にマップ。
  // Mac の Cmd+C/V も拾う。
  const dbg = (msg) => window.ipc.postMessage(JSON.stringify({t:'debug', msg: msg}));

  term.attachCustomKeyEventHandler((e) => {
    if (e.type !== 'keydown') return true;
    const key = (e.key || '').toLowerCase();
    const modCopy = (e.ctrlKey && e.shiftKey) || e.metaKey;
    if (modCopy && key === 'c') {
      const sel = term.getSelection();
      if (sel) {
        navigator.clipboard.writeText(sel)
          .then(() => dbg('copy ok: ' + sel.length + ' chars'))
          .catch((err) => dbg('copy failed: ' + err));
      }
      return false;
    }
    if (modCopy && key === 'v') {
      navigator.clipboard.readText()
        .then((text) => { if (text) term.paste(text); })
        .catch((err) => dbg('paste failed: ' + err));
      return false;
    }
    return true;
  });

  // 右クリック = paste (xterm のデフォルト context menu を抑制)
  container.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    navigator.clipboard.readText()
      .then((text) => { if (text) term.paste(text); })
      .catch((err) => dbg('rclick paste failed: ' + err));
  });
})();
</script>
</body>
</html>"#
);
