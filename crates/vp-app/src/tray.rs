//! tray-icon — 常駐トレイアイコン
//!
//! Phase W3 で daemon 稼働状態の表示 + repo 切替を追加予定。

use tray_icon::{TrayIcon, TrayIconBuilder, menu::Menu as TrayMenu};

/// トレイ用のアイコン (portal の山アイコン)。
///
/// 旧実装は 1x1 の透明 dummy (Phase W1 scaffold) で、 実質「見えないトレイ」だった。
/// 32px は Windows の通知領域 / mac の menu bar が引く実寸に合わせた値。
///
/// decode に失敗した場合は透明 1px に落として「トレイ自体は出す」— アイコン欠けで
/// Quit メニューまで失うのは損なので、 tray の生成は妨げない。
fn tray_icon_image() -> tray_icon::Icon {
    if let Some((rgba, w, h)) = crate::icon::icon_rgba(32)
        && let Ok(icon) = tray_icon::Icon::from_rgba(rgba, w, h)
    {
        return icon;
    }
    tracing::warn!(target: "vp_app::tray", "tray icon の生成に失敗 — 透明アイコンで継続");
    tray_icon::Icon::from_rgba(vec![0u8, 0, 0, 0], 1, 1).expect("1x1 icon")
}

/// トレイアイコンを構築
pub fn build_tray() -> anyhow::Result<TrayIcon> {
    let menu = TrayMenu::new();
    let quit = tray_icon::menu::MenuItem::new("Quit", true, None);
    menu.append(&quit)?;

    let tray = TrayIconBuilder::new()
        .with_tooltip("Vantage Point")
        .with_icon(tray_icon_image())
        .with_menu(Box::new(menu))
        .build()?;

    Ok(tray)
}
