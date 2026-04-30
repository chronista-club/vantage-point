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

use commands::file::FileCommands;

// Phase 2.x-e: vp-ccws crate を vp-cli の lib に統合。
// `ccws` 標準 bin (src/bin/ccws.rs) と main.rs (vp) の両方が `vp_cli::ccws` を共有。
#[cfg(feature = "midi")]
use commands::midi::MidiCommands;
use commands::pane::PaneCommands;
use commands::tmux::TmuxCommands;
use vp_cli::ccws;

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
    ///
    /// alias: `vp world` (旧名、 後方互換) / `vp conductor` (古い metaphor)
    #[command(visible_alias = "world", alias = "conductor")]
    Daemon {
        /// 待ち受けポート番号（サブコマンド省略時に使用）
        #[arg(short, long, default_value_t = cli::WORLD_PORT)]
        port: u16,
        /// サブコマンド（省略時は start として動作）
        #[command(subcommand)]
        command: Option<commands::daemon::DaemonCommands>,
    },

    /// SP サーバー管理（HTTP/QUIC サーバーのライフサイクル）
    #[command(subcommand)]
    Sp(commands::sp::SpCommands),

    /// HD インスタンス管理（tmux + Claude CLI + ccwire）
    #[command(subcommand)]
    Hd(commands::hd::HdCommands),

    /// Mailbox actor messaging — Phase 1a で `watch` (long-poll subscribe) と `send` を提供。
    /// Claude Code Monitor の subscription source として使う想定 (vp_mailbox_monitor_agent_inbox.md)。
    #[command(subcommand)]
    Mailbox(commands::mailbox::MailboxCommands),

    /// tmux ペイン操作（キャプチャ・分割・送信・ダッシュボード）
    #[command(subcommand)]
    Tmux(TmuxCommands),

    /// MIDIハードウェア操作
    #[cfg(feature = "midi")]
    #[command(subcommand)]
    Midi(MidiCommands),

    /// SurrealDB デーモン管理
    #[command(subcommand)]
    Db(commands::db::DbCommands),

    /// Stone Free 🧵 — worker workspace 管理（旧 ccws、Phase 1 で統合）
    #[command(subcommand, alias = "workspace")]
    Ws(WsCommands),

    /// Port Layout — deterministic 透過的固定 port の計算・表示
    #[command(subcommand)]
    Port(commands::port::PortCommands),

    /// vp-app GUI 管理 (Mac 主軸切替: Rust + wry + xterm.js + creo-ui)
    #[command(subcommand)]
    App(commands::app::AppCommands),

    /// Window screenshot — vp-app window を PNG 保存 (canonical screenshot 機構)
    #[command(alias = "screenshot")]
    Shot {
        /// 出力 path (default: /tmp/vp/shot-latest.png)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        /// owner process name (default: vp-app)
        #[arg(short, long, default_value = "vp-app")]
        window: String,
        /// 候補 window の n 番目 (0-based、 default: 0 = frontmost)
        #[arg(short, long)]
        index: Option<usize>,
        /// title 部分一致でさらに絞り込む
        #[arg(short, long)]
        title: Option<String>,
        /// list mode: capture せず候補一覧を表示
        #[arg(long)]
        list: bool,
        /// 矩形 capture: "x,y,w,h" (screen 座標、 logical px)
        #[arg(long)]
        rect: Option<String>,
        /// 名付き region: sidebar / main / full (window 内 sub-region に解決)
        #[arg(long)]
        region: Option<String>,
        /// 時系列 capture mode: --interval + (--count or --duration) を一緒に指定
        #[arg(long)]
        series: bool,
        /// frame 間隔 (ex: "200ms" / "0.5s" / "1s")、 series mode 必須
        #[arg(long, default_value = "200ms")]
        interval: String,
        /// frame 数 (count or duration のどちらか必須)
        #[arg(long)]
        count: Option<u32>,
        /// 撮影時間 (ex: "5s" / "10s")、 count と排他
        #[arg(long)]
        duration: Option<String>,
        /// series 出力 dir (default: /tmp/vp/series-{unix-ts}/)
        #[arg(long)]
        output_dir: Option<std::path::PathBuf>,
        /// layout で frame を 1 枚に compose: "MxN" / "vertical" (= "v") / "horizontal" (= "h")
        /// 出力 path は --output で指定 (未指定時 <output_dir>/composed.png)
        #[arg(long)]
        layout: Option<String>,
    },
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
    /// Worker actor を TheWorld msgbox registry に (再) 登録
    ///
    /// Worker 作成時 (`vp ws new`) に一度 register されるが、TheWorld 再起動で
    /// registry が memory-reset するため再登録が必要。本コマンドで手動再登録 or
    /// 起動スクリプトから呼び出して整合性を維持する。
    Register {
        /// Worker 名 (親 project は cwd から推測)
        name: String,
    },
    /// ccws ls の全 Worker を registry に一括再登録 (sync)
    Resync,
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
        Commands::File(cmd) => commands::file::execute(cmd, &config),

        Commands::Daemon { port, command } => {
            let cmd = command.unwrap_or(commands::daemon::DaemonCommands::Start { port });
            commands::daemon::execute(cmd)
        }
        Commands::Sp(cmd) => commands::sp::execute(cmd, &config),
        Commands::Hd(cmd) => commands::hd::execute(cmd, &config),

        Commands::Tmux(cmd) => commands::tmux::execute(cmd, &config),
        #[cfg(feature = "midi")]
        Commands::Midi(cmd) => commands::midi::execute(cmd),
        Commands::Db(cmd) => commands::db::execute(cmd),

        Commands::Ws(cmd) => execute_ws(cmd),
        Commands::Port(cmd) => commands::port::execute(cmd),
        Commands::App(cmd) => commands::app::execute(cmd),
        Commands::Shot {
            output,
            window,
            index,
            title,
            list,
            rect,
            region,
            series,
            interval,
            count,
            duration,
            output_dir,
            layout,
        } => execute_shot(
            output, window, index, title, list, rect, region, series, interval, count, duration,
            output_dir, layout,
        ),
        Commands::Mailbox(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::mailbox::run(cmd))
        }
    }
}

/// `vp shot` ── canonical screenshot 機構の薄い wrapper。
/// 実装本体は `vantage_point::screenshot` module (trait + 各 OS backend)。
/// stdout に保存先 path 1 行を吐く (caller が grep / read しやすく)。
/// 簡易 duration parser ("200ms" / "0.5s" / "1s" など)
fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        let n: u64 = num
            .trim()
            .parse()
            .map_err(|e| format!("bad ms value '{}': {}", num, e))?;
        Ok(std::time::Duration::from_millis(n))
    } else if let Some(num) = s.strip_suffix('s') {
        let n: f64 = num
            .trim()
            .parse()
            .map_err(|e| format!("bad s value '{}': {}", num, e))?;
        Ok(std::time::Duration::from_secs_f64(n))
    } else {
        Err(format!(
            "invalid duration '{}': expected 'Nms' / 'Ns' / 'N.Ns'",
            s
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_shot(
    output: Option<std::path::PathBuf>,
    window: String,
    index: Option<usize>,
    title: Option<String>,
    list: bool,
    rect: Option<String>,
    region: Option<String>,
    series: bool,
    interval: String,
    count: Option<u32>,
    duration: Option<String>,
    output_dir: Option<std::path::PathBuf>,
    layout: Option<String>,
) -> Result<()> {
    use vantage_point::screenshot::{
        CaptureFilter, Rect,
        compose::{Layout, compose},
        default_backend, region_for_name,
    };
    let backend = default_backend();
    let filter = CaptureFilter {
        owner: window,
        index,
        title_match: title,
    };

    // ── Phase 5-C v2: series mode ───────────────────────────────────────────
    // 時系列 capture: 1 回 list_windows + Rect resolve、 以降 loop は capture_rect 直叩き。
    // swift JIT は最初の 1 回だけ → frame 間 ~50ms / 20fps 上限の高速 capture が可能。
    if series {
        let interval_dur = parse_duration(&interval).map_err(|e| anyhow::anyhow!(e))?;
        let frame_count: u32 = match (count, duration.as_deref()) {
            (Some(c), None) => c,
            (None, Some(d)) => {
                let total = parse_duration(d).map_err(|e| anyhow::anyhow!(e))?;
                let n = (total.as_millis() / interval_dur.as_millis().max(1)) as u32;
                n.max(1)
            }
            (Some(_), Some(_)) => {
                anyhow::bail!("--series: --count と --duration は排他 (どちらか 1 つ指定)")
            }
            (None, None) => anyhow::bail!("--series は --count または --duration が必要"),
        };

        // Rect resolve (rect / region / 全 window 全部 Rect 化で統一)
        let target_rect: Rect = if let Some(rs) = rect {
            Rect::parse(&rs).map_err(|e| anyhow::anyhow!(e))?
        } else {
            let windows = backend
                .list_windows(&filter)
                .map_err(|e| anyhow::anyhow!(e))?;
            if windows.is_empty() {
                anyhow::bail!("no window with owner = {:?}", filter.owner);
            }
            let target = vantage_point::screenshot::pick_window(&windows, &filter)
                .map_err(|e| anyhow::anyhow!(e))?;
            if let Some(reg) = region {
                region_for_name(&reg, &target).ok_or_else(|| {
                    anyhow::anyhow!("unknown region {:?} (known: sidebar / main / full)", reg)
                })?
            } else {
                Rect {
                    x: target.x,
                    y: target.y,
                    w: target.width,
                    h: target.height,
                }
            }
        };

        // 出力 dir resolve
        let dir = output_dir.unwrap_or_else(|| {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            std::path::PathBuf::from(format!("/tmp/vp/series-{}", ts))
        });
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("mkdir {}: {}", dir.display(), e))?;

        // capture loop
        let series_started = std::time::Instant::now();
        for i in 0..frame_count {
            let frame_path = dir.join(format!("frame-{:04}.png", i));
            backend
                .capture_rect(target_rect, Some(frame_path))
                .map_err(|e| anyhow::anyhow!(e))?;
            if i + 1 < frame_count {
                std::thread::sleep(interval_dur);
            }
        }

        let total_ms = series_started.elapsed().as_millis();
        eprintln!(
            "(series: {} frames @ {:?} interval, capture total {}ms — rect {}x{} at {},{})",
            frame_count,
            interval_dur,
            total_ms,
            target_rect.w,
            target_rect.h,
            target_rect.x,
            target_rect.y
        );

        // ── layout 指定時: 全 frame を 1 枚に compose ─────────────────────
        if let Some(layout_str) = layout {
            let layout_spec = Layout::parse(&layout_str).map_err(|e| anyhow::anyhow!(e))?;
            let frame_paths: Vec<std::path::PathBuf> = (0..frame_count)
                .map(|i| dir.join(format!("frame-{:04}.png", i)))
                .collect();
            let compose_output = output.unwrap_or_else(|| dir.join("composed.png"));
            let compose_started = std::time::Instant::now();
            let (cw, ch) = compose(&frame_paths, layout_spec, &compose_output)
                .map_err(|e| anyhow::anyhow!(e))?;
            eprintln!(
                "(composed {}x{} from {} frames in {}ms — layout {})",
                cw,
                ch,
                frame_count,
                compose_started.elapsed().as_millis(),
                layout_str
            );
            // stdout: composed image path (caller が parse しやすく、 frame dir は eprintln で報告)
            println!("{}", compose_output.display());
            eprintln!("(frames remain at {})", dir.display());
        } else {
            // layout 無し: dir path を stdout に
            println!("{}", dir.display());
        }
        return Ok(());
    }

    if list {
        let windows = backend
            .list_windows(&filter)
            .map_err(|e| anyhow::anyhow!(e))?;
        if windows.is_empty() {
            eprintln!(
                "(no window with owner = {:?}; is the app running?)",
                filter.owner
            );
            return Ok(());
        }
        println!("ID       OWNER       POSITION      SIZE         TITLE");
        for w in windows {
            println!(
                "{:<8} {:<11} {:>5},{:<5}    {:>4}x{:<4}   {}",
                w.id, w.owner, w.x, w.y, w.width, w.height, w.title
            );
        }
        return Ok(());
    }

    // Phase 5-C v2: --rect / --region で sub-region capture
    if let Some(rect_str) = rect {
        let r = Rect::parse(&rect_str).map_err(|e| anyhow::anyhow!(e))?;
        let result = backend
            .capture_rect(r, output)
            .map_err(|e| anyhow::anyhow!(e))?;
        println!("{}", result.path.display());
        eprintln!(
            "(rect captured {}x{} in {}ms — at {},{})",
            result.width, result.height, result.elapsed_ms, r.x, r.y
        );
        return Ok(());
    }
    if let Some(region_name) = region {
        // 名付き region は window 解決が必要 → list して候補から target を選ぶ → region_for_name で rect 計算
        let windows = backend
            .list_windows(&filter)
            .map_err(|e| anyhow::anyhow!(e))?;
        if windows.is_empty() {
            anyhow::bail!("no window with owner = {:?}", filter.owner);
        }
        let target = vantage_point::screenshot::pick_window(&windows, &filter)
            .map_err(|e| anyhow::anyhow!(e))?;
        let r = region_for_name(&region_name, &target).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown region {:?} (known: sidebar / main / full)",
                region_name
            )
        })?;
        let result = backend
            .capture_rect(r, output)
            .map_err(|e| anyhow::anyhow!(e))?;
        println!("{}", result.path.display());
        eprintln!(
            "(region '{}' captured {}x{} in {}ms — at {},{})",
            region_name, result.width, result.height, result.elapsed_ms, r.x, r.y
        );
        return Ok(());
    }

    // 通常: window 全体 capture
    let result = backend
        .capture(&filter, output)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("{}", result.path.display());
    eprintln!(
        "(captured {}x{} in {}ms — id={} title={:?})",
        result.width, result.height, result.elapsed_ms, result.window.id, result.window.title
    );
    Ok(())
}

/// Stone Free 🧵 worker workspace 操作を vp-ccws library に委譲
///
/// Phase 2 追加: worker 作成/削除時に TheWorld の msgbox registry に
/// `worker-{name}@{project}` actor を register/unregister（best-effort、
/// TheWorld 未起動でも workspace 操作自体は成功させる）。
fn execute_ws(cmd: WsCommands) -> Result<()> {
    use ccws::commands as ws;

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
        WsCommands::Register { name } => register_worker_actor(&name),
        WsCommands::Resync => resync_all_workers(),
    }
}

/// ccws ls の全 Worker を registry に一括再登録。
/// parent project は **config から最長一致** で解決 (cwd 不要)。
fn resync_all_workers() -> Result<()> {
    use std::fs;
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME env 未設定"))?;
    let workers_dir = std::path::PathBuf::from(home).join(".local/share/ccws");
    if !workers_dir.exists() {
        println!("No ccws workers found at {}", workers_dir.display());
        return Ok(());
    }
    let config = vantage_point::config::Config::load()
        .map_err(|e| anyhow::anyhow!("config load failed: {e}"))?;

    let entries = fs::read_dir(&workers_dir)?;
    let mut ok = 0;
    let mut fail = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dirname = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        match register_worker_from_dirname(&config, &dirname) {
            Ok(()) => ok += 1,
            Err(e) => {
                eprintln!("  skip {dirname}: {e}");
                fail += 1;
            }
        }
    }
    eprintln!("resync: {ok} ok, {fail} skipped");
    Ok(())
}

/// dirname (`{project}-{worker}` format) から parent project を config の最長一致で
/// 決定、SP port を discovery で確認、register API call。
fn register_worker_from_dirname(
    config: &vantage_point::config::Config,
    dirname: &str,
) -> Result<()> {
    // config.projects の name で最長 prefix match の project を探す
    let parent = config
        .projects
        .iter()
        .filter(|p| dirname.starts_with(&format!("{}-", p.name)))
        .max_by_key(|p| p.name.len())
        .ok_or_else(|| anyhow::anyhow!("parent project not found in config"))?;
    let worker_name = dirname
        .strip_prefix(&format!("{}-", parent.name))
        .ok_or_else(|| anyhow::anyhow!("worker name extract failed"))?;
    let process = vantage_point::discovery::find_by_project_blocking(&parent.path)
        .ok_or_else(|| anyhow::anyhow!("parent SP not running"))?;
    register_worker_actor_with_context(&parent.name, process.port, worker_name)
}

/// Worker actor を TheWorld msgbox registry に登録 (cwd ベース — ws new 等の内部用)
///
/// actor format: `worker-{name}` (例: `worker-VP-10`)
fn register_worker_actor(worker_name: &str) -> Result<()> {
    let (project_name, port) = resolve_parent_project()?;
    register_worker_actor_with_context(&project_name, port, worker_name)
}

/// Worker actor を指定 parent project / port で register (core 実装)
fn register_worker_actor_with_context(
    project_name: &str,
    port: u16,
    worker_name: &str,
) -> Result<()> {
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
    let repo_root = ccws::config::find_repo_root()
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
