//! muda メニューバー
//!
//! macOS: NSApp menu (Cmd+Q 等)。Windows/Linux: in-window menubar。

use muda::{Menu, PredefinedMenuItem, Submenu};

/// 標準メニューバーを構築
pub fn build_menu_bar() -> Menu {
    let menu = Menu::new();

    // App メニュー (macOS では左端、Windows では File の前に隠れる)
    let app_menu = Submenu::with_items(
        "Vantage Point",
        true,
        &[
            &PredefinedMenuItem::about(Some("About Vantage Point"), None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(Some("Quit Vantage Point")),
        ],
    )
    .expect("Failed to build App menu");

    // File メニュー
    let file_menu = Submenu::with_items(
        "File",
        true,
        &[&PredefinedMenuItem::close_window(Some("Close Window"))],
    )
    .expect("Failed to build File menu");

    // Edit メニュー (copy / paste / select_all のシステム標準)
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
    .expect("Failed to build Edit menu");

    menu.append(&app_menu).expect("append App menu");
    menu.append(&file_menu).expect("append File menu");
    menu.append(&edit_menu).expect("append Edit menu");
    menu
}
