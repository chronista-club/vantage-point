//! Native terminal window — Unison (QUIC) ブリッジモード
//!
//! Arctic/Nordic + Ocean ダークテーマのターミナルウィンドウ。
//! Stand サーバーの PTY に Unison QUIC チャネルで接続し、ウィンドウを閉じても PTY は生存する。
//!
//! ## パイプライン
//! ```text
//! Stand Server (QUIC "terminal") ─── UnisonChannel ───► EventLoopProxy → TerminalState → TerminalView
//! keyboard (main thread) → mpsc → input-bridge → unison-bridge → UnisonChannel → Stand Server
//! ```

use std::sync::mpsc;

use muda::{Menu, PredefinedMenuItem, Submenu};
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    keyboard::{KeyCode, ModifiersState},
    window::WindowBuilder,
};

#[cfg(target_os = "macos")]
use crate::terminal::TerminalState;

use crate::terminal::StatusBarInfo;

/// tao EventLoop に送るカスタムイベント
#[derive(Debug)]
enum TerminalEvent {
    /// 端末出力データ（VTバイトストリーム）
    Output(Vec<u8>),
}

/// PTYブリッジに送るコマンド
enum PtyInputCommand {
    /// PTY 入力データ（生バイト列）
    Input(Vec<u8>),
    /// PTY リサイズ
    Resize { cols: u16, rows: u16 },
}

/// メニューバー作成（Edit メニュー: コピー/ペースト対応）
pub(crate) fn create_menu_bar() -> Menu {
    let menu = Menu::new();

    // Edit メニュー: Copy/Paste は KeyboardInput ハンドラで処理するため、
    // PredefinedMenuItem を使わない（macOS がショートカットを横取りするため）
    let edit_menu = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::select_all(None),
        ],
    )
    .expect("Failed to create Edit menu");

    menu.append(&edit_menu).expect("Failed to append Edit menu");
    menu
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

// =============================================================================
// キーボード → PTY 変換
// =============================================================================

/// 特殊キー → エスケープシーケンス変換
///
/// Application Cursor Keys モード（DECCKM）対応。
/// TUIアプリ有効時は矢印キーが SS3 形式（`\x1bOx`）になる。
fn key_to_pty_bytes(key: &KeyCode, app_cursor: bool, shift: bool) -> Option<Vec<u8>> {
    match key {
        KeyCode::Backspace => Some(b"\x7f".to_vec()),
        KeyCode::Tab => {
            if shift {
                Some(b"\x1b[Z".to_vec()) // Shift+Tab (reverse tab)
            } else {
                Some(b"\t".to_vec())
            }
        }
        KeyCode::Escape => Some(b"\x1b".to_vec()),
        // 矢印キー: app_cursor_mode で形式が変わる
        KeyCode::ArrowUp => Some(if app_cursor { b"\x1bOA" } else { b"\x1b[A" }.to_vec()),
        KeyCode::ArrowDown => Some(if app_cursor { b"\x1bOB" } else { b"\x1b[B" }.to_vec()),
        KeyCode::ArrowRight => Some(if app_cursor { b"\x1bOC" } else { b"\x1b[C" }.to_vec()),
        KeyCode::ArrowLeft => Some(if app_cursor { b"\x1bOD" } else { b"\x1b[D" }.to_vec()),
        KeyCode::Home => Some(if app_cursor { b"\x1bOH" } else { b"\x1b[H" }.to_vec()),
        KeyCode::End => Some(if app_cursor { b"\x1bOF" } else { b"\x1b[F" }.to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        // F1-F12
        KeyCode::F1 => Some(b"\x1bOP".to_vec()),
        KeyCode::F2 => Some(b"\x1bOQ".to_vec()),
        KeyCode::F3 => Some(b"\x1bOR".to_vec()),
        KeyCode::F4 => Some(b"\x1bOS".to_vec()),
        KeyCode::F5 => Some(b"\x1b[15~".to_vec()),
        KeyCode::F6 => Some(b"\x1b[17~".to_vec()),
        KeyCode::F7 => Some(b"\x1b[18~".to_vec()),
        KeyCode::F8 => Some(b"\x1b[19~".to_vec()),
        KeyCode::F9 => Some(b"\x1b[20~".to_vec()),
        KeyCode::F10 => Some(b"\x1b[21~".to_vec()),
        KeyCode::F11 => Some(b"\x1b[23~".to_vec()),
        KeyCode::F12 => Some(b"\x1b[24~".to_vec()),
        _ => None,
    }
}

/// Ctrl+物理キー → 制御コード（ASCII 0x01-0x1D）
fn ctrl_key_byte(key: &KeyCode) -> Option<u8> {
    match key {
        KeyCode::KeyA => Some(0x01),
        KeyCode::KeyB => Some(0x02),
        KeyCode::KeyC => Some(0x03),
        KeyCode::KeyD => Some(0x04),
        KeyCode::KeyE => Some(0x05),
        KeyCode::KeyF => Some(0x06),
        KeyCode::KeyG => Some(0x07),
        KeyCode::KeyH => Some(0x08),
        KeyCode::KeyI => Some(0x09), // Tab
        KeyCode::KeyJ => Some(0x0A), // LF
        KeyCode::KeyK => Some(0x0B),
        KeyCode::KeyL => Some(0x0C),
        KeyCode::KeyM => Some(0x0D), // CR
        KeyCode::KeyN => Some(0x0E),
        KeyCode::KeyO => Some(0x0F),
        KeyCode::KeyP => Some(0x10),
        KeyCode::KeyQ => Some(0x11),
        KeyCode::KeyR => Some(0x12),
        KeyCode::KeyS => Some(0x13),
        KeyCode::KeyT => Some(0x14),
        KeyCode::KeyU => Some(0x15),
        KeyCode::KeyV => Some(0x16),
        KeyCode::KeyW => Some(0x17),
        KeyCode::KeyX => Some(0x18),
        KeyCode::KeyY => Some(0x19),
        KeyCode::KeyZ => Some(0x1A),
        KeyCode::BracketLeft => Some(0x1B),  // Ctrl+[ = ESC
        KeyCode::Backslash => Some(0x1C),
        KeyCode::BracketRight => Some(0x1D),
        _ => None,
    }
}

/// 物理キー → 基本ASCII文字（Alt+key 用）
///
/// macOS では Alt(Option)+key が特殊文字を生成するため、
/// 物理キーから基本文字を直接取得する。
fn keycode_to_base_char(key: &KeyCode) -> Option<u8> {
    match key {
        KeyCode::KeyA => Some(b'a'),
        KeyCode::KeyB => Some(b'b'),
        KeyCode::KeyC => Some(b'c'),
        KeyCode::KeyD => Some(b'd'),
        KeyCode::KeyE => Some(b'e'),
        KeyCode::KeyF => Some(b'f'),
        KeyCode::KeyG => Some(b'g'),
        KeyCode::KeyH => Some(b'h'),
        KeyCode::KeyI => Some(b'i'),
        KeyCode::KeyJ => Some(b'j'),
        KeyCode::KeyK => Some(b'k'),
        KeyCode::KeyL => Some(b'l'),
        KeyCode::KeyM => Some(b'm'),
        KeyCode::KeyN => Some(b'n'),
        KeyCode::KeyO => Some(b'o'),
        KeyCode::KeyP => Some(b'p'),
        KeyCode::KeyQ => Some(b'q'),
        KeyCode::KeyR => Some(b'r'),
        KeyCode::KeyS => Some(b's'),
        KeyCode::KeyT => Some(b't'),
        KeyCode::KeyU => Some(b'u'),
        KeyCode::KeyV => Some(b'v'),
        KeyCode::KeyW => Some(b'w'),
        KeyCode::KeyX => Some(b'x'),
        KeyCode::KeyY => Some(b'y'),
        KeyCode::KeyZ => Some(b'z'),
        KeyCode::Digit0 => Some(b'0'),
        KeyCode::Digit1 => Some(b'1'),
        KeyCode::Digit2 => Some(b'2'),
        KeyCode::Digit3 => Some(b'3'),
        KeyCode::Digit4 => Some(b'4'),
        KeyCode::Digit5 => Some(b'5'),
        KeyCode::Digit6 => Some(b'6'),
        KeyCode::Digit7 => Some(b'7'),
        KeyCode::Digit8 => Some(b'8'),
        KeyCode::Digit9 => Some(b'9'),
        KeyCode::Minus => Some(b'-'),
        KeyCode::Equal => Some(b'='),
        KeyCode::BracketLeft => Some(b'['),
        KeyCode::BracketRight => Some(b']'),
        KeyCode::Backslash => Some(b'\\'),
        KeyCode::Semicolon => Some(b';'),
        KeyCode::Quote => Some(b'\''),
        KeyCode::Comma => Some(b','),
        KeyCode::Period => Some(b'.'),
        KeyCode::Slash => Some(b'/'),
        KeyCode::Backquote => Some(b'`'),
        _ => None,
    }
}

/// テキストをクリップボードにコピー（macOS: pbcopy）
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    child.wait()?;
    Ok(())
}

// =============================================================================
// Unison (QUIC) ブリッジモード
// =============================================================================

/// Unison ブリッジスレッドを起動
///
/// Stand サーバーの QUIC "terminal" チャネルに接続し、
/// PTY 出力を EventLoop へ、キーボード入力を Stand へ中継する。
/// raw frame でバイナリ直送（base64 不要）。
fn start_unison_bridge(
    port: u16, // HTTP ポート（QUIC = port + QUIC_PORT_OFFSET）
    proxy: EventLoopProxy<TerminalEvent>,
    input_rx: mpsc::Receiver<PtyInputCommand>,
) {
    std::thread::Builder::new()
        .name("unison-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .expect("Failed to create tokio runtime");
            rt.block_on(async move {
                let quic_port = port + crate::stand::unison_server::QUIC_PORT_OFFSET;
                let addr = format!("[::1]:{}", quic_port);

                let client = match unison::ProtocolClient::new_default() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("ProtocolClient 作成失敗: {}", e);
                        return;
                    }
                };
                if let Err(e) = client.connect(&addr).await {
                    tracing::error!("Unison 接続失敗 ({}): {}", addr, e);
                    return;
                }
                let channel = match client.open_channel("terminal").await {
                    Ok(ch) => ch,
                    Err(e) => {
                        tracing::error!("terminal チャネル開設失敗: {}", e);
                        return;
                    }
                };

                tracing::info!("Unison connected to Stand (port={})", quic_port);

                // sync mpsc → tokio mpsc ブリッジ
                let (bridge_tx, mut bridge_rx) =
                    tokio::sync::mpsc::channel::<PtyInputCommand>(256);
                std::thread::Builder::new()
                    .name("input-bridge".into())
                    .spawn(move || {
                        while let Ok(cmd) = input_rx.recv() {
                            if bridge_tx.blocking_send(cmd).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("input-bridge spawn failed");

                // メインループ: select で双方向処理
                loop {
                    tokio::select! {
                        // PTY output from Stand
                        data = channel.recv_raw() => {
                            match data {
                                Ok(bytes) => {
                                    if proxy.send_event(
                                        TerminalEvent::Output(bytes)
                                    ).is_err() {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        // Keyboard input → Stand
                        cmd = bridge_rx.recv() => {
                            match cmd {
                                Some(PtyInputCommand::Input(data)) => {
                                    if channel.send_raw(&data).await.is_err() {
                                        break;
                                    }
                                }
                                Some(PtyInputCommand::Resize { cols, rows }) => {
                                    let _ = channel.request(
                                        "resize",
                                        serde_json::json!({
                                            "cols": cols, "rows": rows
                                        }),
                                    ).await;
                                }
                                None => break,
                            }
                        }
                    }
                }
            });
        })
        .expect("unison-bridge スレッドの起動に失敗");
}

/// Unison ブリッジモードのターミナルウィンドウを起動
///
/// Stand サーバーの QUIC "terminal" チャネルに接続し、ネイティブウィンドウで描画する。
/// ウィンドウを閉じても Stand 側の PTY は生存し、再度接続で re-attach できる。
pub fn run_terminal_unison(port: u16) -> anyhow::Result<()> {
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

    // 入力チャネル（メインスレッド → PTYブリッジ）
    let (input_tx, input_rx) = mpsc::channel::<PtyInputCommand>();

    // Unison ブリッジ開始
    let proxy = event_loop.create_proxy();
    start_unison_bridge(port, proxy, input_rx);

    // 初期リサイズを送信
    let _ = input_tx.send(PtyInputCommand::Resize { cols: 80, rows: 24 });

    // ステータスバーにポート番号を表示
    let session_name = format!("Stand:{}", port);

    #[cfg(target_os = "macos")]
    {
        terminal_view.update_status_bar(StatusBarInfo {
            session_name: session_name.clone(),
            ..Default::default()
        });
    }

    // IME確定Enter抑制フラグ
    let mut suppress_next_enter = false;
    let mut enter_handled_this_frame = false;

    // マウス位置追跡（テキスト選択用）
    let mut cursor_pos: LogicalPosition<f64> = LogicalPosition::new(0.0, 0.0);
    let mut mouse_dragging = false;

    // 修飾キー状態（ModifiersChanged で一括追跡）
    let mut modifiers = ModifiersState::empty();

    tracing::info!("Terminal Unison bridge mode started (port={})", port);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            // 端末出力イベント（PTYブリッジから）
            Event::UserEvent(TerminalEvent::Output(bytes)) => {
                #[cfg(target_os = "macos")]
                {
                    term_state.feed_bytes(&bytes);
                    let snap = term_state.snapshot();
                    terminal_view.update_cells(&snap.cells);
                    terminal_view.update_cursor(
                        snap.cursor.0,
                        snap.cursor.1,
                        snap.cursor_visible,
                    );
                    terminal_view.request_redraw();

                    // IME変換ウィンドウをカーソル位置に追従
                    let cw = terminal_view.cell_width();
                    let ch = terminal_view.cell_height();
                    let ime_x = snap.cursor.1 as f64 * cw;
                    let ime_y = (snap.cursor.0 + 1) as f64 * ch;
                    window.set_ime_position(LogicalPosition::new(ime_x, ime_y));
                }
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
                        let new_rows = ((logical.height - cell_h - 1.0) / cell_h) as u16;
                        if new_cols > 0 && new_rows > 0 {
                            term_state.resize(new_cols as usize, new_rows as usize);
                            terminal_view.resize_grid(new_cols as usize, new_rows as usize);
                            let _ = input_tx.send(PtyInputCommand::Resize {
                                cols: new_cols,
                                rows: new_rows,
                            });
                        }
                    }

                    terminal_view.request_redraw();
                }
            }
            // 修飾キー状態の一括追跡
            Event::WindowEvent {
                event: WindowEvent::ModifiersChanged(mods),
                ..
            } => {
                modifiers = mods;
            }
            // IME 確定テキスト
            Event::WindowEvent {
                event: WindowEvent::ReceivedImeText(text),
                ..
            } => {
                // Ctrl/Alt 押下中はスキップ（KeyboardInput で処理済み）
                if modifiers.intersects(ModifiersState::CONTROL | ModifiersState::ALT) {
                    return;
                }

                if !text.is_empty() {
                    let is_newline = text == "\n" || text == "\r";

                    if is_newline {
                        if suppress_next_enter {
                            // IME確定直後の "\n" → スキップ
                        } else if !enter_handled_this_frame {
                            let _ = input_tx.send(PtyInputCommand::Input(b"\r".to_vec()));
                            enter_handled_this_frame = true;
                        }
                    } else {
                        let bytes: Vec<u8> = text.bytes().collect();
                        let _ = input_tx.send(PtyInputCommand::Input(bytes));
                        suppress_next_enter = true;
                    }
                }
            }
            // キーボード入力
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event, .. },
                ..
            } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                // Cmd+key: ウィンドウ操作
                #[cfg(target_os = "macos")]
                if modifiers.contains(ModifiersState::SUPER) {
                    // Cmd+V: ペースト（Bracketed Paste 対応）
                    if event.physical_key == KeyCode::KeyV {
                        if let Ok(output) = std::process::Command::new("pbpaste").output()
                            && output.status.success()
                            && !output.stdout.is_empty()
                        {
                            let data = if term_state.bracketed_paste_mode() {
                                let mut d = b"\x1b[200~".to_vec();
                                d.extend(&output.stdout);
                                d.extend(b"\x1b[201~");
                                d
                            } else {
                                output.stdout
                            };
                            let _ = input_tx.send(PtyInputCommand::Input(data));
                        }
                        return;
                    }

                    // Cmd+C: コピー（選択テキスト → クリップボード）
                    if event.physical_key == KeyCode::KeyC {
                        if let Some(text) = terminal_view.selected_text() {
                            let _ = copy_to_clipboard(&text);
                            terminal_view.clear_selection();
                        }
                        return;
                    }

                    // その他の Cmd+key はシステムに委譲
                    return;
                }

                // Escape: テキスト選択解除
                #[cfg(target_os = "macos")]
                if event.physical_key == KeyCode::Escape && terminal_view.has_selection() {
                    terminal_view.clear_selection();
                    return;
                }

                // Ctrl+key: 制御コード
                if modifiers.contains(ModifiersState::CONTROL) {
                    if let Some(byte) = ctrl_key_byte(&event.physical_key) {
                        let _ = input_tx.send(PtyInputCommand::Input(vec![byte]));
                        return;
                    }
                }

                // Alt+key: ESC + 基本文字
                if modifiers.contains(ModifiersState::ALT) {
                    if let Some(ch) = keycode_to_base_char(&event.physical_key) {
                        let _ = input_tx.send(PtyInputCommand::Input(vec![0x1b, ch]));
                        return;
                    }
                }

                // 特殊キー（矢印、F1-F12等）
                #[cfg(target_os = "macos")]
                let app_cursor = term_state.app_cursor_mode();
                #[cfg(not(target_os = "macos"))]
                let app_cursor = false;

                if let Some(bytes) = key_to_pty_bytes(
                    &event.physical_key,
                    app_cursor,
                    modifiers.contains(ModifiersState::SHIFT),
                ) {
                    let _ = input_tx.send(PtyInputCommand::Input(bytes));
                    return;
                }

                // Enter（IME未処理分）
                if event.physical_key == KeyCode::Enter
                    && !suppress_next_enter
                    && !enter_handled_this_frame
                {
                    let _ = input_tx.send(PtyInputCommand::Input(b"\r".to_vec()));
                    enter_handled_this_frame = true;
                }
            }
            // マウスカーソル位置追跡 + ドラッグ中のテキスト選択拡張
            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. },
                ..
            } => {
                cursor_pos = position.to_logical(window.scale_factor());

                #[cfg(target_os = "macos")]
                if mouse_dragging {
                    let (row, col) = terminal_view.point_to_cell(cursor_pos.x, cursor_pos.y);
                    terminal_view.extend_selection(row, col);
                }
            }
            // マウスボタン押下 → テキスト選択開始
            Event::WindowEvent {
                event:
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    },
                ..
            } => {
                #[cfg(target_os = "macos")]
                {
                    let (row, col) = terminal_view.point_to_cell(cursor_pos.x, cursor_pos.y);
                    terminal_view.start_selection(row, col);
                    mouse_dragging = true;
                }
            }
            // マウスボタンリリース → ドラッグ終了
            Event::WindowEvent {
                event:
                    WindowEvent::MouseInput {
                        state: ElementState::Released,
                        button: MouseButton::Left,
                        ..
                    },
                ..
            } => {
                mouse_dragging = false;
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
