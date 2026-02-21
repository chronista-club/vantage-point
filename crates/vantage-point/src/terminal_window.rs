//! Native terminal window
//!
//! Arctic/Nordic + Ocean ダークテーマのターミナルウィンドウ。
//! TerminalView (alacritty_terminal + CoreText ネイティブレンダラー) でフルスクリーン描画。
//!
//! ## パイプライン（Daemon モード）
//! ```text
//! PtySlot → Daemon (broadcast) → DaemonClient → TerminalState → TerminalView
//! keyboard → DaemonClient → Daemon → PtySlot
//! ```

use std::sync::mpsc;

use muda::{Menu, PredefinedMenuItem, Submenu};
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    keyboard::KeyCode,
    window::WindowBuilder,
};

#[cfg(target_os = "macos")]
use crate::terminal::TerminalState;

use crate::daemon::client::DaemonClient;
use crate::terminal::StatusBarInfo;

/// tao EventLoop に送るカスタムイベント
#[derive(Debug)]
enum TerminalEvent {
    /// 端末出力データ（VTバイトストリーム）
    Output(Vec<u8>),
    /// 端末セッション開始
    Ready,
    /// ステータスバー更新
    StatusUpdate(StatusBarInfo),
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
// Daemon モード（tmux 非依存）
// =============================================================================

/// Daemon ブリッジに送るコマンド
///
/// Daemon 経由で PTY を直接操作する。base64 エンコードは DaemonClient が行う。
enum DaemonInputCommand {
    /// PTY 入力データ（生バイト列）
    Input(Vec<u8>),
    /// PTY リサイズ
    Resize { cols: u16, rows: u16 },
    /// 新規ペイン作成（Cmd+T）
    CreatePane,
    /// アクティブペイン終了（Cmd+W）
    KillPane,
}

/// Daemon ブリッジスレッドを起動
///
/// Daemon に QUIC 接続し、セッション・ペインを作成して PTY I/O を中継する。
/// 入力コマンドは mpsc channel で受信し、PTY 出力は EventLoop にプッシュする。
fn start_daemon_bridge(
    daemon_port: u16,
    project_name: String,
    proxy: EventLoopProxy<TerminalEvent>,
    input_rx: mpsc::Receiver<DaemonInputCommand>,
) {
    std::thread::Builder::new()
        .name("daemon-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime の作成に失敗");

            // Daemon に接続してセッション・ペインを作成
            let setup = rt.block_on(async {
                let client = DaemonClient::connect(daemon_port, 30).await?;
                tracing::info!("Daemon 接続完了: {}", client.addr());

                // セッション作成（既に存在していてもOK）
                let session_id = project_name.clone();
                match client.create_session(&session_id).await {
                    Ok(_) => tracing::info!("セッション作成: {}", session_id),
                    Err(e) => {
                        // 既存セッションの場合はアタッチ
                        tracing::debug!("セッション作成スキップ（既存の可能性）: {}", e);
                    }
                }

                // デフォルトシェルでペイン作成
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
                let pane_id = client.create_pane(&session_id, &shell, 80, 24).await?;
                tracing::info!(
                    "ペイン作成: session={}, pane_id={}, shell={}",
                    session_id,
                    pane_id,
                    shell
                );

                // アタッチ
                client.attach(&session_id).await?;

                // セッション準備完了を通知
                let _ = proxy.send_event(TerminalEvent::Ready);

                Ok::<_, anyhow::Error>((client, session_id, pane_id))
            });

            let (client, session_id, mut active_pane_id) = match setup {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("Daemon ブリッジ初期化失敗: {}", e);
                    return;
                }
            };

            // PTY 出力ポーリングスレッドを起動
            // Daemon の terminal.read_output RPC を繰り返し呼び、
            // 出力があれば EventLoop に送信する
            let output_client = std::sync::Arc::new(client);
            let input_client = output_client.clone();
            let output_session = session_id.clone();
            let output_proxy = proxy.clone();

            std::thread::Builder::new()
                .name("daemon-output".into())
                .spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("tokio runtime の作成に失敗");
                    let mut consecutive_errors = 0u32;

                    loop {
                        match rt.block_on(output_client.read_output(
                            &output_session,
                            active_pane_id,
                            50, // 50ms タイムアウト
                        )) {
                            Ok(data) if !data.is_empty() => {
                                consecutive_errors = 0;
                                if output_proxy
                                    .send_event(TerminalEvent::Output(data))
                                    .is_err()
                                {
                                    // EventLoop が閉じた
                                    break;
                                }
                            }
                            Ok(_) => {
                                // タイムアウト（出力なし）— 正常
                                consecutive_errors = 0;
                            }
                            Err(e) => {
                                consecutive_errors += 1;
                                if consecutive_errors > 10 {
                                    tracing::error!(
                                        "PTY 出力読み取りで連続エラー ({}): {}",
                                        consecutive_errors,
                                        e
                                    );
                                    break;
                                }
                                tracing::debug!("PTY 出力読み取りエラー: {}", e);
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                        }
                    }
                })
                .expect("daemon-output スレッドの起動に失敗");

            // 入力コマンドのハンドリングループ
            loop {
                let cmd = match input_rx.recv() {
                    Ok(cmd) => cmd,
                    Err(_) => {
                        tracing::info!("daemon-bridge: 入力チャネル閉鎖、終了");
                        break;
                    }
                };

                match cmd {
                    DaemonInputCommand::Input(data) => {
                        if let Err(e) = rt.block_on(input_client.write_input(
                            &session_id,
                            active_pane_id,
                            &data,
                        )) {
                            tracing::warn!("PTY 入力送信失敗: {}", e);
                        }
                    }
                    DaemonInputCommand::Resize { cols, rows } => {
                        if let Err(e) = rt.block_on(input_client.resize_pane(
                            &session_id,
                            active_pane_id,
                            cols,
                            rows,
                        )) {
                            tracing::warn!("リサイズ送信失敗: {}", e);
                        }
                    }
                    DaemonInputCommand::CreatePane => {
                        let shell =
                            std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
                        match rt.block_on(input_client.create_pane(&session_id, &shell, 80, 24)) {
                            Ok(new_pane_id) => {
                                tracing::info!("新規ペイン作成: pane_id={}", new_pane_id);
                                active_pane_id = new_pane_id;
                            }
                            Err(e) => {
                                tracing::warn!("新規ペイン作成失敗: {}", e);
                            }
                        }
                    }
                    DaemonInputCommand::KillPane => {
                        if let Err(e) =
                            rt.block_on(input_client.kill_pane(&session_id, active_pane_id))
                        {
                            tracing::warn!("ペイン終了失敗: {}", e);
                        }
                    }
                }
            }
        })
        .expect("daemon-bridge スレッドの起動に失敗");
}

/// Daemon 経由のターミナルウィンドウを起動
///
/// Daemon の PTY 直接管理で動作する。
/// 入力は DaemonClient.write_input()（base64 は client 内部で処理）、
/// 出力は Daemon の terminal.read_output RPC でポーリング受信。
pub fn run_terminal_with_daemon(daemon_port: u16, project_name: &str) -> anyhow::Result<()> {
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

    // 入力チャネル（メインスレッド → Daemon ブリッジ）
    let (input_tx, input_rx) = mpsc::channel::<DaemonInputCommand>();

    // Daemon ブリッジ開始
    let proxy = event_loop.create_proxy();
    start_daemon_bridge(
        daemon_port,
        project_name.to_string(),
        proxy.clone(),
        input_rx,
    );

    // 初期リサイズを送信
    let _ = input_tx.send(DaemonInputCommand::Resize { cols: 80, rows: 24 });

    // IME確定Enter抑制フラグ
    let mut suppress_next_enter = false;
    let mut enter_handled_this_frame = false;

    // マウス位置追跡（テキスト選択用）
    let mut cursor_pos: LogicalPosition<f64> = LogicalPosition::new(0.0, 0.0);
    let mut mouse_dragging = false;

    // 修飾キー追跡（Cmd+数字キー用）
    let mut logo_pressed = false;

    tracing::info!(
        "Terminal fullscreen window started (daemon_port={}, project={})",
        daemon_port,
        project_name
    );

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            // 端末出力イベント（Daemon ブリッジから）
            Event::UserEvent(TerminalEvent::Output(bytes)) => {
                #[cfg(target_os = "macos")]
                {
                    term_state.feed_bytes(&bytes);
                    let snap = term_state.snapshot();
                    terminal_view.update_cells(&snap.cells);
                    terminal_view.request_redraw();

                    // IME変換ウィンドウをカーソル位置に追従
                    let cw = terminal_view.cell_width();
                    let ch = terminal_view.cell_height();
                    let ime_x = snap.cursor.1 as f64 * cw;
                    let ime_y = (snap.cursor.0 + 1) as f64 * ch;
                    window.set_ime_position(LogicalPosition::new(ime_x, ime_y));
                }
            }
            Event::UserEvent(TerminalEvent::Ready) => {
                tracing::info!("Daemon terminal session ready");
            }
            // ステータスバー更新（Daemon ベース）
            Event::UserEvent(TerminalEvent::StatusUpdate(info)) => {
                #[cfg(target_os = "macos")]
                {
                    terminal_view.update_status_bar(info);
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
                            let _ = input_tx.send(DaemonInputCommand::Resize {
                                cols: new_cols,
                                rows: new_rows,
                            });
                        }
                    }

                    terminal_view.request_redraw();
                }
            }
            // IME 確定テキスト
            Event::WindowEvent {
                event: WindowEvent::ReceivedImeText(text),
                ..
            } => {
                tracing::info!("IME text received: {:?}", text);
                if !text.is_empty() {
                    let is_newline = text == "\n" || text == "\r";

                    if is_newline {
                        if suppress_next_enter {
                            // IME確定直後の "\n" → スキップ
                        } else if !enter_handled_this_frame {
                            let _ = input_tx.send(DaemonInputCommand::Input(b"\r".to_vec()));
                            enter_handled_this_frame = true;
                        }
                    } else {
                        let bytes: Vec<u8> = text.bytes().collect();
                        let _ = input_tx.send(DaemonInputCommand::Input(bytes));
                        suppress_next_enter = true;
                    }
                }
            }
            // キーボード入力
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event, .. },
                ..
            } => {
                tracing::debug!(
                    "KeyboardInput: {:?} state={:?}",
                    event.physical_key,
                    event.state
                );
                if event.state == ElementState::Pressed {
                    // Cmd+キーでウィンドウ操作
                    #[cfg(target_os = "macos")]
                    if logo_pressed {
                        // Cmd+数字: タブ切替（TODO: Daemon ペイン切替）
                        let _win_idx = match event.physical_key {
                            KeyCode::Digit1 => Some(0usize),
                            KeyCode::Digit2 => Some(1),
                            KeyCode::Digit3 => Some(2),
                            KeyCode::Digit4 => Some(3),
                            KeyCode::Digit5 => Some(4),
                            KeyCode::Digit6 => Some(5),
                            KeyCode::Digit7 => Some(6),
                            KeyCode::Digit8 => Some(7),
                            KeyCode::Digit9 => Some(8),
                            _ => None,
                        };
                        // TODO: Daemon セッション内のペイン切替
                        // 現状は単一ペインのみ対応

                        // Cmd+V: ペースト（クリップボード → Daemon PTY）
                        if event.physical_key == KeyCode::KeyV {
                            if let Ok(output) = std::process::Command::new("pbpaste").output()
                                && output.status.success()
                                && !output.stdout.is_empty()
                            {
                                let _ = input_tx.send(DaemonInputCommand::Input(output.stdout));
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

                        // Cmd+T: 新規ペイン
                        if event.physical_key == KeyCode::KeyT {
                            let _ = input_tx.send(DaemonInputCommand::CreatePane);
                            return;
                        }

                        // Cmd+W: ペイン終了
                        if event.physical_key == KeyCode::KeyW {
                            let _ = input_tx.send(DaemonInputCommand::KillPane);
                            return;
                        }
                    }

                    // Escape: テキスト選択解除
                    #[cfg(target_os = "macos")]
                    if event.physical_key == KeyCode::Escape && terminal_view.has_selection() {
                        terminal_view.clear_selection();
                        return;
                    }

                    if event.physical_key == KeyCode::Enter
                        && !suppress_next_enter
                        && !enter_handled_this_frame
                    {
                        let _ = input_tx.send(DaemonInputCommand::Input(b"\r".to_vec()));
                        enter_handled_this_frame = true;
                    } else if let Some(bytes) = special_key_to_bytes(&event.physical_key) {
                        let _ = input_tx.send(DaemonInputCommand::Input(bytes));
                    }
                }

                // ロゴキー（Cmd）の状態を追跡
                if event.physical_key == KeyCode::SuperLeft
                    || event.physical_key == KeyCode::SuperRight
                {
                    logo_pressed = event.state == ElementState::Pressed;
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
                    // TODO: ステータスバーのタブクリック → Daemon ペイン切替
                    // グリッド領域: テキスト選択開始
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
