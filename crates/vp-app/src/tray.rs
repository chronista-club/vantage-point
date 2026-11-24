//! tray-icon — 常駐トレイアイコン
//!
//! engine usage meter（spec creo `mem_1ChMLKCMUvC73JHTLjG3xg`）: macOS の menu bar に
//! **最も逼迫している engine の使用率**（`Cdx 95%`）を出し、メニューに 4 エンジンの使用率と
//! リセット時刻、`Refresh Usage` を並べる。値は daemon "daemon-usage" の push で live 更新
//! （[`Tray::apply_usage`]、event loop の `AppEvent::UsageUpdated` から）。
//!
//! 表示文字列の組み立ては [`crate::usage`] の純関数（test はそちら）。ここは muda / tray-icon の
//! handle を持って差し替えるだけ。`set_title` は macOS 以外では no-op（tray-icon の仕様）。

use tray_icon::menu::{Menu as TrayMenu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::usage::UsageSnapshot;

/// usage 行を並べる engine（daemon の snapshot と同じ順）。
const USAGE_ENGINES: [&str; 4] = ["claude", "codex", "agy", "grok"];

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

/// トレイアイコンと、usage 表示のために差し替える menu item の handle。
pub struct Tray {
    icon: TrayIcon,
    /// (engine, 行)。押せない（情報表示だけ）。
    usage_items: Vec<(&'static str, MenuItem)>,
    /// `Refresh Usage` の id（event loop の `MenuClicked` で照合する）。
    pub refresh_usage_id: MenuId,
}

impl Tray {
    /// snapshot を menu bar の文字とメニュー行に反映する。
    pub fn apply_usage(&self, snapshot: &UsageSnapshot) {
        self.icon.set_title(crate::usage::tray_title(snapshot));
        let offset = *chrono::Local::now().offset();
        for (engine, item) in &self.usage_items {
            let text = match snapshot.engines.iter().find(|e| e.engine == *engine) {
                Some(usage) => crate::usage::tray_line(usage, offset),
                None => format!("{} — (Refresh で取得)", crate::usage::display_name(engine)),
            };
            item.set_text(text);
        }
    }
}

/// トレイアイコンを構築
pub fn build_tray(ids: &crate::menu::MenuIds) -> anyhow::Result<Tray> {
    let menu = TrayMenu::new();
    let mut usage_items = Vec::new();
    for engine in USAGE_ENGINES {
        let item = MenuItem::new(
            format!("{} — …", crate::usage::display_name(engine)),
            false,
            None,
        );
        menu.append(&item)?;
        usage_items.push((engine, item));
    }
    let refresh = MenuItem::new("Refresh Usage", true, None);
    menu.append(&refresh)?;
    menu.append(&PredefinedMenuItem::separator())?;
    let quit = MenuItem::with_id(ids.quit.clone(), "Quit Vantage Point", true, None);
    let new_window = MenuItem::with_id(ids.new_window.clone(), "New Window", true, None);
    menu.append(&new_window)?;
    menu.append(&quit)?;

    let icon = TrayIconBuilder::new()
        .with_tooltip("Vantage Point")
        .with_icon(tray_icon_image())
        .with_menu(Box::new(menu))
        .build()?;

    Ok(Tray {
        icon,
        usage_items,
        refresh_usage_id: refresh.id().clone(),
    })
}
