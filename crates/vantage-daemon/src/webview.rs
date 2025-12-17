//! Native WebView window using wry
//!
//! Opens a native window with embedded WebView instead of requiring a browser.
//!
//! DevTools: Press Cmd+Option+I (macOS) or F12 to open

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::KeyCode,
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

    let webview = WebViewBuilder::new()
        .with_url(&url)
        .with_devtools(true) // Always enable devtools for debugging
        .build(&window)?;

    tracing::info!("WebView window opened: {}", url);
    tracing::info!("Press Cmd+Option+I to open DevTools");

    // Open DevTools immediately in debug builds
    #[cfg(debug_assertions)]
    {
        webview.open_devtools();
    }

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
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event, .. },
                ..
            } => {
                // Cmd+Option+I or F12 to toggle DevTools
                if event.state == tao::event::ElementState::Pressed {
                    let is_devtools_shortcut =
                        // F12
                        event.physical_key == KeyCode::F12 ||
                        // Cmd+Option+I (macOS)
                        (event.physical_key == KeyCode::KeyI
                            && event.state == tao::event::ElementState::Pressed);

                    if is_devtools_shortcut {
                        if webview.is_devtools_open() {
                            webview.close_devtools();
                        } else {
                            webview.open_devtools();
                        }
                    }
                }
            }
            _ => {}
        }
    });
}
