//! vp tui — ratatui コンソール
//!
//! tmux セッションをヘッダー/フッター付きで表示する TUI コンソール。
//! どのターミナル（Kitty, Ghostty, iTerm, VantagePoint.app）でも
//! 同じ見た目・操作感を提供する「ターミナル体験の標準レイヤー」。

use anyhow::Result;

use crate::config::Config;
use crate::tmux;

/// VP Shell コマンドを実行
fn execute_command(input: &str, session_name: &str) -> String {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let cmd = parts[0];
    let args = parts.get(1).copied().unwrap_or("");

    match cmd {
        "split" | "sp" => {
            // tmux split-window
            let tmux_bin = if std::path::Path::new("/opt/homebrew/bin/tmux").exists() {
                "/opt/homebrew/bin/tmux"
            } else {
                "tmux"
            };
            let status = std::process::Command::new(tmux_bin)
                .args(["split-window", "-t", session_name, "-d"])
                .status();
            match status {
                Ok(s) if s.success() => "Split created".to_string(),
                _ => "Split failed".to_string(),
            }
        }
        "vsplit" | "vs" => {
            let tmux_bin = if std::path::Path::new("/opt/homebrew/bin/tmux").exists() {
                "/opt/homebrew/bin/tmux"
            } else {
                "tmux"
            };
            let status = std::process::Command::new(tmux_bin)
                .args(["split-window", "-h", "-t", session_name, "-d"])
                .status();
            match status {
                Ok(s) if s.success() => "Vertical split created".to_string(),
                _ => "VSplit failed".to_string(),
            }
        }
        "q" | "quit" => {
            std::process::exit(0);
        }
        "help" | "h" => {
            ":split :vsplit :quit :help".to_string()
        }
        _ => {
            if args.is_empty() {
                format!("Unknown command: {}", cmd)
            } else {
                format!("Unknown command: {} {}", cmd, args)
            }
        }
    }
}

/// vp tui コマンドを実行
pub fn execute(session: Option<String>, config: &Config) -> Result<()> {
    // セッション名を解決（指定なしなら cwd から自動検出）
    let session_name = if let Some(s) = session {
        s
    } else {
        let cwd = std::env::current_dir()?;
        let project_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default");
        // config から名前を解決（登録済みプロジェクトなら config の名前を優先）
        let resolved_name = config
            .projects
            .iter()
            .find(|p| {
                Config::normalize_path(std::path::Path::new(&p.path))
                    == Config::normalize_path(&cwd)
            })
            .map(|p| p.name.as_str())
            .unwrap_or(project_name);
        tmux::session_name(resolved_name)
    };

    // tmux セッションが存在するか確認
    if !tmux::session_exists(&session_name) {
        eprintln!("tmux session '{}' not found.", session_name);
        eprintln!("Start a project first: vp start");
        std::process::exit(1);
    }

    // ratatui TUI を起動
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_tui_console(&session_name))?;

    Ok(())
}

/// ratatui コンソールのメインループ
async fn run_tui_console(session_name: &str) -> Result<()> {
    use std::io;
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Color, Style};
    use ratatui::widgets::Paragraph;

    // ターミナル初期化
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    crossterm::execute!(stdout, crossterm::event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // PTY でtmux attach を起動
    let size = terminal.size()?;
    let pty_rows = size.height.saturating_sub(2); // ヘッダー1行 + フッター1行
    let pty_cols = size.width;

    let term_state = std::sync::Arc::new(std::sync::Mutex::new(
        crate::terminal::state::TerminalState::new(pty_cols as usize, pty_rows as usize),
    ));

    // tmux attach コマンドを PTY で起動
    let tmux_bin = if std::path::Path::new("/opt/homebrew/bin/tmux").exists() {
        "/opt/homebrew/bin/tmux"
    } else {
        "tmux"
    };
    let pty_command = format!("{} attach-session -t {}", tmux_bin, session_name);

    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("/"))
        .to_string_lossy()
        .to_string();

    // PTY プロセスを起動（portable-pty）
    use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: pty_rows,
        cols: pty_cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(["-l", "-c", &pty_command]);
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave); // slave は子プロセスが使う

    let reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // PTY 出力リーダースレッド
    let term_state_for_reader = term_state.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut state = term_state_for_reader.lock().unwrap();
                    state.feed_bytes(&buf[..n]);
                }
            }
        }
    });

    // メインループ
    let project_name = session_name
        .strip_suffix("-vp")
        .unwrap_or(session_name)
        .to_string();

    // Stand 情報を定期取得（5秒間隔、バックグラウンド）
    let header_text = std::sync::Arc::new(std::sync::Mutex::new(
        format!("  {}  │  connecting...", project_name),
    ));
    {
        let header_text = header_text.clone();
        let project_name = project_name.clone();
        // SP のポートを発見（cwd ベースで TheWorld に問い合わせ）
        let cwd_for_discover = cwd.clone();
        let port = crate::discovery::find_by_project_blocking(&cwd_for_discover)
            .map(|p| p.port);
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap_or_default();
            loop {
                if let Some(port) = port {
                    let url = format!("http://[::1]:{}/api/health", port);
                    if let Ok(resp) = client.get(&url).send() {
                        if let Ok(json) = resp.json::<serde_json::Value>() {
                            let mut parts = vec![format!("  {}", project_name)];

                            // Stand ステータス
                            if let Some(stands) = json.get("stands").and_then(|s| s.as_object()) {
                                let icons: Vec<&str> = stands
                                    .iter()
                                    .filter(|(_, v)| {
                                        v.get("status").and_then(|s| s.as_str()) != Some("disabled")
                                    })
                                    .map(|(k, _)| match k.as_str() {
                                        "heavens_door" => "HD",
                                        "paisley_park" => "PP",
                                        "gold_experience" => "GE",
                                        "hermit_purple" => "HP",
                                        _ => "??",
                                    })
                                    .collect();
                                if !icons.is_empty() {
                                    parts.push(icons.join(" "));
                                }
                            }

                            // 起動時刻
                            if let Some(started) = json.get("started_at").and_then(|s| s.as_str()) {
                                if let Some(time_part) = started.split('T').nth(1) {
                                    let short_time = &time_part[..5]; // HH:MM
                                    parts.push(short_time.to_string());
                                }
                            }

                            *header_text.lock().unwrap() = parts.join("  │  ");
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });
    }

    // コマンドモード状態
    let mut command_mode = false;
    let mut command_input = String::new();
    let mut status_message: Option<String> = None;

    loop {
        // 描画
        let current_header = header_text.lock().unwrap().clone();
        let footer_text = if command_mode {
            format!("  :{}", command_input)
        } else if let Some(ref msg) = status_message {
            format!("  {}", msg)
        } else {
            "  :cmd │ ⌘D Split │ Ctrl+C Quit".to_string()
        };

        terminal.draw(|frame| {
            let chunks = Layout::vertical([
                Constraint::Length(1), // ヘッダー
                Constraint::Min(1),    // ターミナル
                Constraint::Length(1), // フッター
            ])
            .split(frame.area());

            // ヘッダー（動的: Stand + 時刻）
            let header = Paragraph::new(current_header)
                .style(Style::default().fg(Color::Gray).bg(Color::DarkGray));
            frame.render_widget(header, chunks[0]);

            // ターミナルビューポート
            let state = term_state.lock().unwrap();
            let snapshot = state.snapshot();
            let widget = crate::tui::terminal_widget::TerminalView::new(&snapshot);
            frame.render_widget(widget, chunks[1]);

            // フッター（通常 or コマンド入力）
            let footer_style = if command_mode {
                Style::default().fg(Color::White).bg(Color::DarkGray)
            } else {
                Style::default().fg(Color::DarkGray).bg(Color::DarkGray)
            };
            let footer = Paragraph::new(footer_text.clone()).style(footer_style);
            frame.render_widget(footer, chunks[2]);
        })?;

        // イベント処理（10ms ポーリング）
        if event::poll(std::time::Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if command_mode {
                    // コマンドモード: 入力をバッファに蓄積
                    match key.code {
                        KeyCode::Enter => {
                            // コマンド実行
                            let result = execute_command(&command_input, session_name);
                            status_message = Some(result);
                            command_input.clear();
                            command_mode = false;
                        }
                        KeyCode::Esc => {
                            // コマンドモード解除
                            command_input.clear();
                            command_mode = false;
                        }
                        KeyCode::Backspace => {
                            command_input.pop();
                            if command_input.is_empty() {
                                command_mode = false;
                            }
                        }
                        KeyCode::Char(c) => {
                            command_input.push(c);
                        }
                        _ => {}
                    }
                    continue;
                }

                // 通常モード
                // Ctrl+C で終了
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('c')
                {
                    break;
                }

                // ':' でコマンドモードに入る
                if key.code == KeyCode::Char(':') && key.modifiers.is_empty() {
                    command_mode = true;
                    command_input.clear();
                    status_message = None;
                    continue;
                }

                // ステータスメッセージをクリア（任意のキーで）
                status_message = None;

                // キー入力を PTY に送信
                let app_cursor = {
                    let state = term_state.lock().unwrap();
                    state.app_cursor_mode()
                };
                let bytes = crate::tui::input::key_to_pty_bytes(key, app_cursor);
                if !bytes.is_empty() {
                    use std::io::Write;
                    let _ = writer.write_all(&bytes);
                    let _ = writer.flush();
                }
            }
        }

        // 子プロセスが終了したかチェック
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
    }

    // クリーンアップ
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;

    Ok(())
}
