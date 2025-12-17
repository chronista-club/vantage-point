//! Native WebView window using wry
//!
//! Opens a native window with embedded WebView instead of requiring a browser.

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

/// Run the WebView window pointing to the daemon's HTTP server
pub fn run_webview(port: u16) -> anyhow::Result<()> {
    let event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)?;

    let url = format!("http://localhost:{}", port);

    let _webview = WebViewBuilder::new()
        .with_url(&url)
        .with_devtools(cfg!(debug_assertions))
        .build(&window)?;

    tracing::info!("WebView window opened: {}", url);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                tracing::info!("WebView window closed");
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
