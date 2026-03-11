//! Canvas ペインの状態管理
//!
//! Unison QUIC 経由で Process サーバーからリアルタイム受信した
//! Show/Clear イベントを保持し、TUI 内での描画に使用する。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui_image::StatefulImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tui_scrollview::{ScrollView, ScrollViewState};

use crate::protocol::{Content, ProcessMessage};

use super::theme::*;

/// Canvas ペインの状態（Unison 経由でリアルタイム受信）
pub struct CanvasState {
    /// pane_id → (title, content)
    pub panes: HashMap<String, (Option<String>, Content)>,
    /// 画像プロトコル状態（pane_id → StatefulProtocol）
    pub images: HashMap<String, StatefulProtocol>,
    /// 画像プロトコル Picker（ターミナル検出結果をキャッシュ）
    picker: Option<Picker>,
    /// スクロール状態
    pub scroll_state: ScrollViewState,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            panes: HashMap::new(),
            images: HashMap::new(),
            picker: Picker::from_query_stdio().ok(),
            scroll_state: ScrollViewState::default(),
        }
    }
}

impl CanvasState {
    /// ProcessMessage を適用
    pub fn apply(&mut self, msg: &ProcessMessage) {
        match msg {
            ProcessMessage::Show {
                pane_id,
                content,
                append,
                title,
            } => {
                if *append {
                    if let Some((existing_title, existing_content)) = self.panes.get_mut(pane_id) {
                        *existing_content = existing_content.append_with(content);
                        if title.is_some() {
                            *existing_title = title.clone();
                        }
                    } else {
                        self.panes
                            .insert(pane_id.clone(), (title.clone(), content.clone()));
                    }
                } else {
                    self.panes
                        .insert(pane_id.clone(), (title.clone(), content.clone()));
                }

                // 画像コンテンツの場合、プロトコル状態を更新
                if let Content::ImageBase64 { data, .. } = content {
                    self.update_image(pane_id, data);
                }
            }
            ProcessMessage::Clear { pane_id } => {
                self.panes.remove(pane_id);
                self.images.remove(pane_id);
            }
            _ => {}
        }
    }

    /// Base64 画像データからプロトコル状態を生成
    fn update_image(&mut self, pane_id: &str, data: &str) {
        let Some(picker) = &mut self.picker else {
            return;
        };

        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        if let Ok(bytes) = engine.decode(data)
            && let Ok(img) = image::load_from_memory(&bytes)
        {
            let protocol = picker.new_resize_protocol(img);
            self.images.insert(pane_id.to_string(), protocol);
        }
    }

    /// スクロールアップ（3行分）
    pub fn scroll_up(&mut self) {
        for _ in 0..3 {
            self.scroll_state.scroll_up();
        }
    }

    /// スクロールダウン（3行分）
    pub fn scroll_down(&mut self) {
        for _ in 0..3 {
            self.scroll_state.scroll_down();
        }
    }
}

/// Unison QUIC で Process サーバーの "canvas" チャネルに接続し、
/// Show/Clear イベントを受信するスレッドを起動
pub fn spawn_canvas_receiver(
    port: u16,
    canvas_state: Arc<Mutex<CanvasState>>,
) -> Option<std::thread::JoinHandle<()>> {
    let handle = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };

        rt.block_on(async {
            let quic_port = port + crate::process::unison_server::QUIC_PORT_OFFSET;
            let addr = format!("[::1]:{}", quic_port);

            let client = match unison::ProtocolClient::new_default() {
                Ok(c) => c,
                Err(_) => return,
            };

            let mut attempts = 0;
            loop {
                match client.connect(&addr).await {
                    Ok(_) => break,
                    Err(_) => {
                        attempts += 1;
                        if attempts >= 5 {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }

            let channel = match client.open_channel("canvas").await {
                Ok(ch) => ch,
                Err(_) => return,
            };

            while let Ok(msg) = channel.recv().await {
                let payload = msg.payload_as_value().unwrap_or_default();
                if let Ok(process_msg) = serde_json::from_value::<ProcessMessage>(payload) {
                    let mut state = canvas_state.lock().unwrap();
                    state.apply(&process_msg);
                }
            }
        });
    });
    Some(handle)
}

/// Canvas ペインの描画（tui-markdown + scrollview + image）
pub fn render_canvas(frame: &mut ratatui::Frame, area: Rect, state: &mut CanvasState) {
    if state.panes.is_empty() {
        let placeholder = Paragraph::new(vec![
            Line::from(Span::styled(
                "Canvas ready",
                Style::default().fg(NORD_COMMENT),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "MCP show で内容が表示されます",
                Style::default().fg(NORD_COMMENT),
            )),
        ]);
        frame.render_widget(placeholder, area);
        return;
    }

    let content_width = area.width;
    let mut total_height: u16 = 0;

    let mut pane_ids: Vec<_> = state.panes.keys().cloned().collect();
    pane_ids.sort();

    for (i, pane_id) in pane_ids.iter().enumerate() {
        if let Some((_, content)) = state.panes.get(pane_id) {
            if i > 0 {
                total_height += 3;
            }
            total_height += 2;
            match content {
                Content::Markdown(text) | Content::Log(text) | Content::Html(text) => {
                    let rendered = tui_markdown::from_str(text);
                    total_height += rendered.height() as u16;
                }
                Content::ImageBase64 { .. } => {
                    total_height += area.height.saturating_sub(4).max(8);
                }
                Content::Url(_) => {
                    total_height += 1;
                }
            }
        }
    }

    let content_size = ratatui::layout::Size::new(content_width, total_height.max(1));
    let mut scroll_view = ScrollView::new(content_size);
    let mut y: u16 = 0;

    for (i, pane_id) in pane_ids.iter().enumerate() {
        if let Some((title, content)) = state.panes.get(pane_id) {
            if i > 0 {
                let sep = Paragraph::new(Line::from(Span::styled(
                    "─".repeat(content_width as usize),
                    Style::default().fg(NORD_COMMENT),
                )));
                scroll_view.render_widget(sep, Rect::new(0, y + 1, content_width, 1));
                y += 3;
            }

            let display_title = title.as_deref().unwrap_or(pane_id);
            let title_widget = Paragraph::new(Line::from(Span::styled(
                format!("▎ {}", display_title),
                Style::default().fg(NORD_CYAN).add_modifier(Modifier::BOLD),
            )));
            scroll_view.render_widget(title_widget, Rect::new(0, y, content_width, 1));
            y += 2;

            match content {
                Content::Markdown(text) => {
                    let rendered = tui_markdown::from_str(text);
                    let h = rendered.height() as u16;
                    scroll_view.render_widget(
                        Paragraph::new(rendered).wrap(ratatui::widgets::Wrap { trim: false }),
                        Rect::new(0, y, content_width, h.max(1)),
                    );
                    y += h;
                }
                Content::Log(text) | Content::Html(text) => {
                    let rendered = tui_markdown::from_str(text);
                    let h = rendered.height() as u16;
                    scroll_view.render_widget(
                        Paragraph::new(rendered).wrap(ratatui::widgets::Wrap { trim: false }),
                        Rect::new(0, y, content_width, h.max(1)),
                    );
                    y += h;
                }
                Content::ImageBase64 { .. } => {
                    let img_height = area.height.saturating_sub(4).max(8);
                    let img_rect = Rect::new(0, y, content_width, img_height);
                    if let Some(protocol) = state.images.get_mut(pane_id) {
                        let img_widget = StatefulImage::default();
                        scroll_view.render_stateful_widget(img_widget, img_rect, protocol);
                    } else {
                        scroll_view.render_widget(
                            Paragraph::new(Span::styled(
                                "[Image loading...]",
                                Style::default().fg(NORD_GREEN),
                            )),
                            img_rect,
                        );
                    }
                    y += img_height;
                }
                Content::Url(url) => {
                    scroll_view.render_widget(
                        Paragraph::new(Span::styled(
                            format!("→ {}", url),
                            Style::default().fg(NORD_GREEN),
                        )),
                        Rect::new(0, y, content_width, 1),
                    );
                    y += 1;
                }
            }
        }
    }

    frame.render_stateful_widget(scroll_view, area, &mut state.scroll_state);
}
