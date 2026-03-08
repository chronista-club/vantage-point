//! Vantage Point Agent - AI協働開発プラットフォーム
//!
//! Usage:
//!   vp start    # Processを起動（HTTP + WebSocket）
//!   vp ps       # 稼働中インスタンス一覧
//!   vp mcp      # MCPサーバーとして起動（stdio）
//!   vp daemon   # デーモンプロセス管理
//!   vp midi     # MIDIハードウェア操作
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # デバッグ表示モード
//!   VANTAGE_PROJECT_DIR=/path/to/project  # デフォルトプロジェクトディレクトリ
//!
//! Config file: ~/.config/vp/config.toml

// 開発中のスキャフォールドコードが多いため一時的に抑制
#![allow(dead_code)]

use anyhow::Result;
use clap::{Parser, Subcommand};

mod agent;
mod agui;
mod canvas;
mod capability;
mod cli;
mod commands;
mod config;
pub(crate) mod daemon;
mod file_watcher;
mod mcp;
mod midi;
mod notify;
mod process;
mod protocol;
mod resolve;
mod terminal;
mod terminal_window;
mod tmux;
pub(crate) mod trace_log;
mod tray;
mod tui;

use cli::{DebugModeArg, parse_debug_env};
use config::Config;
use protocol::DebugMode;

use commands::canvas_cmd::CanvasCommands;
use commands::daemon::DaemonCommands;
use commands::file_cmd::FileCommands;
use commands::midi::MidiCommands;
use commands::pane::PaneCommands;

#[derive(Parser)]
#[command(name = "vp")]
#[command(version)]
#[command(about = "Vantage Point Agent - AI協働開発プラットフォーム")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Processを起動（HTTPサーバー + WebSocketハブ）[デフォルト]
    Start {
        /// プロジェクト名またはインデックス（省略時はcwd自動検出）
        #[arg()]
        target: Option<String>,

        /// 待ち受けポート番号
        #[arg(short, long)]
        port: Option<u16>,

        /// ネイティブウィンドウモード（Unison ブリッジ）
        #[arg(long)]
        gui: bool,

        /// ヘッドレスモード（HTTPサーバーのみ）
        #[arg(long)]
        headless: bool,

        /// システムブラウザでWebUIを開く
        #[arg(long)]
        browser: bool,

        /// デバッグ表示モード（VANTAGE_DEBUG環境変数より優先）
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,

        /// プロジェクトディレクトリ（targetより優先）
        #[arg(long, short = 'C')]
        project_dir: Option<String>,

        /// MIDI入力を有効化（ポート番号または名前パターン）
        #[arg(long, short = 'm')]
        midi: Option<String>,
    },
    /// Processを停止
    Stop {
        /// プロジェクト名またはインデックス（省略時はcwd自動検出）
        #[arg()]
        target: Option<String>,
    },
    /// Processを再起動（セッション状態を保持）
    Restart {
        /// プロジェクト名またはインデックス（省略時はcwd自動検出）
        #[arg()]
        target: Option<String>,

        /// ネイティブWebViewの代わりにシステムブラウザを使用
        #[arg(long)]
        browser: bool,

        /// ネイティブWebViewを開く（デフォルトはヘッドレス）
        #[arg(long)]
        gui: bool,
    },
    /// 稼働中のインスタンス一覧
    #[command(alias = "list")]
    Ps,
    /// 指定インスタンスのWebUIを開く
    Open {
        /// プロジェクト名またはインデックス（省略時はcwd自動検出）
        #[arg()]
        target: Option<String>,
    },
    /// 設定と登録済みプロジェクトを表示
    Config,
    /// MCPサーバーとして起動（stdio JSON-RPC）
    Mcp,
    /// self-update: GitHub Releasesから最新バイナリに更新
    Update {
        /// チェックのみ（適用しない）
        #[arg(long)]
        check: bool,
    },
    /// Canvas ウィンドウ操作
    #[command(subcommand)]
    Canvas(CanvasCommands),
    /// ペイン操作（コンテンツ表示・レイアウト）
    #[command(subcommand)]
    Pane(PaneCommands),
    /// ファイル監視
    #[command(subcommand)]
    File(FileCommands),

    // --- App ---
    /// VantagePoint.app を起動（Daemon も自動起動）
    App {
        /// Daemonポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
        /// Daemon起動をスキップ（既に起動している場合）
        #[arg(long)]
        no_daemon: bool,
    },
    /// メニューバーアイコンとして起動（システムトレイ）
    Tray {
        /// MIDI入力を有効化（ポート番号または名前パターン）
        #[arg(long, short = 'm')]
        midi: Option<String>,
    },

    // --- Groups ---
    /// デーモンプロセス管理（Process管理 + ヘルスチェック）
    #[command(subcommand)]
    Daemon(DaemonCommands),
    /// MIDIハードウェア操作
    #[command(subcommand)]
    Midi(MidiCommands),
}

fn main() -> Result<()> {
    // CLIパース（tracingより先に）
    let cli = Cli::parse();

    // Load config
    let config = Config::load().unwrap_or_default();

    // Default to Start if no command given
    let command = cli.command.unwrap_or(Commands::Start {
        target: None,
        port: None,
        gui: false,
        headless: false,
        browser: false,
        debug: None,
        project_dir: None,
        midi: None,
    });

    // Initialize tracing
    let debug_mode_for_tracing = match &command {
        Commands::Start { debug, .. } => debug
            .as_ref()
            .map(|d| DebugMode::from(*d))
            .or_else(parse_debug_env)
            .unwrap_or_default(),
        Commands::Restart { .. } => parse_debug_env().unwrap_or_default(),
        _ => parse_debug_env().unwrap_or_default(),
    };
    cli::init_tracing(debug_mode_for_tracing);

    match command {
        // Core
        Commands::Start {
            target,
            port,
            gui,
            headless,
            browser,
            debug,
            project_dir,
            midi,
        } => commands::start::execute(commands::start::StartOptions {
            target,
            port,
            gui,
            headless,
            browser,
            debug,
            project_dir,
            midi,
            config: &config,
        }),
        Commands::Stop { target } => cli::stop_by_target(target.as_deref(), &config),
        Commands::Restart {
            target,
            browser,
            gui,
        } => commands::restart::execute(target.as_deref(), browser, !gui, &config),
        Commands::Ps => cli::list_instances(&config),
        Commands::Open { target } => cli::open_by_target(target.as_deref(), &config),
        Commands::Config => commands::config::execute(&config),
        Commands::Mcp => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(33000))
        }
        Commands::Update { check } => commands::update::execute(check),
        Commands::Canvas(cmd) => commands::canvas_cmd::execute(cmd, &config),
        Commands::Pane(cmd) => commands::pane::execute(cmd, &config),
        Commands::File(cmd) => commands::file_cmd::execute(cmd, &config),

        // App
        Commands::App { port, no_daemon } => commands::app::execute(port, no_daemon),
        Commands::Tray { midi } => {
            // MIDI をバックグラウンドスレッドで起動
            if let Some(ref midi_arg) = midi {
                let mut config = midi::MidiConfig::default();
                config
                    .note_actions
                    .insert(36, midi::MidiAction::OpenWebUI { port: None });
                config
                    .note_actions
                    .insert(37, midi::MidiAction::CancelChat { port: None });
                config
                    .note_actions
                    .insert(38, midi::MidiAction::ResetSession { port: None });

                let (port_idx, config) = if let Ok(idx) = midi_arg.parse::<usize>() {
                    (Some(idx), config)
                } else {
                    let mut c = config;
                    c.port_pattern = Some(midi_arg.clone());
                    (None, c)
                };

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async {
                        if let Err(e) = midi::run_midi(port_idx, config, 33000).await {
                            tracing::error!("MIDI error: {}", e);
                        }
                    });
                });
                tracing::info!("MIDI monitoring enabled");
            }

            tray::run_tray()
        }

        // Groups
        Commands::Daemon(cmd) => commands::daemon::execute(cmd),
        Commands::Midi(cmd) => commands::midi::execute(cmd),
    }
}
