//! Vantage Point Agent - AI協働開発プラットフォーム
//!
//! Usage:
//!   vp start    # デーモンを起動（HTTP + WebSocket）
//!   vp mcp      # MCPサーバーとして起動（stdio）
//!   vp status   # デーモンの稼働状態を確認
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
mod capability;
mod cli;
mod commands;
mod config;
mod mcp;
mod midi;
mod park;
mod protocol;
mod stand;
mod tray;
mod webview;
mod world;

use cli::{DebugModeArg, parse_debug_env};
use config::Config;
use protocol::DebugMode;

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
    /// 設定と登録済みプロジェクトを表示
    Config,
    /// MCPサーバーとして起動（stdio JSON-RPC）
    Mcp,
    /// Standの稼働状態を確認
    Status {
        /// 確認するポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
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
    /// メニューバーアイコンとして起動（システムトレイ）
    Tray {
        /// MIDI入力を有効化（ポート番号または名前パターン）
        #[arg(long, short = 'm')]
        midi: Option<String>,
    },
    /// WebViewウィンドウのみを開く（Standは別途起動済み）
    Webview {
        /// 接続先ポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// 利用可能なMIDI入力ポート一覧
    MidiPorts,
    /// MIDI入力の監視を開始
    Midi {
        /// 接続するMIDIポート番号
        #[arg(short, long)]
        port: Option<usize>,
        /// アクション送信先のデーモンポート
        #[arg(short = 'P', long, default_value = "33000")]
        stand_port: u16,
    },
    /// LPD8コントローラー設定
    #[command(subcommand)]
    Lpd8(Lpd8Commands),
    /// Stand Conductorを起動（複数Standを管理）
    Conductor {
        /// 待ち受けポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
    },
    /// VantagePoint.app を起動（Conductor Standも自動起動）
    App {
        /// Conductorポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
        /// Conductor起動をスキップ（既に起動している場合）
        #[arg(long)]
        no_conductor: bool,
    },
    /// The World 管理（常駐コアプロセス）
    #[command(subcommand)]
    World(WorldCommands),
    /// Paisley Park 管理（プロジェクト単位 Agent）
    #[command(subcommand)]
    Park(ParkCommands),
    /// Gold Experience（創造・回復）
    #[command(subcommand)]
    Ge(GeCommands),
}

/// The World サブコマンド（JoJo Part 3 DIO のスタンド）
#[derive(Subcommand)]
pub(crate) enum WorldCommands {
    /// The World を起動「時よ止まれ」
    #[command(alias = "start")]
    Up {
        /// デバッグモード
        #[arg(long, short)]
        debug: bool,
        /// 静的ファイルディレクトリ（ViewPoint 用）
        #[arg(long, short = 's')]
        static_dir: Option<std::path::PathBuf>,
        /// MIDI 入力を有効化（ポートパターンで検索）
        #[arg(long, short = 'm')]
        midi: Option<String>,
        /// Requiem モード（GER: 自動防御強化）「真実にはたどり着けない」
        #[arg(long, short = 'r')]
        requiem: bool,
    },
    /// The World を停止「そして時は動き出す」
    #[command(alias = "stop")]
    Down,
    /// The World のステータス確認
    Status,
    /// ViewPoint を WebView で開く
    Open,
    /// MCP Server として起動（Claude CLI 連携）
    Mcp,
    /// スナップショット作成「時を止める」
    Snapshot {
        /// スナップショット名
        name: String,
        /// 説明（オプション）
        #[arg(long, short)]
        description: Option<String>,
    },
    /// スナップショットから復元「ゼロに戻す」
    Restore {
        /// スナップショット名
        name: String,
    },
    /// スナップショット一覧
    Snapshots,
    /// Guardian（自動防御）管理
    #[command(subcommand)]
    Guardian(GuardianCommands),
}

/// Paisley Park サブコマンド（JoJo Part 8 広瀬康穂のスタンド）
#[derive(Subcommand)]
pub(crate) enum ParkCommands {
    /// Paisley Park を起動「情報を収集します」
    #[command(alias = "start")]
    Up {
        /// プロジェクトディレクトリ
        #[arg(short = 'C', long)]
        project_dir: Option<String>,
        /// 使用するポート番号
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Paisley Park を停止
    #[command(alias = "stop")]
    Down {
        /// プロジェクトID
        project_id: Option<String>,
    },
    /// 登録済み Paisley Park 一覧
    List,
}

/// Guardian サブコマンド（GER の自動防御機能）
#[derive(Subcommand)]
pub(crate) enum GuardianCommands {
    /// Guardian のステータス確認
    Status,
    /// Guardian を有効化
    Enable,
    /// Guardian を無効化
    Disable,
    /// 保護ルール一覧
    Rules,
    /// 保護ルールを追加
    AddRule {
        /// ルール名
        name: String,
        /// ルールパターン（glob形式）
        pattern: String,
    },
}

/// Gold Experience サブコマンド（JoJo Part 5 ジョルノのスタンド）
#[derive(Subcommand)]
pub(crate) enum GeCommands {
    /// プロジェクト/コード生成「生命を与える」
    Scaffold {
        /// テンプレート名
        template: String,
        /// 出力先ディレクトリ
        #[arg(short, long, default_value = ".")]
        output: std::path::PathBuf,
        /// プロジェクト/モジュール名
        #[arg(short, long)]
        name: Option<String>,
        /// 説明
        #[arg(short, long)]
        description: Option<String>,
    },
    /// コード修復「回復」
    Heal {
        /// 修復対象ディレクトリ
        #[arg(short = 'C', long, default_value = ".")]
        dir: std::path::PathBuf,
        /// 修復アクション (format, lint-fix, all)
        #[arg(short, long, default_value = "all")]
        action: String,
    },
    /// テンプレート一覧
    Templates,
    /// プロジェクト検出
    Detect {
        /// 検出対象ディレクトリ
        #[arg(default_value = ".")]
        dir: std::path::PathBuf,
    },
    /// 成長統計
    Stats,
}

/// LPD8サブコマンド
#[derive(Subcommand)]
pub(crate) enum Lpd8Commands {
    /// VP用設定をLPD8 Program 1に書き込む
    Write {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "LPD8")]
        port: String,
        /// 書き込み先プログラム番号（1-4）
        #[arg(short, long, default_value = "1")]
        program: u8,
    },
    /// LPD8から現在の設定を読み取る
    Read {
        /// MIDIポート名のパターン
        #[arg(long, default_value = "LPD8")]
        port: String,
        /// 読み取り元プログラム番号（1-4）
        #[arg(short, long, default_value = "1")]
        program: u8,
    },
    /// アクティブプログラムを切り替える
    Switch {
        /// プログラム番号（1-4）
        program: u8,
        /// MIDIポート名のパターン
        #[arg(long, default_value = "LPD8")]
        port: String,
    },
    /// 利用可能なMIDI出力ポート一覧
    Ports,
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
    // Startコマンドの場合はdebugフラグを取得、その他はデフォルト
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
        Commands::Start {
            project_index,
            port,
            headless,
            browser,
            debug,
            project_dir,
            midi,
        } => commands::start::execute(
            project_index,
            port,
            headless,
            browser,
            debug,
            project_dir,
            midi,
            &config,
        ),
        Commands::Config => commands::config::execute(&config),
        Commands::Mcp => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(33000))
        }
        Commands::Status { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cli::check_status(port))
        }
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
        Commands::Tray { midi } => {
            // Start MIDI in background thread if enabled
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
        Commands::Webview { port } => webview::run_webview(port),
        Commands::MidiPorts => {
            midi::print_ports();
            Ok(())
        }
        Commands::Midi { port, stand_port } => {
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

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(midi::run_midi_interactive(port, config, stand_port))
        }
        Commands::Lpd8(cmd) => commands::lpd8::execute(cmd),
        Commands::Conductor { port } => {
            println!("🎭 Starting Stand Conductor on port {}...", port);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(stand::run_conductor(port))
        }
        Commands::App { port, no_conductor } => commands::app::execute(port, no_conductor),
        Commands::World(cmd) => commands::world::execute(cmd),
        Commands::Park(cmd) => commands::park::execute(cmd),
        Commands::Ge(cmd) => commands::ge::execute(cmd),
    }
}
