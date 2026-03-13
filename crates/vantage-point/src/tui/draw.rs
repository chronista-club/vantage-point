//! TUI 描画関数
//!
//! ヘッダバー、フッターバー、プロジェクト選択画面、セッション選択画面の描画。

use std::time::Duration;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::config::{Config, ProjectConfig};

use super::session::ClaudeSession;
use super::theme::*;

// =============================================================================
// ヘッダバー
// =============================================================================

/// ヘッダバー描画（マルチプロジェクトタブ付き）
pub fn draw_header_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    tabs: &[(String, bool, u32, bool)], // (name, is_active, notifications, completed)
    port: u16,
    ai_busy: bool,
    pp_open: bool,
    connection_status: &str,
) {
    let mut spans: Vec<Span> = Vec::new();

    // プロジェクトタブ
    for (i, (name, is_active, notif, completed)) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "│",
                Style::default().fg(NORD_COMMENT).bg(NORD_POLAR),
            ));
        }

        // タブテキスト構築: ★（完了）+ ●N（通知）
        let badge = match (*completed, *notif > 0) {
            (true, true) => format!(" ★●{}", notif),
            (true, false) => " ★".to_string(),
            (false, true) => format!(" ●{}", notif),
            (false, false) => String::new(),
        };
        let tab_text = format!(" {}{} ", name, badge);

        if *is_active {
            spans.push(Span::styled(
                tab_text,
                Style::default()
                    .fg(NORD_BG)
                    .bg(NORD_CYAN)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            let fg = if *completed {
                NORD_GREEN
            } else if *notif > 0 {
                NORD_YELLOW
            } else {
                NORD_FG
            };
            spans.push(Span::styled(
                tab_text,
                Style::default().fg(fg).bg(NORD_POLAR),
            ));
        }
    }

    // Stand ステータス
    let sep = Span::styled(" ", Style::default().bg(NORD_POLAR));
    spans.push(sep.clone());

    // ⭐ Star Platinum — Process 接続状態
    let (sp_text, sp_color) = if connection_status.starts_with("エラー") {
        ("⭐✗", NORD_RED)
    } else if connection_status == "接続済み" {
        ("⭐", NORD_GREEN)
    } else {
        ("⭐…", NORD_YELLOW)
    };
    spans.push(Span::styled(
        sp_text,
        Style::default().fg(sp_color).bg(NORD_POLAR),
    ));
    spans.push(sep.clone());

    // 🧭 Paisley Park — PP window 状態（呼び出し元からキャッシュ値を受け取る）
    let (pp_text, pp_color) = if pp_open {
        ("🧭", NORD_GREEN)
    } else {
        ("🧭", NORD_COMMENT)
    };
    spans.push(Span::styled(
        pp_text,
        Style::default().fg(pp_color).bg(NORD_POLAR),
    ));

    // 右端: ポート + 📖 AI 状態
    let (hd_text, hd_color) = if ai_busy {
        (" 📖応答中   ", NORD_YELLOW)
    } else {
        (" 📖入力待ち ", NORD_GREEN)
    };

    let port_span = Span::styled(
        format!(" :{} ", port),
        Style::default().fg(NORD_COMMENT).bg(NORD_POLAR),
    );
    let hd_span = Span::styled(hd_text, Style::default().fg(hd_color).bg(NORD_POLAR));

    let left_width: usize = spans.iter().map(|s| s.width()).sum();
    let right_width = port_span.width() + hd_span.width();
    let gap = (area.width as usize).saturating_sub(left_width + right_width);

    spans.push(Span::styled(
        " ".repeat(gap),
        Style::default().bg(NORD_POLAR),
    ));
    spans.push(port_span);
    spans.push(hd_span);

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}

// =============================================================================
// フッターバー
// =============================================================================

/// フッターバー描画（マルチプロジェクト対応）
pub fn draw_footer_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    project_count: usize,
    session_count: usize,
) {
    let key_style = Style::default().fg(NORD_CYAN).bg(NORD_POLAR);
    let desc_style = Style::default().fg(NORD_COMMENT).bg(NORD_POLAR);

    let mut spans = vec![
        Span::styled(" C-p", key_style),
        Span::styled(" projects ", desc_style),
    ];

    if project_count > 1 {
        spans.push(Span::styled(" C-←/→", key_style));
        spans.push(Span::styled(" switch ", desc_style));
    }

    spans.push(Span::styled(" Home", key_style));
    spans.push(Span::styled(" 🧭canvas ", desc_style));
    spans.push(Span::styled(" C-q", key_style));
    spans.push(Span::styled(" quit ", desc_style));
    spans.push(Span::styled(" PgUp/Dn", key_style));
    spans.push(Span::styled(" scroll ", desc_style));

    if session_count > 1 {
        spans.push(Span::styled(" C-S-←/→", key_style));
        spans.push(Span::styled(" session ", desc_style));
    }
    spans.push(Span::styled(" C-n", key_style));
    spans.push(Span::styled(" new ", desc_style));
    spans.push(Span::styled(" C-T", key_style));
    spans.push(Span::styled(" add tab ", desc_style));

    if project_count > 1 {
        spans.push(Span::styled(" C-W", key_style));
        spans.push(Span::styled(" close tab ", desc_style));
    }

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}

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

/// PTY サイズ計算（常にフルワイド — inline Canvas 分割は廃止）
pub fn calc_pty_size(term_width: u16, term_height: u16) -> (usize, usize) {
    // ヘッダ（1行）+ フッター（1行）+ PTY ブロック枠上下（各1セル）
    let lines = (term_height.saturating_sub(4) as usize).max(1);
    let cols = term_width.saturating_sub(2) as usize;
    (cols.max(1), lines)
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
        assert_eq!(cols, 78); // 80 - 2 (borders)
        assert_eq!(lines, 20); // 24 - 4 (header+footer+borders)
    }

    #[test]
    fn calc_pty_size_tiny_terminal() {
        let (cols, lines) = calc_pty_size(5, 3);
        assert!(cols >= 1);
        assert!(lines >= 1);
    }
}
