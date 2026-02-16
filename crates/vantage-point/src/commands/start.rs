//! `vp start` コマンドの実行ロジック

use anyhow::Result;

use crate::cli::{DebugModeArg, PORT_RANGE_END, PORT_RANGE_START, parse_debug_env};
use crate::config::Config;
use crate::protocol::DebugMode;

/// `vp start` の起動オプション
pub struct StartOptions<'a> {
    pub project_index: Option<usize>,
    pub port: Option<u16>,
    pub headless: bool,
    pub browser: bool,
    pub debug: Option<DebugModeArg>,
    pub project_dir: Option<String>,
    pub midi: Option<String>,
    pub config: &'a Config,
}

/// `vp start` を実行
pub fn execute(opts: StartOptions) -> Result<()> {
    let StartOptions {
        project_index,
        port,
        headless,
        browser,
        debug,
        project_dir,
        midi,
        config,
    } = opts;
    // Resolve project directory and effective index
    // Priority: --project-dir > project_index > cwd > config default
    // 相対パスは絶対パスに変換される
    //
    // resolved_index: ポート割り当てに使う 0-based インデックス
    // CWD がconfig内プロジェクトに一致すれば、そのインデックスを使う
    let (resolved_project_dir, resolved_index) = if let Some(ref dir) = project_dir {
        // 1. Explicit --project-dir takes precedence
        let dir_normalized = Config::normalize_path(std::path::Path::new(dir));
        let idx = config.find_project_index(&dir_normalized);
        (dir_normalized, idx)
    } else if let Some(idx) = project_index {
        // 2. Project index from config (convert 1-based to 0-based)
        if idx == 0 || idx > config.projects.len() {
            eprintln!(
                "✗ Invalid project index {}. Use `vp config` to list projects (1–{}).",
                idx,
                config.projects.len()
            );
            std::process::exit(1);
        }
        let project = &config.projects[idx - 1];
        println!("📁 Project: {} ({})", project.name, project.path);
        (
            Config::normalize_path(std::path::Path::new(&project.path)),
            Some(idx - 1),
        )
    } else {
        // 3. cwd > config default → CWD がconfig内プロジェクトに一致するか検索
        let dir = Config::resolve_project_dir(None, config);
        let idx = config.find_project_index(&dir);
        if let Some(i) = idx {
            println!(
                "📁 Project: {} ({})",
                config.projects[i].name, config.projects[i].path
            );
        }
        (dir, idx)
    };

    // Resolve port: CLI explicit > project index based (33000 + index)
    let resolved_port = if let Some(p) = port {
        // Explicit CLI port
        p
    } else {
        // Port based on resolved index: project #1(idx=0) → 33000, #2(idx=1) → 33001, etc.
        let idx = resolved_index.unwrap_or(0) as u16;
        let p = PORT_RANGE_START + idx;
        if p > PORT_RANGE_END {
            eprintln!(
                "✗ Project index {} exceeds port range. Max {} projects supported.",
                idx,
                PORT_RANGE_END - PORT_RANGE_START + 1
            );
            std::process::exit(1);
        }
        println!("🔌 Using port {}", p);
        p
    };

    // Determine debug mode: CLI flag > env var > default
    let debug_mode = debug
        .map(DebugMode::from)
        .or_else(parse_debug_env)
        .unwrap_or_default();

    if debug_mode != DebugMode::None {
        tracing::info!("Debug mode: {:?}", debug_mode);
    }

    tracing::info!("Project dir: {}", resolved_project_dir);

    // Setup MIDI config if enabled
    let midi_config = midi.as_ref().map(|midi_arg| {
        let mut config = crate::midi::MidiConfig::default();
        // LPD8 pad mappings (notes 36-43)
        config
            .note_actions
            .insert(36, crate::midi::MidiAction::OpenWebUI { port: None });
        config
            .note_actions
            .insert(37, crate::midi::MidiAction::CancelChat { port: None });
        config
            .note_actions
            .insert(38, crate::midi::MidiAction::ResetSession { port: None });

        // Set port pattern if provided as string, or port index if numeric
        if let Ok(idx) = midi_arg.parse::<usize>() {
            config.port_index = Some(idx);
        } else {
            config.port_pattern = Some(midi_arg.clone());
        }
        config
    });

    // Create CapabilityConfig
    let cap_config = crate::stand::CapabilityConfig {
        project_dir: resolved_project_dir.clone(),
        midi_config,
        bonjour_port: Some(resolved_port), // Bonjour広告を有効化
    };

    if headless || browser {
        // Headless or browser mode - use tokio runtime
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let server_handle = tokio::spawn(async move {
                crate::stand::run(resolved_port, false, debug_mode, cap_config).await
            });

            if browser {
                // Wait for server to start, then open browser
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let url = format!("http://localhost:{}", resolved_port);
                tracing::info!("Opening in browser: {}", url);
                let _ = open::that(&url);
            }

            server_handle.await?
        })
    } else {
        // WebView mode - run server in background thread, WebView on main thread
        let server_thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async {
                crate::stand::run(resolved_port, false, debug_mode, cap_config).await
            })
        });

        // Wait a bit for server to start
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Run WebView on main thread (required by macOS)
        let webview_result = crate::terminal_window::run_terminal(resolved_port);

        match webview_result {
            Ok(()) => {
                tracing::info!("WebView closed");
            }
            Err(e) => {
                tracing::error!("WebView error: {}", e);
            }
        }

        // Server thread will be terminated when main exits
        drop(server_thread);
        Ok(())
    }
}
