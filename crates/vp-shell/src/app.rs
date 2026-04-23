//! Main EventLoop + window lifecycle
//!
//! ## アーキテクチャ方針 (Mac 版と同等 + Creo UI 統一)
//!
//! 「ネイティブ層ベース + WebUI on top」のハイブリッド構成。
//! デザインシステムは **Creo UI** (mint-dark theme) を全ペインで共有。
//!
//! ```text
//! ┌─── tao ネイティブウィンドウ (native chrome, menu, tray) ──┐
//! │ ┌──────────┬───────────────────────────────────────┐ │
//! │ │ sidebar  │       canvas (wry WebView)             │ │
//! │ │ wry      │                                        │ │
//! │ │ WebView  ├────────────────────────────────────────┤ │
//! │ │          │       terminal (xterm.js in WebView)   │ │
//! │ │ (~280px) │                                        │ │
//! │ └──────────┴───────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! - **ウィンドウ・メニュー・トレイ・レイアウト境界** は Rust (tao + muda + tray-icon)
//! - **各ペインの内容** は wry WebView (HTML/CSS/JS、xterm.js 含む)
//! - **Creo UI tokens.css (mint-dark)** を各 WebView に inline して token 統一

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{
    Rect, WebView, WebViewBuilder, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize,
};

use crate::terminal::{self, TerminalEvent};

/// Sidebar の固定幅 (LogicalPixel)
const SIDEBAR_WIDTH: f64 = 280.0;

/// Creo UI design tokens (CSS custom properties、mint-dark default)
///
/// <https://github.com/chronista-club/creo-ui> packages/web が source。
/// vp-shell の 3 ペインすべてに inline して共通 token で描画する。
pub const CREO_TOKENS_CSS: &str = include_str!("../assets/creo-tokens.css");

const SIDEBAR_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="ja" data-theme="mint-dark">
<head><meta charset="utf-8"><style>"#,
    include_str!("../assets/creo-tokens.css"),
    r#"</style><style>
  html,body{margin:0;height:100%;background:var(--color-surface-bg-subtle);color:var(--color-text-primary);font-family:system-ui,-apple-system,"Segoe UI",sans-serif;}
  header{padding:16px;font-size:12px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:.08em;}
  ul{list-style:none;margin:0;padding:0 8px;}
  li{padding:8px 12px;border-radius:var(--radius-sm,6px);color:var(--color-text-primary);cursor:pointer;font-size:14px;transition:background .12s ease;}
  li:hover{background:var(--color-surface-bg-emphasis);}
  li.active{background:var(--color-brand-primary-subtle);color:var(--color-text-primary);}
</style></head>
<body>
  <header>Projects</header>
  <ul>
    <li class="active">vantage-point</li>
    <li>(Phase W2 scaffold)</li>
  </ul>
</body>
</html>"#
);

const CANVAS_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="ja" data-theme="mint-dark">
<head><meta charset="utf-8"><style>"#,
    include_str!("../assets/creo-tokens.css"),
    r#"</style><style>
  html,body{margin:0;height:100%;background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:system-ui,-apple-system,"Segoe UI","Cascadia Code",monospace;}
  body{display:grid;place-items:center;}
  main{text-align:center;}
  h1{font-weight:500;font-size:1.6rem;margin:0 0 .25rem;color:var(--color-text-primary);}
  p{color:var(--color-text-tertiary);margin:0;font-size:.9rem;}
  .brand{color:var(--color-brand-primary);}
</style></head>
<body>
  <main>
    <h1>Canvas pane</h1>
    <p>Phase W2 — <span class="brand">Creo UI mint-dark</span> を全ペイン統一で適用</p>
  </main>
</body>
</html>"#
);

/// Sidebar / Canvas / Terminal の bounds をウィンドウサイズから計算
fn update_pane_bounds(
    sidebar: &WebView,
    canvas: &WebView,
    terminal_view: &WebView,
    window_size: tao::dpi::PhysicalSize<u32>,
    scale: f64,
) {
    let logical = window_size.to_logical::<f64>(scale);
    let width = logical.width;
    let height = logical.height;
    let right_x = SIDEBAR_WIDTH;
    let right_w = (width - SIDEBAR_WIDTH).max(0.0);
    let canvas_h = (height / 2.0).round();
    let terminal_y = canvas_h;
    let terminal_h = (height - canvas_h).max(0.0);

    let _ = sidebar.set_bounds(Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: WryLogicalSize::new(SIDEBAR_WIDTH, height).into(),
    });
    let _ = canvas.set_bounds(Rect {
        position: LogicalPosition::new(right_x, 0.0).into(),
        size: WryLogicalSize::new(right_w, canvas_h).into(),
    });
    let _ = terminal_view.set_bounds(Rect {
        position: LogicalPosition::new(right_x, terminal_y).into(),
        size: WryLogicalSize::new(right_w, terminal_h).into(),
    });
}

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vp_shell=info".into()),
        )
        .init();

    tracing::info!("vp-shell 起動 (Creo UI mint-dark)");

    let event_loop = EventLoopBuilder::<TerminalEvent>::with_user_event().build();

    // メニューバー + トレイ
    let _menu = crate::menu::build_menu_bar();
    let _tray = match crate::tray::build_tray() {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!("トレイ初期化失敗 (無効化): {}", e);
            None
        }
    };

    let window = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)?;

    // PTY を起動し、reader thread が EventLoopProxy 経由で出力イベントを送る
    let proxy = event_loop.create_proxy();
    let pty = terminal::spawn_shell(None, 80, 24, proxy)?;

    // Sidebar
    let sidebar = WebViewBuilder::new()
        .with_html(SIDEBAR_HTML)
        .with_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: WryLogicalSize::new(SIDEBAR_WIDTH, 800.0).into(),
        })
        .build_as_child(&window)?;

    // Canvas
    let canvas = WebViewBuilder::new()
        .with_html(CANVAS_HTML)
        .with_bounds(Rect {
            position: LogicalPosition::new(SIDEBAR_WIDTH, 0.0).into(),
            size: WryLogicalSize::new(1200.0 - SIDEBAR_WIDTH, 400.0).into(),
        })
        .build_as_child(&window)?;

    // Terminal pane: xterm.js + IPC handler で PTY に双方向接続
    // IPC handler は ready 通知を EventLoopProxy 経由で main thread に伝える
    let pty_for_ipc = pty.clone();
    let ipc_proxy = event_loop.create_proxy();
    let terminal_view = WebViewBuilder::new()
        .with_html(terminal::TERMINAL_HTML)
        .with_bounds(Rect {
            position: LogicalPosition::new(SIDEBAR_WIDTH, 400.0).into(),
            size: WryLogicalSize::new(1200.0 - SIDEBAR_WIDTH, 400.0).into(),
        })
        .with_devtools(true)
        .with_ipc_handler(move |req| {
            terminal::handle_ipc_message(req.body(), &pty_for_ipc, &ipc_proxy);
        })
        .with_focused(true)
        .build_as_child(&window)?;

    tracing::info!("メインウィンドウ + 3 ペイン (sidebar/canvas/terminal) 作成");

    // xterm.js が ready になるまで PTY 出力を buffer
    // (ConPTY は起動直後に DSR \x1b[6n を送ってきて xterm の応答を待つため、
    //  ready 前の bytes を欠落させると shell が永久に block する)
    let mut xterm_ready = false;
    let mut pending: Vec<u8> = Vec::new();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                tracing::info!("Window close requested");
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                update_pane_bounds(
                    &sidebar,
                    &canvas,
                    &terminal_view,
                    size,
                    window.scale_factor(),
                );
            }
            Event::UserEvent(TerminalEvent::Output(bytes)) => {
                if !xterm_ready {
                    // xterm がまだ起動中 → buffer
                    pending.extend_from_slice(&bytes);
                    tracing::debug!(
                        "PTY output buffered ({} bytes, pending total={})",
                        bytes.len(),
                        pending.len()
                    );
                } else {
                    let script = terminal::build_output_script(&bytes);
                    if let Err(e) = terminal_view.evaluate_script(&script) {
                        tracing::warn!("terminal evaluate_script 失敗: {}", e);
                    }
                }
            }
            Event::UserEvent(TerminalEvent::XtermReady) => {
                xterm_ready = true;
                if !pending.is_empty() {
                    tracing::info!("xterm ready → flush {} 保留バイト", pending.len());
                    let script = terminal::build_output_script(&pending);
                    if let Err(e) = terminal_view.evaluate_script(&script) {
                        tracing::warn!("terminal flush 失敗: {}", e);
                    }
                    pending.clear();
                }
            }
            _ => {}
        }
    });
}
