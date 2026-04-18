//! オーバーレイ UI
//!
//! メイン画面の上にモーダル的に表示されるオーバーレイ。
//! プロジェクトスイッチャー、ペイン管理など。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use crate::config::ProjectConfig;

use super::theme::*;

/// オーバーレイ種別
pub enum OverlayKind {
    /// プロジェクト切替オーバーレイ
    ProjectSwitcher {
        list_state: ListState,
        /// config.projects のスナップショット（起動済みフラグ付き）
        items: Vec<(ProjectConfig, bool)>,
    },
    /// 新規タブ追加オーバーレイ（Ctrl+Shift+T）
    ProjectAdder {
        list_state: ListState,
        /// config.projects のスナップショット（起動済みフラグ付き）
        items: Vec<(ProjectConfig, bool)>,
    },
}

/// オーバーレイの描画ディスパッチ
pub fn draw_overlay(frame: &mut ratatui::Frame, area: Rect, overlay: &OverlayKind) {
    match overlay {
        OverlayKind::ProjectSwitcher { list_state, items } => {
            draw_project_switcher(frame, area, list_state, items, " Projects (C-p) ");
        }
        OverlayKind::ProjectAdder { list_state, items } => {
            draw_project_switcher(frame, area, list_state, items, " Add Tab (C-T) ");
        }
    }
}

/// プロジェクトスイッチャーオーバーレイの描画
fn draw_project_switcher(
    frame: &mut ratatui::Frame,
    area: Rect,
    list_state: &ListState,
    items: &[(ProjectConfig, bool)],
    title: &str,
) {
    // オーバーレイサイズ計算（中央配置）
    let overlay_width = 50u16.min(area.width.saturating_sub(4));
    let overlay_height = (items.len() as u16 + 4).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(overlay_width)) / 2;
    let y = area.y + (area.height.saturating_sub(overlay_height)) / 2;
    let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

    // Clear で背景を消す（擬似半透明）
    frame.render_widget(ratatui::widgets::Clear, overlay_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(NORD_CYAN))
        .style(Style::default().bg(NORD_BG));
    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|(project, active)| {
            let status = if *active {
                Span::styled(" ⭐ ", Style::default().fg(NORD_GREEN))
            } else {
                Span::styled("    ", Style::default())
            };

            ListItem::new(Line::from(vec![
                status,
                Span::styled(&project.name, Style::default().fg(NORD_FG)),
            ]))
        })
        .collect();

    let list = List::new(list_items)
        .highlight_style(
            Style::default()
                .bg(NORD_POLAR)
                .fg(NORD_CYAN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = *list_state;
    frame.render_stateful_widget(list, inner, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_area_calculation() {
        // オーバーレイが親エリア内に収まることを検証
        let area = Rect::new(0, 0, 80, 24);
        let items_count = 5u16;
        let overlay_width = 50u16.min(area.width.saturating_sub(4));
        let overlay_height = (items_count + 4).min(area.height.saturating_sub(4));
        let x = area.x + (area.width.saturating_sub(overlay_width)) / 2;
        let y = area.y + (area.height.saturating_sub(overlay_height)) / 2;

        assert!(x + overlay_width <= area.width);
        assert!(y + overlay_height <= area.height);
        assert_eq!(overlay_width, 50);
        assert_eq!(overlay_height, 9); // 5 + 4
    }

    #[test]
    fn overlay_area_small_terminal() {
        // 小さいターミナルでもパニックしない
        let area = Rect::new(0, 0, 30, 10);
        let overlay_width = 50u16.min(area.width.saturating_sub(4));
        let overlay_height = (8u16 + 4).min(area.height.saturating_sub(4));

        assert_eq!(overlay_width, 26); // 30 - 4
        assert_eq!(overlay_height, 6); // 10 - 4
    }
}
