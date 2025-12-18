//! System tray icon for vp
//!
//! Provides a menu bar icon that shows running instances and allows control.

use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{
    TrayIconBuilder,
    menu::{MenuEvent, MenuId},
};

/// Menu item IDs
const QUIT_ID: &str = "quit";
const REFRESH_ID: &str = "refresh";
const OPEN_WEBUI_PREFIX: &str = "open_";
const STOP_PREFIX: &str = "stop_";

/// Running instance info (copied from main.rs for now)
#[derive(Clone)]
struct Instance {
    port: u16,
    pid: u32,
    project_dir: Option<String>,
}

/// Scan for running vp instances
async fn scan_instances() -> Vec<Instance> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap();

    let mut instances = Vec::new();

    for port in 33000..=33010 {
        let url = format!("http://localhost:{}/api/health", port);
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
                && let Ok(health) = response.json::<serde_json::Value>().await {
                    instances.push(Instance {
                        port,
                        pid: health["pid"].as_u64().unwrap_or(0) as u32,
                        project_dir: health["project_dir"].as_str().map(String::from),
                    });
                }
    }

    instances
}

/// Stop a vp instance
async fn stop_instance(port: u16) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://localhost:{}/api/shutdown", port);
    let _ = client.post(&url).send().await;
}

/// Create the tray menu
fn create_tray_menu(instances: &[Instance]) -> tray_icon::menu::Menu {
    let menu = tray_icon::menu::Menu::new();

    if instances.is_empty() {
        let no_instances = tray_icon::menu::MenuItem::new("No running instances", false, None);
        menu.append(&no_instances).ok();
    } else {
        for inst in instances {
            let project_name = inst
                .project_dir
                .as_ref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("unknown");

            // Submenu for each instance
            let instance_menu =
                tray_icon::menu::Submenu::new(format!("{} (:{}) ", project_name, inst.port), true);

            let open_item = tray_icon::menu::MenuItem::with_id(
                MenuId::new(format!("{}{}", OPEN_WEBUI_PREFIX, inst.port)),
                "Open WebUI",
                true,
                None,
            );
            let stop_item = tray_icon::menu::MenuItem::with_id(
                MenuId::new(format!("{}{}", STOP_PREFIX, inst.port)),
                "Stop",
                true,
                None,
            );

            instance_menu.append(&open_item).ok();
            instance_menu.append(&stop_item).ok();
            menu.append(&instance_menu).ok();
        }
    }

    menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
        .ok();

    let refresh_item =
        tray_icon::menu::MenuItem::with_id(MenuId::new(REFRESH_ID), "Refresh", true, None);
    menu.append(&refresh_item).ok();

    menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
        .ok();

    let quit_item = tray_icon::menu::MenuItem::with_id(MenuId::new(QUIT_ID), "Quit", true, None);
    menu.append(&quit_item).ok();

    menu
}

/// Simple icon (a colored square) - in production would use a proper icon file
fn create_icon() -> tray_icon::Icon {
    // Create a simple 22x22 icon (standard macOS menu bar size)
    let width = 22u32;
    let height = 22u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    // Draw a simple "V" shape
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            // Simple gradient for visibility
            let in_v = (x as i32 - 11).abs() < (y as i32 / 2 + 2) && y > 4 && y < 18;
            if in_v {
                rgba[idx] = 100; // R
                rgba[idx + 1] = 149; // G
                rgba[idx + 2] = 237; // B - Cornflower blue
                rgba[idx + 3] = 255; // A
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, width, height).expect("Failed to create icon")
}

/// Run the system tray
pub fn run_tray() -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::new().build();

    // Initial scan
    let rt = tokio::runtime::Runtime::new()?;
    let instances = rt.block_on(scan_instances());

    let menu = create_tray_menu(&instances);
    let icon = create_icon();

    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Vantage Point")
        .with_icon(icon)
        .build()?;

    tracing::info!("System tray started");

    let menu_channel = MenuEvent::receiver();

    event_loop.run(move |_event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Ok(event) = menu_channel.try_recv() {
            let id = event.id.0.as_str();

            if id == QUIT_ID {
                *control_flow = ControlFlow::Exit;
            } else if id == REFRESH_ID {
                // Refresh instances
                tracing::info!("Refreshing instances...");
                // Note: In a real implementation, we'd update the menu here
            } else if let Some(port_str) = id.strip_prefix(OPEN_WEBUI_PREFIX) {
                if let Ok(port) = port_str.parse::<u16>() {
                    // Open WebView window for existing Stand instance
                    if let Err(e) = crate::webview::run_webview_detached(port) {
                        tracing::error!("Failed to open WebView: {}", e);
                        // Fallback to browser
                        let url = format!("http://localhost:{}", port);
                        let _ = open::that(&url);
                    }
                }
            } else if let Some(port_str) = id.strip_prefix(STOP_PREFIX)
                && let Ok(port) = port_str.parse::<u16>() {
                    tracing::info!("Stopping instance on port {}...", port);
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(stop_instance(port));
                }
        }
    });
}
