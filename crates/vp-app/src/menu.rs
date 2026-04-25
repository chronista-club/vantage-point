//! muda メニューバー
//!
//! macOS: NSApp menu (Cmd+Q 等)。Windows/Linux: in-window menubar。
//!
//! VP-100 follow-up: 1Password 風の「開発者モード」設定を View メニューに追加。
//! settings file に永続化、runtime で即時切替 (`Open Developer Tools` の有効/無効が連動)。

use muda::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// MenuEvent dispatch で使う MenuId 群
pub struct MenuIds {
    /// View → "Developer Mode" (CheckMenuItem)
    pub developer_mode: MenuId,
    /// View → "Open Developer Tools" (MenuItem、developer_mode == true の時のみ enabled)
    pub open_devtools: MenuId,
}

/// メニューバー + 動的に状態更新する item の handle
pub struct MenuHandles {
    pub menu: Menu,
    pub developer_mode_item: CheckMenuItem,
    pub open_devtools_item: MenuItem,
    pub ids: MenuIds,
}

/// 標準メニューバーを構築
///
/// `initial_dev_mode` で View → "Developer Mode" の初期 check 状態を設定し、
/// "Open Developer Tools" の enabled 状態も連動させる。
pub fn build_menu_bar(initial_dev_mode: bool) -> MenuHandles {
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

    // View メニュー — VP-100 follow-up で開発者モード設定を追加
    //
    // 1Password 風 UX:
    //  - "Developer Mode" check item: 設定で ON にすると "Open Developer Tools" が enabled に
    //  - "Open Developer Tools": dev_mode == true の時のみクリック可、`webview.open_devtools()` を呼ぶ
    //  - 切替は runtime で即時反映、settings file に永続化 (再起動後も保持)
    let developer_mode_item = CheckMenuItem::new("Developer Mode", true, initial_dev_mode, None);
    let open_devtools_item = MenuItem::new(
        "Open Developer Tools",
        initial_dev_mode, // 初期 enabled は dev_mode に従う
        None,
    );
    let view_menu = Submenu::new("View", true);
    view_menu
        .append(&developer_mode_item)
        .expect("append Developer Mode");
    view_menu
        .append(&PredefinedMenuItem::separator())
        .expect("append separator");
    view_menu
        .append(&open_devtools_item)
        .expect("append Open Developer Tools");

    menu.append(&app_menu).expect("append App menu");
    menu.append(&file_menu).expect("append File menu");
    menu.append(&edit_menu).expect("append Edit menu");
    menu.append(&view_menu).expect("append View menu");

    let ids = MenuIds {
        developer_mode: developer_mode_item.id().clone(),
        open_devtools: open_devtools_item.id().clone(),
    };

    MenuHandles {
        menu,
        developer_mode_item,
        open_devtools_item,
        ids,
    }
}
