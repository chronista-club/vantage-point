//! Native split window: Terminal (left) + WebView Dashboard (right)
//!
//! Arctic/Nordic + Ocean ダークテーマの分割ウィンドウ。
//! 左ペイン: TerminalView (alacritty_terminal + CoreText ネイティブレンダラー)
//! 右ペイン: wry WebView（既存のダッシュボード/ペインシステム）
//!
//! ## パイプライン
//! ```text
//! tmux → Stand (pipe-pane) → WebSocket → TerminalState → TerminalView
//! keyboard → WebSocket → Stand → tmux (send-keys)
//! ```
//!
//! DevTools: Press Cmd+Option+I (macOS) or F12 to open

use std::sync::mpsc;

use muda::{Menu, PredefinedMenuItem, Submenu};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    keyboard::KeyCode,
    window::WindowBuilder,
};
use wry::{
    Rect, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize as WryLogicalSize},
};

#[cfg(target_os = "macos")]
use crate::terminal::TerminalState;

/// 左ペイン（端末）の幅比率
const TERMINAL_RATIO: f64 = 0.55;

/// tao EventLoop に送るカスタムイベント
#[derive(Debug)]
enum TerminalEvent {
    /// 端末出力データ（VTバイトストリーム）
    Output(Vec<u8>),
    /// 端末セッション開始
    Ready,
}

/// WebSocket ブリッジに送るコマンド
enum WsBridgeCommand {
    /// 端末入力を送信（base64エンコード済み）
    Input(String),
    /// 端末リサイズ
    Resize { cols: u16, rows: u16 },
}

/// Create the application menu bar with Edit menu for copy/paste support
fn create_menu_bar() -> Menu {
    let menu = Menu::new();

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
    .expect("Failed to create Edit menu");

    menu.append(&edit_menu).expect("Failed to append Edit menu");
    menu
}

/// Launch WebView in a detached process
pub fn run_webview_detached(port: u16) -> anyhow::Result<()> {
    std::process::Command::new("vp")
        .args(["webview", "-p", &port.to_string()])
        .spawn()?;
    Ok(())
}

/// 分割レイアウトの座標を計算
struct SplitLayout {
    /// 左ペインの幅
    left_width: f64,
    /// 右ペインの幅
    right_width: f64,
    /// ウィンドウ全体の高さ
    height: f64,
}

impl SplitLayout {
    fn from_window_size(width: f64, height: f64) -> Self {
        let left_width = (width * TERMINAL_RATIO).floor();
        let right_width = width - left_width;
        Self {
            left_width,
            right_width,
            height,
        }
    }

    /// 右ペイン（WebView）の Rect
    fn webview_bounds(&self) -> Rect {
        Rect {
            position: LogicalPosition::new(self.left_width, 0.0).into(),
            size: WryLogicalSize::new(self.right_width, self.height).into(),
        }
    }

    /// 左ペイン（TerminalView）の NSRect
    #[cfg(target_os = "macos")]
    fn terminal_frame(&self) -> objc2_foundation::NSRect {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(self.left_width, self.height),
        )
    }
}

/// TerminalView を作成し、NSWindow の contentView に追加
#[cfg(target_os = "macos")]
fn setup_terminal_view(
    window: &tao::window::Window,
    layout: &SplitLayout,
) -> objc2::rc::Retained<crate::terminal::renderer::TerminalView> {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::NSColor;
    use tao::platform::macos::WindowExtMacOS;

    use crate::terminal::renderer::TerminalView;

    let mtm = MainThreadMarker::new().expect("メインスレッド上で実行する必要があります");

    // 端末グリッドサイズ
    let terminal_view = TerminalView::new(mtm, layout.terminal_frame(), 80, 24);

    // NSWindow の contentView に TerminalView を追加
    unsafe {
        let ns_window_ptr = window.ns_window() as *mut objc2_app_kit::NSWindow;
        let ns_window: Retained<objc2_app_kit::NSWindow> =
            Retained::retain(ns_window_ptr).expect("NSWindow が取得できません");

        // ウィンドウ背景: Arctic Deep Ocean (#0B1120)
        let bg_color = NSColor::colorWithSRGBRed_green_blue_alpha(
            11.0 / 255.0,
            17.0 / 255.0,
            32.0 / 255.0,
            1.0,
        );
        ns_window.setBackgroundColor(Some(&bg_color));

        if let Some(content_view) = ns_window.contentView() {
            content_view.addSubview(&terminal_view);
        }
    }

    tracing::info!(
        "TerminalView embedded: {}x{} cells in {:.0}x{:.0}px",
        80,
        24,
        layout.left_width,
        layout.height
    );

    terminal_view
}

/// macOS以外のプラットフォーム用スタブ
#[cfg(not(target_os = "macos"))]
fn setup_window_background(window: &tao::window::Window) {
    let _ = window;
}

/// WebSocketでStandに接続し、双方向ブリッジを提供
///
/// - 端末出力: WebSocket → EventLoopProxy
/// - 端末入力: mpsc channel → WebSocket
fn start_terminal_bridge(
    port: u16,
    proxy: EventLoopProxy<TerminalEvent>,
    input_rx: mpsc::Receiver<WsBridgeCommand>,
) {
    std::thread::Builder::new()
        .name("terminal-bridge".into())
        .spawn(move || {
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;

            let ws_url = format!("ws://localhost:{}/ws", port);

            // 接続リトライ（サーバー起動を待つ）
            let mut socket = None;
            for attempt in 0..30 {
                match tungstenite::connect(&ws_url) {
                    Ok((ws, _)) => {
                        tracing::info!("Terminal bridge connected to {}", ws_url);
                        socket = Some(ws);
                        break;
                    }
                    Err(_) => {
                        if attempt < 29 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                }
            }

            let Some(mut ws) = socket else {
                tracing::warn!("Terminal bridge: WebSocket接続に失敗");
                return;
            };

            // ソケットに読み取りタイムアウトを設定（入出力多重化のため）
            if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = ws.get_ref() {
                tcp.set_read_timeout(Some(std::time::Duration::from_millis(16)))
                    .ok();
            }

            loop {
                // WebSocket からの読み取り（タイムアウト付き）
                match ws.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&text) {
                            match msg.get("type").and_then(|t| t.as_str()) {
                                Some("TerminalOutput") => {
                                    if let Some(data) = msg.get("data").and_then(|d| d.as_str())
                                        && let Ok(bytes) = engine.decode(data)
                                    {
                                        let _ = proxy.send_event(TerminalEvent::Output(bytes));
                                    }
                                }
                                Some("TerminalReady") => {
                                    let _ = proxy.send_event(TerminalEvent::Ready);
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) => {
                        tracing::info!("Terminal bridge: WebSocket closed");
                        break;
                    }
                    Err(tungstenite::Error::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        // タイムアウト — 正常、入力キューを処理
                    }
                    Err(tungstenite::Error::ConnectionClosed) => {
                        tracing::info!("Terminal bridge: connection closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("Terminal bridge error: {}", e);
                        break;
                    }
                    _ => {}
                }

                // 入力キューからメッセージを送信
                while let Ok(cmd) = input_rx.try_recv() {
                    let json = match cmd {
                        WsBridgeCommand::Input(data) => {
                            serde_json::json!({"type": "TerminalInput", "data": data})
                        }
                        WsBridgeCommand::Resize { cols, rows } => {
                            serde_json::json!({"type": "TerminalResize", "cols": cols, "rows": rows})
                        }
                    };
                    if ws
                        .send(tungstenite::Message::Text(json.to_string().into()))
                        .is_err()
                    {
                        tracing::warn!("Terminal bridge: WebSocket送信失敗");
                        return;
                    }
                }
            }
        })
        .expect("terminal-bridge スレッドの起動に失敗");
}

/// 特殊キーを端末エスケープシーケンスに変換
///
/// テキスト入力（文字、Enter、Backspace、Tab）は ReceivedImeText で処理されるため、
/// ここでは ReceivedImeText 経由で来ない制御キーのみを扱う。
fn special_key_to_bytes(key: &KeyCode) -> Option<Vec<u8>> {
    match key {
        KeyCode::Escape => Some(b"\x1b".to_vec()),
        KeyCode::ArrowUp => Some(b"\x1b[A".to_vec()),
        KeyCode::ArrowDown => Some(b"\x1b[B".to_vec()),
        KeyCode::ArrowRight => Some(b"\x1b[C".to_vec()),
        KeyCode::ArrowLeft => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

/// Run the split window: Terminal (left) + WebView Dashboard (right)
pub fn run_webview(port: u16) -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::<TerminalEvent>::with_user_event().build();

    let window = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .build(&event_loop)?;

    // Initialize menu bar for macOS
    let menu = create_menu_bar();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    // 初期レイアウト計算
    let size = window.inner_size();
    let scale = window.scale_factor();
    let logical = size.to_logical::<f64>(scale);
    let layout = SplitLayout::from_window_size(logical.width, logical.height);

    // 左ペイン: TerminalView をセットアップ
    #[cfg(target_os = "macos")]
    let terminal_view = setup_terminal_view(&window, &layout);

    #[cfg(not(target_os = "macos"))]
    setup_window_background(&window);

    // TerminalState (VT パーサー)
    #[cfg(target_os = "macos")]
    let mut term_state = TerminalState::new(80, 24);

    // 入力チャネル（メインスレッド → WebSocketブリッジ）
    let (input_tx, input_rx) = mpsc::channel::<WsBridgeCommand>();

    // WebSocket ブリッジ開始（双方向）
    let proxy = event_loop.create_proxy();
    start_terminal_bridge(port, proxy, input_rx);

    // 初期TerminalResizeを送信（tmuxセッション作成トリガー）
    let _ = input_tx.send(WsBridgeCommand::Resize { cols: 80, rows: 24 });

    // 右ペイン: WebView ダッシュボード
    let url = format!("http://localhost:{}", port);

    let webview = WebViewBuilder::new()
        .with_bounds(layout.webview_bounds())
        .with_url(&url)
        .with_devtools(true)
        .build_as_child(&window)?;

    tracing::info!(
        "Split window: terminal={:.0}px | webview={:.0}px (port={})",
        layout.left_width,
        layout.right_width,
        port
    );

    #[cfg(debug_assertions)]
    {
        webview.open_devtools();
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            // 端末出力イベント（WebSocketブリッジから）
            Event::UserEvent(TerminalEvent::Output(bytes)) => {
                #[cfg(target_os = "macos")]
                {
                    term_state.feed_bytes(&bytes);
                    let snap = term_state.snapshot();
                    terminal_view.update_cells(&snap.cells);
                    terminal_view.request_redraw();
                }
            }
            Event::UserEvent(TerminalEvent::Ready) => {
                tracing::info!("Terminal session ready");
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                tracing::info!("Window closed");
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(new_size),
                ..
            } => {
                let logical = new_size.to_logical::<f64>(window.scale_factor());
                let new_layout = SplitLayout::from_window_size(logical.width, logical.height);

                if let Err(e) = webview.set_bounds(new_layout.webview_bounds()) {
                    tracing::warn!("WebView set_bounds error: {}", e);
                }

                #[cfg(target_os = "macos")]
                {
                    terminal_view.setFrame(new_layout.terminal_frame());

                    // 左ペインのサイズからグリッドサイズを再計算
                    let cell_w = terminal_view.cell_width();
                    let cell_h = terminal_view.cell_height();
                    if cell_w > 0.0 && cell_h > 0.0 {
                        let new_cols = (new_layout.left_width / cell_w) as u16;
                        let new_rows = (new_layout.height / cell_h) as u16;
                        if new_cols > 0 && new_rows > 0 {
                            term_state.resize(new_cols as usize, new_rows as usize);
                            terminal_view.resize_grid(new_cols as usize, new_rows as usize);
                            let _ = input_tx.send(WsBridgeCommand::Resize {
                                cols: new_cols,
                                rows: new_rows,
                            });
                        }
                    }

                    terminal_view.request_redraw();
                }
            }
            // IME 確定テキスト（日本語入力、通常の文字入力を含む）
            // macOS: insertText() 経由で配信される
            Event::WindowEvent {
                event: WindowEvent::ReceivedImeText(text),
                ..
            } => {
                if !text.is_empty() {
                    use base64::Engine;
                    // \n → \r に変換（ターミナルの Enter）
                    let bytes: Vec<u8> = text
                        .bytes()
                        .map(|b| if b == b'\n' { b'\r' } else { b })
                        .collect();
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let _ = input_tx.send(WsBridgeCommand::Input(encoded));
                }
            }
            // キーボード入力（特殊キー + IME非活性時のフォールバック）
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event, .. },
                ..
            } => {
                if event.state == tao::event::ElementState::Pressed {
                    // DevTools トグル (F12)
                    if event.physical_key == KeyCode::F12 {
                        if webview.is_devtools_open() {
                            webview.close_devtools();
                        } else {
                            webview.open_devtools();
                        }
                        return;
                    }

                    // 特殊キーのみ処理（テキスト入力は ReceivedImeText に委譲）
                    // ReceivedImeText で処理済みのキー（Enter, 通常文字）は
                    // text=None で到着するのでここでは送信しない
                    if let Some(bytes) = special_key_to_bytes(&event.physical_key) {
                        use base64::Engine;
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let _ = input_tx.send(WsBridgeCommand::Input(encoded));
                    }
                }
            }
            _ => {}
        }

        #[cfg(target_os = "macos")]
        let _ = &terminal_view;
    });
}
