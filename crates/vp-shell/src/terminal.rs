//! Terminal pane — portable-pty で shell を起動し、wry IPC で xterm.js と双方向ブリッジ
//!
//! ## パイプライン
//! ```text
//!  xterm.js (user input) ── window.ipc.postMessage ──► Rust ipc_handler ──► PTY writer
//!  PTY reader (stdout/stderr) ──► EventLoopProxy ──► UserEvent ──► evaluate_script ──► xterm.js.write
//! ```
//!
//! Phase W2 MVP: vp-shell プロセス内で直接 PTY spawn (local PTY)。
//! 後続 Phase で daemon (TheWorld) の WebSocket PTY channel 経由に差し替え予定。

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use tao::event_loop::EventLoopProxy;

/// EventLoop に送る terminal 関連のイベント
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// PTY から読み取った出力バイト列
    Output(Vec<u8>),
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
    proxy: EventLoopProxy<TerminalEvent>,
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

    // シェルコマンドを決定 (Windows: cmd.exe、Unix: $SHELL or /bin/bash)
    let shell = detect_shell();
    let mut cmd = CommandBuilder::new(&shell);
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    } else if let Some(home) = dirs_home() {
        cmd.cwd(&home);
    }

    let _child = pair.slave.spawn_command(cmd).context("spawn shell")?;
    tracing::info!("PTY shell 起動: {} ({}x{})", shell, cols, rows);

    // reader thread: PTY master → EventLoopProxy
    let reader = pair.master.try_clone_reader().context("clone reader")?;
    thread::Builder::new()
        .name("vp-shell-pty-reader".into())
        .spawn(move || reader_loop(reader, proxy))
        .context("spawn reader thread")?;

    let writer = pair.master.take_writer().context("take writer")?;

    Ok(PtyHandle {
        inner: Arc::new(Mutex::new(PtyInner { writer, pair })),
    })
}

/// PTY reader ループ。EOF / エラーで終了。
fn reader_loop(mut reader: Box<dyn Read + Send>, proxy: EventLoopProxy<TerminalEvent>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::info!("PTY reader: EOF");
                break;
            }
            Ok(n) => {
                if proxy
                    .send_event(TerminalEvent::Output(buf[..n].to_vec()))
                    .is_err()
                {
                    // EventLoop 終了済み
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
fn detect_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
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
/// - `{"t":"ready"}` — xterm.js 初期化完了 (現状は情報のみ)
pub fn handle_ipc_message(msg: &str, pty: &PtyHandle) {
    let parsed: serde_json::Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("terminal IPC JSON パース失敗: {}", e);
            return;
        }
    };

    match parsed.get("t").and_then(|v| v.as_str()) {
        Some("in") => {
            if let Some(data) = parsed.get("d").and_then(|v| v.as_str())
                && let Err(e) = pty.write(data.as_bytes())
            {
                tracing::warn!("PTY write 失敗: {}", e);
            }
        }
        Some("resize") => {
            let cols = parsed.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
            let rows = parsed.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            if let Err(e) = pty.resize(cols, rows) {
                tracing::warn!("PTY resize 失敗: {}", e);
            }
        }
        Some("ready") => {
            tracing::info!("xterm.js ready");
        }
        other => {
            tracing::debug!("terminal IPC: unknown type {:?}", other);
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
<title>vp-shell terminal</title>
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
#t{padding:8px;}
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
    fontFamily: '"Cascadia Mono", "SF Mono", Menlo, Consolas, monospace',
    fontSize: 13,
    theme: theme,
    allowProposedApi: true,
    convertEol: true
  });
  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  term.open(document.getElementById('t'));
  fitAddon.fit();

  function sendResize() {
    window.ipc.postMessage(JSON.stringify({t:'resize', cols: term.cols, rows: term.rows}));
  }

  window.addEventListener('resize', () => { fitAddon.fit(); sendResize(); });

  term.onData(d => window.ipc.postMessage(JSON.stringify({t:'in', d: d})));

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

  term.focus();
})();
</script>
</body>
</html>"#
);
