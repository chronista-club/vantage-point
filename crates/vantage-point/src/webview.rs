//! Native WebView window using wry
//!
//! Opens a native window with embedded WebView instead of requiring a browser.
//!
//! DevTools: Press Cmd+Option+I (macOS) or F12 to open

use muda::{Menu, PredefinedMenuItem, Submenu};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::KeyCode,
    window::WindowBuilder,
};
use wry::WebViewBuilder;

/// Create the application menu bar with Edit menu for copy/paste support
fn create_menu_bar() -> Menu {
    let menu = Menu::new();

    // Edit menu (required for Cmd+C/V to work on macOS)
    let edit_menu = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::select_all(None),
        ],
    )
    .expect("Failed to create Edit menu");

    menu.append(&edit_menu).expect("Failed to append Edit menu");

    menu
}

/// Launch WebView in a detached process
pub fn run_webview_detached(port: u16) -> anyhow::Result<()> {
    // Launch vp webview command as a separate process
    std::process::Command::new("vp")
        .args(["webview", "-p", &port.to_string()])
        .spawn()?;
    Ok(())
}

/// Run the WebView window pointing to the daemon's HTTP server
pub fn run_webview(port: u16) -> anyhow::Result<()> {
    let event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)?;

    // Initialize menu bar for macOS
    let menu = create_menu_bar();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

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
