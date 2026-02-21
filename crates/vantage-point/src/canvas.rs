//! Canvas ウィンドウ（WebViewのみ）
//!
//! StandのWeb UIをスタンドアロンウィンドウで表示。
//! ターミナルとは独立したウィンドウで、フォーカス干渉なし。

use tao::dpi::LogicalSize;

use crate::terminal_window::create_menu_bar;

/// 別プロセスで Canvas ウィンドウを起動
pub fn run_canvas_detached(port: u16) -> anyhow::Result<()> {
    std::process::Command::new("vp")
        .args(["canvas", "--port", &port.to_string()])
        .spawn()?;
    Ok(())
}

/// キャンバスウィンドウ（WebViewのみ、ターミナルなし）
///
/// Standの Web UIをスタンドアロンウィンドウで表示。
/// ターミナルとは独立したウィンドウで、フォーカス干渉なし。
pub fn run_canvas(port: u16) -> anyhow::Result<()> {
    use tao::{
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    let event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title("Vantage Point Canvas")
        .with_inner_size(LogicalSize::new(800.0, 900.0))
        .build(&event_loop)?;

    // メニューバー（コピー/ペースト対応）
    let menu = create_menu_bar();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    let url = format!("http://localhost:{}", port);

    let _webview = WebViewBuilder::new()
        .with_url(&url)
        .with_devtools(true)
        .build(&window)?;

    tracing::info!("Canvas window opened: {} (port={})", url, port);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }

        let _ = &_webview;
        let _ = &menu;
    });
}
