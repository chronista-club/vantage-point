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

    /// Stone Free 🧵 — worker workspace 管理（旧 ccws、Phase 1 で統合）
    #[command(subcommand, alias = "workspace")]
    Ws(WsCommands),

    /// Port Layout — deterministic 透過的固定 port の計算・表示
    #[command(subcommand)]
    Port(commands::port_cmd::PortCommands),
}

/// Stone Free worker workspace コマンド（vp-ccws library への薄い wrapper）
#[derive(Subcommand)]
enum WsCommands {
    /// 新しい worker 環境を作成（clone + symlink + setup）
    New {
        /// Worker 名
        name: String,
        /// 作成するブランチ名
        branch: String,
        /// 既存 worker を上書き
        #[arg(long, short)]
        force: bool,
    },
    /// 現在の dirty state を新しい worker 環境に fork
    Fork {
        /// Worker 名
        name: String,
        /// 作成するブランチ名
        branch: String,
        /// 既存 worker を上書き
        #[arg(long, short)]
        force: bool,
    },
    /// worker 環境一覧
    #[command(alias = "list")]
    Ls,
    /// worker 環境のパスを表示
    Path {
        /// Worker 名
        name: String,
    },
    /// worker 環境を削除
    Rm {
        /// 削除する Worker 名（--all 指定時は不要）
        name: Option<String>,
        /// 全 worker を削除
        #[arg(long)]
        all: bool,
        /// 確認なしで強制削除
        #[arg(long, short)]
        force: bool,
    },
    /// 全 worker の状態表示
    Status,
    /// branch が main に merge 済の worker を削除
    Cleanup {
        /// 確認なしで強制削除
        #[arg(long, short)]
        force: bool,
    },
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

        Commands::Ws(cmd) => execute_ws(cmd),
        Commands::Port(cmd) => commands::port_cmd::execute(cmd),
    }
}

/// Stone Free 🧵 worker workspace 操作を vp-ccws library に委譲
///
/// Phase 2 追加: worker 作成/削除時に TheWorld の msgbox registry に
/// `worker-{name}@{project}` actor を register/unregister（best-effort、
/// TheWorld 未起動でも workspace 操作自体は成功させる）。
fn execute_ws(cmd: WsCommands) -> Result<()> {
    use vp_ccws::commands as ws;

    match cmd {
        WsCommands::New {
            name,
            branch,
            force,
        } => {
            ws::new_worker(&name, &branch, force).map_err(|e| anyhow::anyhow!(e))?;
            // best-effort: TheWorld に worker actor を register
            if let Err(e) = register_worker_actor(&name) {
                eprintln!("  msgbox: register skipped ({e})");
            }
            Ok(())
        }
        WsCommands::Fork {
            name,
            branch,
            force,
        } => {
            ws::fork_worker(&name, &branch, force).map_err(|e| anyhow::anyhow!(e))?;
            if let Err(e) = register_worker_actor(&name) {
                eprintln!("  msgbox: register skipped ({e})");
            }
            Ok(())
        }
        WsCommands::Ls => ws::list_workers().map_err(|e| anyhow::anyhow!(e)),
        WsCommands::Path { name } => ws::worker_path(&name).map_err(|e| anyhow::anyhow!(e)),
        WsCommands::Rm { name, all, force } => {
            // 先に unregister（削除後だと parent SP 不明になる可能性）
            if let Some(ref worker_name) = name
                && let Err(e) = unregister_worker_actor(worker_name)
            {
                eprintln!("  msgbox: unregister skipped ({e})");
            }
            ws::remove_worker(name.as_deref(), all, force).map_err(|e| anyhow::anyhow!(e))
        }
        WsCommands::Status => ws::status_workers().map_err(|e| anyhow::anyhow!(e)),
        WsCommands::Cleanup { force } => ws::cleanup_workers(force).map_err(|e| anyhow::anyhow!(e)),
    }
}

/// Worker actor を TheWorld msgbox registry に登録
///
/// actor format: `worker-{name}` (例: `worker-VP-10`)
/// project_name: 親プロジェクトの repo root dir 名
/// port: 親プロジェクト SP の port (discovery::find_by_project_blocking で取得)
fn register_worker_actor(worker_name: &str) -> Result<()> {
    let (project_name, port) = resolve_parent_project()?;
    let actor = format!("worker-{worker_name}");
    let world_port = vantage_point::cli::WORLD_PORT;
    let url = format!("http://[::1]:{world_port}/api/world/msgbox/register");
    let body = serde_json::json!({
        "actor": actor,
        "project_name": project_name,
        "port": port,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;
    let resp = client.post(&url).json(&body).send()?;
    if resp.status().is_success() {
        eprintln!("  msgbox: registered {actor}@{project_name} (port {port})");
        Ok(())
    } else {
        Err(anyhow::anyhow!("register failed: {}", resp.status()))
    }
}

/// Worker actor を TheWorld msgbox registry から解除
fn unregister_worker_actor(worker_name: &str) -> Result<()> {
    let (project_name, _port) = resolve_parent_project()?;
    let actor = format!("worker-{worker_name}");
    let world_port = vantage_point::cli::WORLD_PORT;
    let url = format!("http://[::1]:{world_port}/api/world/msgbox/unregister");
    let body = serde_json::json!({
        "actor": actor,
        "project_name": project_name,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;
    let resp = client.post(&url).json(&body).send()?;
    if resp.status().is_success() {
        eprintln!("  msgbox: unregistered {actor}@{project_name}");
        Ok(())
    } else {
        Err(anyhow::anyhow!("unregister failed: {}", resp.status()))
    }
}

/// 現在の repo root から parent project 名と SP port を導出
fn resolve_parent_project() -> Result<(String, u16)> {
    let repo_root = vp_ccws::config::find_repo_root()
        .map_err(|e| anyhow::anyhow!("find_repo_root failed: {}", e))?;
    let project_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("project name not found"))?
        .to_string();
    let repo_root_str = repo_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("repo path contains invalid UTF-8"))?;
    let process = vantage_point::discovery::find_by_project_blocking(repo_root_str)
        .ok_or_else(|| anyhow::anyhow!("parent SP not running (TheWorld has no record)"))?;
    Ok((project_name, process.port))
}
