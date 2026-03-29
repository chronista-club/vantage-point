//! Vantage Point CLI — AI協働開発プラットフォーム
//!
//! Usage:
//!   vp            # 稼働中インスタンス一覧（vp ps）
//!   vp sp start   # SP サーバーを起動
//!   vp hd start   # HD (Claude CLI) を起動
//!   vp hd attach  # HD に TUI 接続
//!   vp mcp        # MCPサーバーとして起動（stdio）
//!   vp world      # TheWorld デーモン管理
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # デバッグ表示モード
//!   VANTAGE_PROJECT_DIR=/path/to/project  # デフォルトプロジェクトディレクトリ
//!
//! Config file: ~/.config/vp/config.toml

use anyhow::Result;
use clap::{Parser, Subcommand};

use vantage_point::cli::{self, parse_debug_env};
use vantage_point::commands;
use vantage_point::config::Config;
use vantage_point::mcp;

use commands::file_cmd::FileCommands;
use commands::midi::MidiCommands;
use commands::pane::PaneCommands;
use commands::tmux_cmd::TmuxCommands;

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
    /// 全 Process + TheWorld を一括再起動
    #[command(alias = "ra")]
    RestartAll,
    /// 稼働中のインスタンス一覧
    #[command(alias = "list")]
    Ps,
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
    /// ペイン操作（コンテンツ表示・レイアウト）
    #[command(subcommand)]
    Pane(PaneCommands),
    /// ファイル監視
    #[command(subcommand)]
    File(FileCommands),

    /// TheWorld 管理 — 全 Process を統括する常駐プロセス
    #[command(alias = "conductor")]
    World {
        /// 待ち受けポート番号（サブコマンド省略時に使用）
        #[arg(short, long, default_value_t = cli::WORLD_PORT)]
        port: u16,
        /// サブコマンド（省略時は start として動作）
        #[command(subcommand)]
        command: Option<commands::world_cmd::WorldCommands>,
    },

    /// SP サーバー管理（HTTP/QUIC サーバーのライフサイクル）
    #[command(subcommand)]
    Sp(commands::sp_cmd::SpCommands),

    /// HD インスタンス管理（tmux + Claude CLI + ccwire）
    #[command(subcommand)]
    Hd(commands::hd_cmd::HdCommands),

    /// tmux ペイン操作（キャプチャ・分割・送信・ダッシュボード）
    #[command(subcommand)]
    Tmux(TmuxCommands),

    /// MIDIハードウェア操作
    #[command(subcommand)]
    Midi(MidiCommands),

    /// SurrealDB デーモン管理
    #[command(subcommand)]
    Db(commands::db_cmd::DbCommands),
}

fn main() -> Result<()> {
    // rustls CryptoProvider を最初に初期化（reqwest/quinn が使う）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // CLIパース（tracingより先に）
    let cli = Cli::parse();

    // Load config
    let config = Config::load().unwrap_or_default();

    // 引数なし → vp ps（稼働中インスタンス一覧）
    let command = cli.command.unwrap_or(Commands::Ps);

    // Initialize tracing
    let debug_mode_for_tracing = parse_debug_env().unwrap_or_default();
    cli::init_tracing(debug_mode_for_tracing, false);

    match command {
        Commands::RestartAll => commands::restart_all::execute(),
        Commands::Ps => cli::list_instances(&config),
        Commands::Config => commands::config::execute(&config),
        Commands::Mcp => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(None))
        }
        Commands::Update { check } => commands::update::execute(check),
        Commands::Pane(cmd) => commands::pane::execute(cmd, &config),
        Commands::File(cmd) => commands::file_cmd::execute(cmd, &config),

        Commands::World { port, command } => {
            let cmd = command.unwrap_or(commands::world_cmd::WorldCommands::Start { port });
            commands::world_cmd::execute(cmd)
        }
        Commands::Sp(cmd) => commands::sp_cmd::execute(cmd, &config),
        Commands::Hd(cmd) => commands::hd_cmd::execute(cmd, &config),

        Commands::Tmux(cmd) => commands::tmux_cmd::execute(cmd, &config),
        Commands::Midi(cmd) => commands::midi::execute(cmd),
        Commands::Db(cmd) => commands::db_cmd::execute(cmd),
    }
}
