//! Vantage Point Agent - AI協働開発プラットフォーム
//!
//! Usage:
//!   vp start    # Standを起動（HTTP + WebSocket）
//!   vp ps       # 稼働中インスタンス一覧
//!   vp mcp      # MCPサーバーとして起動（stdio）
//!   vp daemon   # デーモンプロセス管理
//!   vp midi     # MIDIハードウェア操作
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # デバッグ表示モード
//!   VANTAGE_PROJECT_DIR=/path/to/project  # デフォルトプロジェクトディレクトリ
//!
//! Config file: ~/.config/vantage/config.toml

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
mod daemon;
mod mcp;
mod midi;
mod protocol;
mod stand;
mod terminal;
mod terminal_window;
mod tray;

use cli::{DebugModeArg, parse_debug_env};
use config::Config;
use protocol::DebugMode;

use commands::daemon::DaemonCommands;
use commands::midi::MidiCommands;

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
    /// Standを起動（HTTPサーバー + WebSocketハブ）[デフォルト]
    Start {
        /// プロジェクト番号（`vp config`で確認、1始まり）
        #[arg()]
        project_index: Option<usize>,

        /// 待ち受けポート番号
        #[arg(short, long)]
        port: Option<u16>,

        /// ビューアを開かない（ヘッドレスモード）
        #[arg(long)]
        headless: bool,

        /// ネイティブWebViewの代わりにシステムブラウザを使用
        #[arg(long)]
        browser: bool,

        /// デバッグ表示モード（VANTAGE_DEBUG環境変数より優先）
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,

        /// プロジェクトディレクトリ（project_indexより優先）
        #[arg(long, short = 'C')]
        project_dir: Option<String>,

        /// MIDI入力を有効化（ポート番号または名前パターン）
        #[arg(long, short = 'm')]
        midi: Option<String>,
    },
    /// Standを停止
    Stop {
        /// 停止するStandのポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// Standを再起動（セッション状態を保持）
    Restart {
        /// 再起動するStandのポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,

        /// ネイティブWebViewの代わりにシステムブラウザを使用
        #[arg(long)]
        browser: bool,

        /// ビューアを開かない（ヘッドレスモード）
        #[arg(long)]
        headless: bool,
    },
    /// 稼働中のインスタンス一覧
    #[command(alias = "list")]
    Ps,
    /// 指定インスタンスのWebUIを開く
    Open {
        /// インスタンス番号（`vp ps`で確認、1始まり）
        #[arg(default_value = "1")]
        index: usize,
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
    /// デーモンプロセス管理（Stand管理 + ヘルスチェック）
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
        project_index: None,
        port: None,
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
            project_index,
            port,
            headless,
            browser,
            debug,
            project_dir,
            midi,
        } => commands::start::execute(commands::start::StartOptions {
            project_index,
            port,
            headless,
            browser,
            debug,
            project_dir,
            midi,
            config: &config,
        }),
        Commands::Stop { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cli::stop_stand(port))
        }
        Commands::Restart {
            port,
            browser,
            headless,
        } => commands::restart::execute(port, browser, headless),
        Commands::Ps => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cli::list_instances())
        }
        Commands::Open { index } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cli::open_instance(index))
        }
        Commands::Config => commands::config::execute(&config),
        Commands::Mcp => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(33000))
        }
        Commands::Update { check } => commands::update::execute(check),

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
