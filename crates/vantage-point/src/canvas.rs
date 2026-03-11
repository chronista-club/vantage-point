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
pub fn ensure_canvas_running(port: u16, lanes: bool) -> anyhow::Result<u32> {
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

/// Canvas 接続先を決定（TheWorld フォールバック付き）
///
/// TheWorld 稼働中 → (WORLD_PORT, lanes=true)
/// 未稼働 → (sp_port, lanes=false)
pub fn canvas_target(sp_port: u16) -> (u16, bool) {
    if crate::daemon::process::is_daemon_running().is_some() {
        (crate::cli::WORLD_PORT, true)
    } else {
        (sp_port, false)
    }
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
        "VP Canvas".to_string()
    } else {
        format!("VP: {} — Canvas", project_name)
    };

    let window = WindowBuilder::new()
        .with_title(&title)
        .with_inner_size(LogicalSize::new(800.0, 900.0))
        .build(&event_loop)?;

    // メニューバー（コピー/ペースト対応）
    let menu = create_menu_bar();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    // macOS: ウィンドウを前面に表示
    window.set_focus();

    // HTTP URL で Canvas を提供（CDN スクリプトが正常に読み込まれるように）
    // キャッシュは canvas_handler の no-store ヘッダーで回避
    let canvas_url = if lanes {
        format!("http://localhost:{}/canvas?lanes=1", port)
    } else {
        format!("http://localhost:{}/canvas", port)
    };
    let _webview = WebViewBuilder::new()
        .with_url(&canvas_url)
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
