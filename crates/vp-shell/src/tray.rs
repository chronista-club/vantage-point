//! tray-icon — 常駐トレイアイコン
//!
//! Phase W1: ダミー icon + "Quit" メニュー。
//! Phase W3 で TheWorld 稼働状態の表示 + プロジェクト切替を追加予定。

use tray_icon::{TrayIcon, TrayIconBuilder, menu::Menu as TrayMenu};

/// 空の 1x1 ダミーアイコン (Phase W1 scaffold 用)。
///
/// 本物のアイコン asset は Phase W3 で入れ替え予定。
fn dummy_icon() -> tray_icon::Icon {
    // 1x1 RGBA (alpha=0) — 実害ない透明 1 px
    let rgba = vec![0u8, 0, 0, 0];
    tray_icon::Icon::from_rgba(rgba, 1, 1).expect("1x1 icon")
}

/// トレイアイコンを構築
pub fn build_tray() -> anyhow::Result<TrayIcon> {
    let menu = TrayMenu::new();
    let quit = tray_icon::menu::MenuItem::new("Quit", true, None);
    menu.append(&quit)?;

    let tray = TrayIconBuilder::new()
        .with_tooltip("Vantage Point")
        .with_icon(dummy_icon())
        .with_menu(Box::new(menu))
        .build()?;

    Ok(tray)
}
