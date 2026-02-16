//! Native terminal window
//!
//! Arctic/Nordic + Ocean ダークテーマのターミナルウィンドウ。
//! TerminalView (alacritty_terminal + CoreText ネイティブレンダラー) でフルスクリーン描画。
//!
//! ## パイプライン
//! ```text
//! tmux → Stand (pipe-pane) → WebSocket → TerminalState → TerminalView
//! keyboard → WebSocket → Stand → tmux (send-keys)
//! ```

use std::sync::mpsc;

use muda::{Menu, PredefinedMenuItem, Submenu};
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    keyboard::KeyCode,
    window::WindowBuilder,
};

#[cfg(target_os = "macos")]
use crate::terminal::TerminalState;

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

/// メニューバー作成（Edit メニュー: コピー/ペースト対応）
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

/// TerminalView を作成し、NSWindow の contentView に追加
#[cfg(target_os = "macos")]
fn setup_terminal_view(
    window: &tao::window::Window,
    width: f64,
    height: f64,
) -> objc2::rc::Retained<crate::terminal::renderer::TerminalView> {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::NSColor;
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use tao::platform::macos::WindowExtMacOS;

    use crate::terminal::renderer::TerminalView;

    let mtm = MainThreadMarker::new().expect("メインスレッド上で実行する必要があります");

    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height));
    let terminal_view = TerminalView::new(mtm, frame, 80, 24);

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
        width,
        height
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
                                Some("terminal_output") => {
                                    if let Some(data) = msg.get("data").and_then(|d| d.as_str())
                                        && let Ok(bytes) = engine.decode(data)
                                    {
                                        let _ = proxy.send_event(TerminalEvent::Output(bytes));
                                    }
                                }
                                Some("terminal_ready") => {
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
                            serde_json::json!({"type": "terminal_input", "data": data})
                        }
                        WsBridgeCommand::Resize { cols, rows } => {
                            serde_json::json!({"type": "terminal_resize", "cols": cols, "rows": rows})
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
        KeyCode::Backspace => Some(b"\x7f".to_vec()),
        KeyCode::Tab => Some(b"\t".to_vec()),
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

/// ターミナルフルスクリーンウィンドウを起動
pub fn run_webview(port: u16) -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::<TerminalEvent>::with_user_event().build();

    let window = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .build(&event_loop)?;

    // メニューバー初期化
    let menu = create_menu_bar();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    // 初期レイアウト
    let size = window.inner_size();
    let scale = window.scale_factor();
    let logical = size.to_logical::<f64>(scale);

    // TerminalView をセットアップ
    #[cfg(target_os = "macos")]
    let terminal_view = setup_terminal_view(&window, logical.width, logical.height);

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

    // IME確定Enter抑制フラグ
    let mut suppress_next_enter = false;
    let mut enter_handled_this_frame = false;

    tracing::info!("Terminal fullscreen window started (port={})", port);

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

                    // IME変換ウィンドウをカーソル位置に追従させる
                    let cw = terminal_view.cell_width();
                    let ch = terminal_view.cell_height();
                    let ime_x = snap.cursor.1 as f64 * cw;
                    let ime_y = (snap.cursor.0 + 1) as f64 * ch;
                    window.set_ime_position(LogicalPosition::new(ime_x, ime_y));
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

                #[cfg(target_os = "macos")]
                {
                    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
                    let frame = CGRect::new(
                        CGPoint::new(0.0, 0.0),
                        CGSize::new(logical.width, logical.height),
                    );
                    terminal_view.setFrame(frame);

                    let cell_w = terminal_view.cell_width();
                    let cell_h = terminal_view.cell_height();
                    if cell_w > 0.0 && cell_h > 0.0 {
                        let new_cols = (logical.width / cell_w) as u16;
                        let new_rows = (logical.height / cell_h) as u16;
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
            Event::WindowEvent {
                event: WindowEvent::ReceivedImeText(text),
                ..
            } => {
                if !text.is_empty() {
                    use base64::Engine;

                    let is_newline = text == "\n" || text == "\r";

                    if is_newline {
                        if suppress_next_enter {
                            // IME確定直後の "\n" → スキップ
                        } else if !enter_handled_this_frame {
                            let encoded =
                                base64::engine::general_purpose::STANDARD.encode(b"\r");
                            let _ = input_tx.send(WsBridgeCommand::Input(encoded));
                            enter_handled_this_frame = true;
                        }
                    } else {
                        let bytes: Vec<u8> = text.bytes().collect();
                        let encoded =
                            base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let _ = input_tx.send(WsBridgeCommand::Input(encoded));
                        suppress_next_enter = true;
                    }
                }
            }
            // キーボード入力（特殊キー + IME非活性時のフォールバック）
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event, .. },
                ..
            } => {
                if event.state == tao::event::ElementState::Pressed {
                    if event.physical_key == KeyCode::Enter
                        && !suppress_next_enter
                        && !enter_handled_this_frame
                    {
                        use base64::Engine;
                        let encoded =
                            base64::engine::general_purpose::STANDARD.encode(b"\r");
                        let _ = input_tx.send(WsBridgeCommand::Input(encoded));
                        enter_handled_this_frame = true;
                    } else if let Some(bytes) = special_key_to_bytes(&event.physical_key) {
                        use base64::Engine;
                        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        let _ = input_tx.send(WsBridgeCommand::Input(encoded));
                    }
                }
            }
            // フレーム終了 → IMEフラグをリセット
            Event::MainEventsCleared => {
                suppress_next_enter = false;
                enter_handled_this_frame = false;
            }
            _ => {}
        }

        #[cfg(target_os = "macos")]
        let _ = &terminal_view;
        let _ = &menu;
    });
}
