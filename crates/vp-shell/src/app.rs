//! Main EventLoop + window lifecycle
//!
//! ## アーキテクチャ方針 (Mac 版と同等)
//!
//! 「ネイティブ層ベース + WebUI on top」のハイブリッド構成:
//!
//! ```text
//! ┌─── tao ネイティブウィンドウ (native chrome, menu, tray) ──┐
//! │ ┌──────────┬───────────────────────────────────────┐ │
//! │ │ sidebar  │                                        │ │
//! │ │ wry      │       canvas (wry WebView)             │ │
//! │ │ WebView  │                                        │ │
//! │ │          │                                        │ │
//! │ │ (~280px) │       (残り幅)                          │ │
//! │ └──────────┴───────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! - **ウィンドウ・メニュー・トレイ・レイアウト境界** は Rust (tao + muda + tray-icon)
//! - **各ペインの内容** は wry WebView (HTML/CSS/JS)
//! - Phase W2 で terminal ペインを xterm.js WebView として下段に追加予定

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{
    Rect, WebView, WebViewBuilder, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize,
};

/// Sidebar の固定幅 (LogicalPixel)
const SIDEBAR_WIDTH: f64 = 280.0;

const SIDEBAR_HTML: &str = r#"<!doctype html>
<html lang="ja"><head><meta charset="utf-8"><style>
  html,body{margin:0;height:100%;background:#1A2332;color:#D8DEE9;font-family:system-ui,-apple-system,"Segoe UI",sans-serif;}
  header{padding:16px;font-size:12px;color:#81A1C1;text-transform:uppercase;letter-spacing:.08em;}
  ul{list-style:none;margin:0;padding:0 8px;}
  li{padding:8px 12px;border-radius:6px;color:#ECEFF4;cursor:pointer;font-size:14px;}
  li:hover{background:#2C3E50;}
</style></head><body>
  <header>Projects</header>
  <ul>
    <li>(Phase W1 scaffold)</li>
    <li>vantage-point</li>
  </ul>
</body></html>"#;

const CANVAS_HTML: &str = r#"<!doctype html>
<html lang="ja"><head><meta charset="utf-8"><style>
  html,body{margin:0;height:100%;background:#0B1120;color:#ECEFF4;font-family:system-ui,-apple-system,"Segoe UI","Cascadia Code",monospace;}
  body{display:grid;place-items:center;}
  main{text-align:center;}
  h1{font-weight:500;font-size:2rem;margin:0 0 .5rem;}
  p{color:#81A1C1;margin:0;}
</style></head><body>
  <main>
    <h1>Vantage Point</h1>
    <p>Canvas pane — Phase W2 で xterm.js + Canvas HTML を連結予定</p>
  </main>
</body></html>"#;

/// Sidebar と Canvas の bounds を現在のウィンドウサイズから計算して両 WebView に適用。
fn update_pane_bounds(
    sidebar: &WebView,
    canvas: &WebView,
    window_size: tao::dpi::PhysicalSize<u32>,
    scale: f64,
) {
    let logical = window_size.to_logical::<f64>(scale);
    let width = logical.width;
    let height = logical.height;
    let canvas_x = SIDEBAR_WIDTH;
    let canvas_w = (width - SIDEBAR_WIDTH).max(0.0);

    let _ = sidebar.set_bounds(Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: WryLogicalSize::new(SIDEBAR_WIDTH, height).into(),
    });
    let _ = canvas.set_bounds(Rect {
        position: LogicalPosition::new(canvas_x, 0.0).into(),
        size: WryLogicalSize::new(canvas_w, height).into(),
    });
}

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    // tracing 初期化
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vp_shell=info".into()),
        )
        .init();

    tracing::info!("vp-shell 起動");

    let event_loop = EventLoopBuilder::new().build();

    // メニューバー構築 (macOS は NSApp、Windows は in-window menubar に muda が適用)
    let _menu = crate::menu::build_menu_bar();

    // トレイアイコン (Err 時は無効化して続行 — CI/headless 環境用)
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

    // Sidebar + Canvas の 2 つの WebView を親ウィンドウに貼る
    let sidebar = WebViewBuilder::new()
        .with_html(SIDEBAR_HTML)
        .with_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: WryLogicalSize::new(SIDEBAR_WIDTH, 800.0).into(),
        })
        .build_as_child(&window)?;

    let canvas = WebViewBuilder::new()
        .with_html(CANVAS_HTML)
        .with_bounds(Rect {
            position: LogicalPosition::new(SIDEBAR_WIDTH, 0.0).into(),
            size: WryLogicalSize::new(1200.0 - SIDEBAR_WIDTH, 800.0).into(),
        })
        .build_as_child(&window)?;

    tracing::info!("メインウィンドウ + 2 ペイン (sidebar/canvas) 作成");

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
                update_pane_bounds(&sidebar, &canvas, size, window.scale_factor());
            }
            _ => {}
        }
    });
}
