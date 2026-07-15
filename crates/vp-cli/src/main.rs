//! Vantage Point CLI — AI協働開発プラットフォーム
//!
//! Usage:
//!   vp            # 稼働中インスタンス一覧（vp ps）
//!   vp sp start   # SP サーバーを起動
//!   vp lane capture <lane>  # lane console を読む (tmux 非依存)
//!   vp mcp        # MCPサーバーとして起動（stdio）
//!   vp daemon     # TheWorld デーモン管理 (alias: vp world)
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # デバッグ表示モード
//!   VANTAGE_PROJECT_DIR=/path/to/project  # デフォルトプロジェクトディレクトリ
//!
//! Config file: ~/.config/vp/config.kdl

use anyhow::Result;
use clap::{Parser, Subcommand};

use vantage_point::cli::{self, parse_debug_env};
use vantage_point::commands;
use vantage_point::config::Config;
use vantage_point::mcp;

use commands::file::FileCommands;

// Phase 2.x-e: 旧 performer Lane crate を vp-cli の lib に統合。
// `vp` binary が `vp lane` サブコマンド経由で `vp_cli::lane` lib を使う。
#[cfg(feature = "midi")]
use commands::midi::MidiCommands;
use commands::pane::PaneCommands;
use vp_cli::lane;

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
    /// projects.kdl を現実と同期 — ghost project (dir 実在せず) を除去
    Sync,
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
    /// alias: `vp world` (旧名、 後方互換)。
    /// 旧 `vp conductor` alias は撤去 (conductor は lane 役割名に確定、語の衝突回避)。
    #[command(visible_alias = "world")]
    Daemon {
        /// 待ち受けポート番号（サブコマンド省略時に使用）
        #[arg(short, long, default_value_t = cli::world_port())]
        port: u16,
        /// サブコマンド（省略時は start として動作）
        #[command(subcommand)]
        command: Option<commands::daemon::DaemonCommands>,
    },

    /// SP サーバー管理（HTTP/QUIC サーバーのライフサイクル）
    #[command(subcommand)]
    Sp(commands::sp::SpCommands),

    /// wire accumulation messaging — `watch` (long-poll subscribe) / `send` / `watch-supervised` を提供。
    /// Claude Code Monitor の subscription source として使う想定 (wiremsg R5-2)。
    #[command(subcommand)]
    Wire(commands::wire::WireCommands),

    /// event log — agent の episodic memory（doc 27 §5-3）。
    ///
    /// `vp events [--since N]` で log 表示、`vp events emit --kind K` で push。
    /// build/test/lane lifecycle 等を agent の行動間に配り blind を解消する。
    Events(commands::events::EventsArgs),

    /// dev-flow primitives — Conductor × Performer × Memory orchestration の core 操作
    ///
    /// `vp flow handoff <name> --task-spec <file or ->` で performer 作成 + wire_send + nudge を atomic に。
    /// `vp flow progress` で並列 performer の git status + 未読 wire を 1 view で表示。
    /// MCP tool (`mcp__vantage-point__flow_handoff` / `flow_progress`) と同 semantics。
    #[command(subcommand)]
    Flow(commands::flow::FlowCommands),

    /// MIDIハードウェア操作
    #[cfg(feature = "midi")]
    #[command(subcommand)]
    Midi(MidiCommands),

    /// SurrealDB デーモン管理
    #[command(subcommand)]
    Db(commands::db::DbCommands),

    /// Stone Free 🧵 — performer Lane 管理（旧 vp ws、Phase 1 で統合）
    #[command(subcommand, alias = "ws", alias = "workspace")]
    Lane(LaneCommands),

    /// Port Layout — deterministic 透過的固定 port の計算・表示
    #[command(subcommand)]
    Port(commands::port::PortCommands),

    /// 登録 project 管理 — World daemon に直接 Unison RPC (add/remove/rename/enable/disable/reorder/list)
    #[command(subcommand)]
    Projects(commands::projects::ProjectsCommands),

    /// vp-app GUI 管理 (Mac 主軸切替: Rust + wry + xterm.js + creo-ui)
    #[command(subcommand)]
    App(commands::app::AppCommands),

    /// Creo ID 認証 — `vp auth me` / `vp auth login` / `vp auth logout` (= Phase A2 完成)
    #[command(subcommand)]
    Auth(commands::auth::AuthCommands),

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

/// Stone Free performer Lane コマンド（lane library への薄い wrapper）
#[derive(Subcommand)]
enum LaneCommands {
    /// 新しい performer 環境を作成（worktree add + symlink + setup）
    New {
        /// Performer 名
        name: String,
        /// 作成するブランチ名
        branch: String,
        /// 既存 performer を上書き
        #[arg(long, short)]
        force: bool,
        /// 隔離方式: worktree (default、 conductor の .git 共有) / clone (独立 .git、 escape hatch)
        #[arg(long, value_enum, default_value = "worktree")]
        isolation: lane::commands::Isolation,
        /// worktree の分岐元 ref（未 push の local branch も可）。省略時は
        /// performer-files.kdl の base-ref → origin/HEAD → main
        #[arg(long)]
        base: Option<String>,
        /// lane の claude model alias（例: 'opus' / 'sonnet' / 'haiku'）。次回 spawn 時に
        /// `--model` として読まれる。省略時は config の default-lane-model（既定 Opus）
        #[arg(long)]
        model: Option<String>,
    },
    /// 現在の dirty state を新しい performer 環境に fork
    Fork {
        /// Performer 名
        name: String,
        /// 作成するブランチ名
        branch: String,
        /// 既存 performer を上書き
        #[arg(long, short)]
        force: bool,
        /// 隔離方式: worktree (default) / clone (独立 .git、 escape hatch)
        #[arg(long, value_enum, default_value = "worktree")]
        isolation: lane::commands::Isolation,
        /// worktree の分岐元 ref（未 push の local branch も可）。省略時は
        /// performer-files.kdl の base-ref → origin/HEAD → main
        #[arg(long)]
        base: Option<String>,
        /// lane の claude model alias（例: 'opus' / 'sonnet' / 'haiku'）。次回 spawn 時に
        /// `--model` として読まれる。省略時は config の default-lane-model（既定 Opus）
        #[arg(long)]
        model: Option<String>,
    },
    /// performer 環境一覧
    ///
    /// default は `<name>\t<branch>\t<path>` の tab-separated 簡易出力 (= fs scan、 SP 不要)。
    /// `--detail` で SP `/api/lanes` を query して MCP `list_lanes` 同等の JSON (= state /
    /// stand / pid / cwd / performer_status / mailbox_addresses 付き) を出力する (= SP 稼働中のみ)。
    #[command(alias = "list")]
    Ls {
        /// SP `/api/lanes` から MCP list_lanes 同等の詳細 JSON を取得して出力
        #[arg(long)]
        detail: bool,
    },
    /// performer 環境のパスを表示
    Path {
        /// Performer 名
        name: String,
    },
    /// performer 環境を削除
    Rm {
        /// 削除する Performer 名（--all 指定時は不要）
        name: Option<String>,
        /// 全 performer を削除
        #[arg(long)]
        all: bool,
        /// 確認なしで強制削除
        #[arg(long, short)]
        force: bool,
    },
    /// 全 performer の状態表示
    Status,
    /// branch が default branch (origin/HEAD) に merge 済の performer を削除（squash merge も検出）
    Cleanup {
        /// 確認なしで強制削除
        #[arg(long, short)]
        force: bool,
    },
    /// 現 project の vp-app の active Lane を切り替える (= mcp__switch_lane の CLI pair、Unison-native)
    ///
    /// `name` は lane token: 'conductor' (lead) or performer 名 (例: 'feat-api')。現 project の
    /// local SP に `SwitchLane` を投げ、canvas channel 経由で vp-app がその lane を active 化する。
    /// 該当 project の SP が稼働している必要あり。unknown lane は vp-app 受信側で no-op。
    Switch {
        /// active 化する lane token ('conductor' or performer 名)
        name: String,
    },
    /// この lane の最後の CC session id を表示 (R3-b、 echoes spawn の --resume 用)
    ///
    /// project / lane は flag 優先、 無ければ VP_PROJECT / VP_LANE env から導出。
    /// 未記録 / env 不足なら何も出力せず exit 0 (caller は空文字で fallback 判定)。
    /// id の書き手は SessionStart hook (`vp wire hook-check`)。
    LastSession {
        /// project 名 (省略時 VP_PROJECT env)
        #[arg(long)]
        project: Option<String>,
        /// lane label: conductor / performer 名 (省略時 VP_LANE env)
        #[arg(long)]
        lane: Option<String>,
    },
    /// lane console の現在画面を読む (tmux decoupling: 旧 `vp tmux capture` の後継)
    ///
    /// SP の per-lane Term grid (TermAttach) を render して返す。tmux 不要。
    Capture {
        /// lane address ("<project>/conductor" / "<project>/performer/<name>")
        lane: String,
    },
    /// lane の claude / shell に text + Enter を注入 (旧 `vp tmux send-keys` / `vp directmsg` の後継)
    Nudge {
        /// lane address ("<project>/conductor" / "<project>/performer/<name>")
        lane: String,
        /// 注入するテキスト (Enter は自動付与、submit 意味論は SP 側 deliver_nudge)
        text: String,
    },
}

fn main() -> Result<()> {
    // rustls CryptoProvider を最初に初期化（reqwest/quinn が使う）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // CLIパース（tracingより先に）
    let cli = Cli::parse();

    // VP-192: 旧 config/data パスからの冪等なデータ移行 (config 読み込み前に 1 回)
    vantage_point::config::migrate_legacy_paths();

    // Load config
    let config = Config::load().unwrap_or_default();

    // 引数なし → vp ps（稼働中インスタンス一覧）
    let command = cli.command.unwrap_or(Commands::Ps);

    // daemon server 本体 (`vp daemon start` / `vp world` / subcommand 省略) は stdout/stderr が
    // spawn 経路依存で闇に落ちる (daemonize・restart-all の Stdio::null・launchd redirect の混在)
    // ため、VP_DAEMON_LOG_FILE 未設定なら vp_log_dir()/daemon.kdl.log を default にして
    // init_tracing の file appender 分岐 (rotate 付き) へ固定する。spawn 経路非依存の log SSOT。
    // Stop/Status 等の短命 subcommand は対象外 (従来どおり stderr)。
    let is_daemon_server = matches!(
        &command,
        Commands::Daemon { command: None, .. }
            | Commands::Daemon {
                command: Some(commands::daemon::DaemonCommands::Start { .. }),
                ..
            }
    );
    if is_daemon_server && std::env::var("VP_DAEMON_LOG_FILE").map_or(true, |v| v.trim().is_empty())
    {
        let default_log = vantage_point::config::vp_log_dir().join("daemon.kdl.log");
        // main 冒頭・tokio runtime 起動前の single-thread 区間なので set_var は安全
        unsafe { std::env::set_var("VP_DAEMON_LOG_FILE", &default_log) };
    }

    // Initialize tracing
    let debug_mode_for_tracing = parse_debug_env().unwrap_or_default();
    cli::init_tracing(debug_mode_for_tracing, false);

    match command {
        Commands::RestartAll => commands::restart_all::execute(),
        Commands::Ps => cli::list_instances(&config),
        Commands::Sync => commands::sync::execute(),
        Commands::Config => commands::config::execute(&config),
        Commands::Mcp => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(None))
        }
        Commands::Update { check } => commands::update::execute(check),
        Commands::Pane(cmd) => commands::pane::execute(cmd, &config),
        Commands::File(cmd) => commands::file::execute(cmd, &config),

        Commands::Daemon { port, command } => {
            // subcommand 省略時 (`vp daemon --port N`) は Start にフォールバック。
            // PR-α-4 (VP-114) で `--midi` flag が追加されたが、 後方互換 path のため None で省略。
            let cmd = command.unwrap_or({
                #[cfg(feature = "midi")]
                {
                    commands::daemon::DaemonCommands::Start { port, midi: None }
                }
                #[cfg(not(feature = "midi"))]
                {
                    commands::daemon::DaemonCommands::Start { port }
                }
            });
            commands::daemon::execute(cmd)
        }
        Commands::Sp(cmd) => commands::sp::execute(cmd, &config),
        // tmux decoupling PR2: `vp hd` / `vp tmux` は退役。 lane の console 操作は
        // `vp lane capture` / `vp lane nudge` (lane 語彙の後継)。
        #[cfg(feature = "midi")]
        Commands::Midi(cmd) => commands::midi::execute(cmd),
        Commands::Db(cmd) => commands::db::execute(cmd),

        Commands::Lane(cmd) => execute_lane(cmd),
        Commands::Port(cmd) => commands::port::execute(cmd),
        Commands::Projects(cmd) => {
            // projects 操作は World daemon に直接 Unison RPC (async)。 auth/wire/flow と同じ
            // per-command Runtime で block_on する。
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::projects::execute(cmd))
        }
        Commands::App(cmd) => commands::app::execute(cmd),
        Commands::Auth(cmd) => {
            // Wire / Flow と同じ pattern — async handler を per-command Runtime で block_on
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::auth::execute(cmd))
        }
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
        Commands::Wire(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::wire::run(cmd))
        }
        Commands::Events(args) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::events::run(args))
        }
        Commands::Flow(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::flow::run(cmd))
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

/// Stone Free 🧵 performer Lane 操作を lane library に委譲
///
/// wiremsg R5-4: 旧 msgbox の registry サブシステム (performer 作成/削除時の
/// `performer-{name}@{project}` actor register/unregister) は撤去済。
fn execute_lane(cmd: LaneCommands) -> Result<()> {
    use lane::commands as ws;

    match cmd {
        LaneCommands::New {
            name,
            branch,
            force,
            isolation,
            base,
            model,
        } => {
            ws::new_performer(
                &name,
                &branch,
                force,
                isolation,
                base.as_deref(),
                model.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            Ok(())
        }
        LaneCommands::Fork {
            name,
            branch,
            force,
            isolation,
            base,
            model,
        } => {
            ws::fork_performer(
                &name,
                &branch,
                force,
                isolation,
                base.as_deref(),
                model.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            Ok(())
        }
        LaneCommands::Ls { detail } => {
            if detail {
                list_performers_detail()
            } else {
                ws::list_performers().map_err(|e| anyhow::anyhow!(e))
            }
        }
        LaneCommands::Path { name } => ws::performer_path(&name).map_err(|e| anyhow::anyhow!(e)),
        LaneCommands::Rm { name, all, force } => {
            // VP-124: SP-aware delete を試みる (orchestration: PTY kill + tmux kill +
            // lane workspace rm + SystemEvent broadcast を 1 HTTP call で完結)。
            // --all は filesystem-only fallback (一括削除は SP 経由する意味なし、 個別 Lane
            // address が必要なため)。 SP 不在 / failure なら現挙動 (ws::remove_performer fs-only)
            // に fallback して compat 維持。
            if let Some(ref performer_name) = name
                && !all
                && try_sp_delete_performer(performer_name)
            {
                return Ok(());
            }
            ws::remove_performer(name.as_deref(), all, force).map_err(|e| anyhow::anyhow!(e))
        }
        LaneCommands::Status => ws::status_performers().map_err(|e| anyhow::anyhow!(e)),
        LaneCommands::Cleanup { force } => {
            ws::cleanup_performers(force).map_err(|e| anyhow::anyhow!(e))
        }
        LaneCommands::Switch { name } => switch_lane_via_quic(&name),
        LaneCommands::LastSession { project, lane } => {
            // R3-b: echoes task (spawn 時) から env 経由で呼ばれる主経路。
            // 未記録 / env 不足は「出力なし exit 0」 — caller の `[ -n "$RESUME_ID" ]`
            // 判定で従来 (--continue) に fallback させる。
            let project = project.or_else(|| std::env::var("VP_PROJECT").ok());
            let lane = lane.or_else(|| std::env::var("VP_LANE").ok());
            if let (Some(p), Some(l)) = (project, lane)
                && let Some(id) = vantage_point::lane::cc_session::last(&p, &l)
            {
                println!("{id}");
            }
            Ok(())
        }
        LaneCommands::Capture { lane } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::capture(&lane, &config)
        }
        LaneCommands::Nudge { lane, text } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::nudge(&lane, &text, &config)
        }
    }
}

/// `vp lane ls --detail` 実装: World process-proxy ask `lanes_list` を query して pretty JSON で出力。
///
/// lanes portless (doc 27 §3.4.5): 旧 SP `/api/lanes` 直叩きを撤去し World :32000 の process-proxy に
/// 一本化 (`try_sp_delete_performer` と同型、 SP port 解決不要)。 SP 不在 (= TheWorld に未登録 /
/// cwd が repo 外 / SP 未起動) なら World が control channel 逆引き失敗で error を返す。 `--detail` を
/// 要求した時点で SP 稼働を前提とする (= fs-only fallback はせず、 明示的に user に SP 未起動を伝える)。
///
/// MCP `list_lanes` の mailbox_addresses 計算 / project_addresses synthesis までは実装せず、
/// dispatch `lanes_list` の生 JSON (`{lanes:[...]}`) を pretty print する (mailbox は SKILL.md doc 案内)。
fn list_performers_detail() -> Result<()> {
    // repo_root = project_path (World handshake の stable identifier)。 SP port は process-proxy で不要。
    let repo_root = lane::config::find_repo_root()
        .map_err(|e| anyhow::anyhow!("repo root 解決失敗 (--detail は project 内が前提): {}", e))?;
    let project_path = repo_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("repo path に invalid UTF-8"))?;

    let resp = vantage_point::commands::process_client::world_process_request_blocking(
        cli::world_port(),
        project_path,
        "lanes_list",
        serde_json::json!({}),
    )
    .map_err(|e| anyhow::anyhow!("lanes_list 失敗 (--detail は SP 稼働が前提): {}", e))?;

    println!(
        "{}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    Ok(())
}

/// active Lane 切り替え CLI 実装 (= mcp__switch_lane の CLI pair)。
///
/// L0 portless: 現 project の SP に `SwitchLane` ProcessMessage を World :32000 の process-proxy
/// ask で forward する（SP は listen しないので旧来の SP 直結 QUIC は撤去）。World が project_path
/// を path_key に正規化して当該 SP の control channel を逆引きし、`dispatch_process_method`
/// （"switch_lane" → `handle_process_message`）へ forward → hub.broadcast → topic
/// `process/paisley-park/event/switch-lane`（非 retained）→ canvas channel 経由で vp-app が受信し、
/// その lane を active 化する（lane-within-project の per-project 切替）。
/// MCP 側も `process_call("switch_lane", …)`（mcp.rs、process-proxy 経由）で同 dispatch に着地。
fn switch_lane_via_quic(name: &str) -> Result<()> {
    // lane token = "conductor" (lead) or performer 名。server / vp-app 側で実在 lane と照合
    // （unknown lane は vp-app 受信側で no-op）。
    let trimmed = name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("lane token is required (空文字不可)");
    }

    // repo_root = project_path (World process-proxy handshake の stable identifier)。
    // L0 portless: SP port 解決は不要（World が path_key 逆引きで forward する）。
    let repo_root = lane::config::find_repo_root()
        .map_err(|e| anyhow::anyhow!("find_repo_root failed: {}", e))?;
    let (Some(project_name), Some(project_path)) = (
        repo_root.file_name().and_then(|n| n.to_str()),
        repo_root.to_str(),
    ) else {
        anyhow::bail!("repo path contains invalid UTF-8");
    };

    // SwitchLane を World process-proxy ask で SP へ forward（payload = ProcessMessage JSON、
    // `{"type":"switch_lane","lane":...}`）。SP 側 dispatch_process_method が受けて broadcast。
    let msg = vantage_point::protocol::ProcessMessage::SwitchLane {
        lane: trimmed.to_string(),
    };
    let payload = serde_json::to_value(&msg)?;
    vantage_point::commands::process_client::world_process_request_blocking(
        cli::world_port(),
        project_path,
        "switch_lane",
        payload,
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "SP {} への switch_lane 送信失敗 (World process-proxy): {}",
            project_name,
            e
        )
    })?;

    println!(
        "switched active lane to '{}' (project={}, via World process-proxy)",
        trimmed, project_name
    );
    Ok(())
}

/// VP-124 Phase 1: SP-aware Performer Lane delete を試みる helper。
///
/// `vp lane rm <name>` (= 個別削除) で呼ばれ、 parent SP が稼働中なら World process-proxy ask
/// (`lane_delete`) 経由で `delete_lane_orchestrated` を発火 (= PTY kill + tmux kill + lane rm +
/// SystemEvent broadcast を SP 側で atomically 実行)。 SP 不在 / failure なら false 返して
/// filesystem-only fallback (= 現挙動の `ws::remove_performer`) に委譲。
///
/// F6② (doc 27 §3.4.5/§6): 旧 SP 直結 (`DELETE /api/lanes` reqwest) を撤去し World :32000 の
/// process-proxy に一本化 (SP port 解決不要、 L1 portless 前進)。 best-effort: 全 failure
/// (SP 不在 / lane not found / network) は warn print して false → fs-only に委譲。
fn try_sp_delete_performer(performer_name: &str) -> bool {
    // repo_root = project_path (World handshake の stable identifier)。 SP port は process-proxy で不要。
    let repo_root = match lane::config::find_repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  SP delete skipped (repo root resolve failed: {e})");
            return false;
        }
    };
    let (Some(project_name), Some(project_path)) = (
        repo_root.file_name().and_then(|n| n.to_str()),
        repo_root.to_str(),
    ) else {
        eprintln!("  SP delete skipped (repo path に invalid UTF-8)");
        return false;
    };

    // address 構築: `<project>/performer/<name>` (SP 側 parse_address が逆変換)。
    let address = format!("{project_name}/performer/{performer_name}");
    let payload = serde_json::json!({ "address": address, "cleanup": true });

    match vantage_point::commands::process_client::world_process_request_blocking(
        cli::world_port(),
        project_path,
        "lane_delete",
        payload,
    ) {
        Ok(resp) => {
            // 成功 body は DeletedLaneInfo JSON、 user 向けに要点だけ要約。
            let pid = resp.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            let cleanup = resp
                .get("cleanup")
                .and_then(|c| c.as_str())
                .unwrap_or("(skipped)");
            eprintln!("削除: {address} (SP orchestrated: pid={pid} cleanup={cleanup})");
            true
        }
        Err(e) => {
            // SP 不在 / lane not found / network 等は全て fs-only fallback (旧 non-2xx 挙動踏襲)。
            eprintln!("  SP delete failed ({e}), falling back to fs-only");
            false
        }
    }
}
