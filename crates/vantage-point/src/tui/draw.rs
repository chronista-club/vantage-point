//! TUI 描画関数
//!
//! プロジェクト選択画面、セッション選択画面の描画。

use std::time::Duration;

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::config::{Config, ProjectConfig};

use super::session::ClaudeSession;
use super::theme::*;

// =============================================================================
// プロジェクト選択画面
// =============================================================================

/// プロジェクト選択画面の描画
pub fn draw_project_select(
    frame: &mut ratatui::Frame,
    projects: &[ProjectConfig],
    list_state: &mut ListState,
) {
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(NORD_BG)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .margin(1)
        .split(area);

    // タイトル
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " Vantage Point ",
            Style::default().fg(NORD_CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" — プロジェクト選択", Style::default().fg(NORD_FG)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(NORD_COMMENT)),
    );
    frame.render_widget(title, chunks[0]);

    // プロジェクトリスト
    let items: Vec<ListItem> = projects
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let running = crate::discovery::find_by_project_blocking(&Config::normalize_path(
                std::path::Path::new(&p.path),
            ));
            let status = if running.is_some() {
                Span::styled(" [running] ", Style::default().fg(NORD_GREEN))
            } else {
                Span::raw("")
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", i + 1), Style::default().fg(NORD_COMMENT)),
                Span::styled(&p.name, Style::default().fg(NORD_FG)),
                status,
                Span::styled(format!("  {}", p.path), Style::default().fg(NORD_COMMENT)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(NORD_POLAR)
                .fg(NORD_CYAN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, chunks[1], list_state);

    // ヘルプバー
    let help = Line::from(vec![
        Span::styled(" Enter", Style::default().fg(NORD_CYAN)),
        Span::styled(": 選択  ", Style::default().fg(NORD_COMMENT)),
        Span::styled("j/k", Style::default().fg(NORD_CYAN)),
        Span::styled(": 移動  ", Style::default().fg(NORD_COMMENT)),
        Span::styled("q", Style::default().fg(NORD_CYAN)),
        Span::styled(": 終了", Style::default().fg(NORD_COMMENT)),
    ]);
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(NORD_POLAR)),
        chunks[2],
    );
}

// =============================================================================
// セッション選択画面
// =============================================================================

/// セッション選択画面の描画
pub fn draw_session_select(
    frame: &mut ratatui::Frame,
    sessions: &[ClaudeSession],
    list_state: &mut ListState,
) {
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(NORD_BG)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .margin(1)
        .split(area);

    // タイトル
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " Vantage Point ",
            Style::default().fg(NORD_CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" — セッション選択", Style::default().fg(NORD_FG)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(NORD_COMMENT)),
    );
    frame.render_widget(title, chunks[0]);

    // セッションリスト
    let mut items: Vec<ListItem> = Vec::new();

    items.push(ListItem::new(Line::from(vec![
        Span::styled(" ▶ ", Style::default().fg(NORD_GREEN)),
        Span::styled(
            "前回の続き",
            Style::default().fg(NORD_FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" (--continue)", Style::default().fg(NORD_COMMENT)),
    ])));

    items.push(ListItem::new(Line::from(vec![
        Span::styled(" + ", Style::default().fg(NORD_CYAN)),
        Span::styled("新規セッション", Style::default().fg(NORD_FG)),
    ])));

    for session in sessions.iter().take(20) {
        let elapsed = session
            .modified
            .elapsed()
            .map(format_elapsed)
            .unwrap_or_else(|_| "?".to_string());

        let summary_text = if session.summary.is_empty() {
            "(no messages)".to_string()
        } else {
            session.summary.clone()
        };

        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!(" {} ", elapsed), Style::default().fg(NORD_COMMENT)),
            Span::styled(summary_text, Style::default().fg(NORD_FG)),
            Span::styled(
                format!("  ~{}msgs", session.message_count),
                Style::default().fg(NORD_COMMENT),
            ),
        ])));
    }

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(NORD_POLAR)
                .fg(NORD_CYAN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, chunks[1], list_state);

    // ヘルプバー
    let help = Line::from(vec![
        Span::styled(" Enter", Style::default().fg(NORD_CYAN)),
        Span::styled(": 選択  ", Style::default().fg(NORD_COMMENT)),
        Span::styled("j/k", Style::default().fg(NORD_CYAN)),
        Span::styled(": 移動  ", Style::default().fg(NORD_COMMENT)),
        Span::styled("Esc", Style::default().fg(NORD_CYAN)),
        Span::styled(": 前回の続き", Style::default().fg(NORD_COMMENT)),
    ]);
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(NORD_POLAR)),
        chunks[2],
    );
}

// =============================================================================
// ユーティリティ
// =============================================================================

/// 経過時間を人間に読みやすい形式で表示
pub fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// PTY サイズ計算（フルスクリーン — ヘッダ・フッタ・ボーダーなし）
pub fn calc_pty_size(term_width: u16, term_height: u16) -> (usize, usize) {
    let cols = (term_width as usize).max(1);
    let lines = (term_height as usize).max(1);
    (cols, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_just_now() {
        assert_eq!(format_elapsed(Duration::from_secs(30)), "just now");
    }

    #[test]
    fn format_elapsed_minutes() {
        assert_eq!(format_elapsed(Duration::from_secs(300)), "5m ago");
    }

    #[test]
    fn format_elapsed_hours() {
        assert_eq!(format_elapsed(Duration::from_secs(7200)), "2h ago");
    }

    #[test]
    fn format_elapsed_days() {
        assert_eq!(format_elapsed(Duration::from_secs(172800)), "2d ago");
    }

    #[test]
    fn calc_pty_size_full() {
        let (cols, lines) = calc_pty_size(80, 24);
        assert_eq!(cols, 80); // フルスクリーン
        assert_eq!(lines, 24); // ヘッダ・フッタ・ボーダーなし
    }

    #[test]
    fn calc_pty_size_tiny_terminal() {
        let (cols, lines) = calc_pty_size(5, 3);
        assert!(cols >= 1);
        assert!(lines >= 1);
    }
}
