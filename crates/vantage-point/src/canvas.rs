//! Canvas ウィンドウ（WebViewのみ）
//!
//! ProcessのWeb UIをスタンドアロンウィンドウで表示。
//! ターミナルとは独立したウィンドウで、フォーカス干渉なし。
//!
//! ## シングルトン管理
//!
//! Canvas は PID ファイル (`/tmp/vantage-point/canvas.pid`) でグローバルシングルトンとして管理。
//! 複数プロジェクトが同時に動いていても、Canvas ウィンドウは1つだけ。
//! Lane モード (`?lanes=1`) で全プロジェクトを Lane バーで切り替え表示。

use std::path::PathBuf;

use tao::dpi::LogicalSize;

use crate::terminal_window::create_menu_bar;

/// Canvas PID ファイルのパス
fn canvas_pid_path() -> PathBuf {
    PathBuf::from("/tmp/vantage-point/canvas.pid")
}

/// 既存の Canvas プロセスの PID を取得（生存確認付き）
pub fn find_running_canvas() -> Option<u32> {
    let path = canvas_pid_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;

    // プロセスが生きているか確認
    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
    if alive {
        Some(pid)
    } else {
        // ゴースト PID ファイルを削除
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// Canvas PID ファイルを書き込み
fn write_canvas_pid(pid: u32) {
    let path = canvas_pid_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, pid.to_string());
}

/// Canvas PID ファイルを削除
fn remove_canvas_pid() {
    let _ = std::fs::remove_file(canvas_pid_path());
}

/// Canvas シングルトンを起動（既存があればその PID を返す）
///
/// Lane モード対応: port が指定されていれば 1:1 モード、なければ Lane モードで起動。
pub fn ensure_canvas_running(
    port: u16,
    lanes: bool,
    project_name: Option<&str>,
) -> anyhow::Result<u32> {
    // 既存の Canvas が動いていればそれを使う
    if let Some(pid) = find_running_canvas() {
        tracing::info!("Canvas already running (pid={})", pid);
        return Ok(pid);
    }

    // 新規起動
    let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());
    let mut args = vec![
        "canvas".to_string(),
        "internal".to_string(),
        "--port".to_string(),
        port.to_string(),
    ];
    if let Some(name) = project_name {
        args.push("--name".to_string());
        args.push(name.to_string());
    }
    if lanes {
        args.push("--lanes".to_string());
    }

    let child = std::process::Command::new(&vp_bin)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    write_canvas_pid(pid);
    tracing::info!("Canvas launched (pid={}, lanes={})", pid, lanes);
    Ok(pid)
}

/// Canvas 接続先を決定
///
/// TheWorld を自動起動し、WORLD_PORT で LANE モード接続。
/// TheWorld 起動失敗時は sp_port にフォールバック。
/// LANE モードは常に有効。
pub fn canvas_target(sp_port: u16) -> (u16, bool) {
    // TheWorld が未起動なら自動起動を試みる
    if crate::daemon::process::is_daemon_running().is_none() {
        if let Err(e) = crate::daemon::process::ensure_daemon_running(crate::cli::WORLD_PORT) {
            tracing::warn!("TheWorld 自動起動失敗、SP にフォールバック: {}", e);
            return (sp_port, true);
        }
    }
    (crate::cli::WORLD_PORT, true)
}

/// Canvas シングルトンを停止
pub fn stop_canvas() -> Option<u32> {
    let pid = find_running_canvas()?;
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    remove_canvas_pid();
    tracing::info!("Canvas stopped (pid={})", pid);
    Some(pid)
}

/// 別プロセスで Canvas ウィンドウを起動（レガシー互換）
pub fn run_canvas_detached(port: u16, project_name: &str) -> anyhow::Result<()> {
    let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());
    std::process::Command::new(&vp_bin)
        .args([
            "canvas",
            "internal",
            "--port",
            &port.to_string(),
            "--name",
            project_name,
        ])
        .spawn()?;
    Ok(())
}

/// キャンバスウィンドウ（WebViewのみ、ターミナルなし）
///
/// Processの Web UIをスタンドアロンウィンドウで表示。
/// ターミナルとは独立したウィンドウで、フォーカス干渉なし。
pub fn run_canvas(port: u16, project_name: &str, lanes: bool) -> anyhow::Result<()> {
    use tao::{
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    // PID ファイルに自分を登録
    write_canvas_pid(std::process::id());

    // macOS: バックグラウンドから spawn されてもウィンドウを表示するために
    // ActivationPolicy を Regular に設定
    #[cfg(target_os = "macos")]
    let event_loop = {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        let mut event_loop = EventLoop::new();
        event_loop.set_activation_policy(ActivationPolicy::Regular);
        event_loop
    };
    #[cfg(not(target_os = "macos"))]
    let event_loop = EventLoop::new();

    let title = if lanes {
        "PP Canvas".to_string()
    } else {
        format!("PP: {} — Canvas", project_name)
    };

    let window = WindowBuilder::new()
        .with_title(&title)
        .with_inner_size(LogicalSize::new(800.0, 900.0))
        .build(&event_loop)?;

    // メニューバー（コピー/ペースト対応）
    let menu = create_menu_bar();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    // フォーカスは奪わない（TUI に留まる）
    // ユーザーがクリック or Cmd+Tab で切り替え

    // HTML を HTTP で取得してから with_html で直接ロード
    // wry (WebKit) は HTTP キャッシュを頑固に保持するため、
    // with_url ではなく with_html で完全にバイパスする
    let canvas_url = if lanes {
        format!("http://localhost:{}/canvas?project={}", port, project_name)
    } else {
        format!("http://localhost:{}/canvas?direct", port)
    };
    // HTML を取得（最大 5 秒待ち）
    let html = {
        let mut html = None;
        for _ in 0..50 {
            if let Ok(resp) = reqwest::blocking::get(&canvas_url) {
                if let Ok(text) = resp.text() {
                    // __VP_WS_HOST__ プレースホルダーを実際の host:port に置換
                    // with_html では window.location.host が空になるため必須
                    let text = text.replace("__VP_WS_HOST__", &format!("localhost:{}", port));
                    html = Some(text);
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        html.unwrap_or_else(|| include_str!("../../../web/canvas.html").to_string())
    };
    let _webview = WebViewBuilder::new()
        .with_html(&html)
        .with_devtools(true)
        .build(&window)?;

    tracing::info!(
        "Canvas window opened via HTTP (port={}, lanes={})",
        port,
        lanes
    );

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            // ウィンドウ閉じ時に PID ファイルを削除
            remove_canvas_pid();
            *control_flow = ControlFlow::Exit;
        }

        let _ = &_webview;
        let _ = &menu;
    });
}
