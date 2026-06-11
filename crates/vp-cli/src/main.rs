//! Vantage Point CLI — AI協働開発プラットフォーム
//!
//! Usage:
//!   vp            # 稼働中インスタンス一覧（vp ps）
//!   vp sp start   # SP サーバーを起動
//!   vp hd start   # HD (Claude CLI) を起動
//!   vp hd attach  # HD に TUI 接続
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
use commands::tmux::TmuxCommands;
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
        #[arg(short, long, default_value_t = cli::WORLD_PORT)]
        port: u16,
        /// サブコマンド（省略時は start として動作）
        #[command(subcommand)]
        command: Option<commands::daemon::DaemonCommands>,
    },

    /// SP サーバー管理（HTTP/QUIC サーバーのライフサイクル）
    #[command(subcommand)]
    Sp(commands::sp::SpCommands),

    /// HD インスタンス管理（tmux + Claude CLI）
    #[command(subcommand)]
    Hd(commands::hd::HdCommands),

    /// wire accumulation messaging — `watch` (long-poll subscribe) / `send` / `watch-supervised` を提供。
    /// Claude Code Monitor の subscription source として使う想定 (wiremsg R5-2)。
    #[command(subcommand)]
    Wire(commands::wire::WireCommands),

    /// dev-flow primitives — Conductor × Performer × Memory orchestration の core 操作
    ///
    /// `vp flow handoff <name> --task-spec <file or ->` で performer 作成 + wire_send + nudge を atomic に。
    /// `vp flow progress` で並列 performer の git status + 未読 wire を 1 view で表示。
    /// MCP tool (`mcp__vantage-point__flow_handoff` / `flow_progress`) と同 semantics。
    #[command(subcommand)]
    Flow(commands::flow::FlowCommands),

    /// directmsg — tmux send-keys ベースの直接メッセージ（緊急 / ephemeral 用、wiremsg の補助）
    ///
    /// 宛先 lane の tmux session に直接テキストを send-keys する。SP / DB 非依存。
    Directmsg {
        /// 宛先 lane address（"<project>/conductor" または "<project>/performer/<name>"）
        lane: String,
        /// 送信テキスト
        text: String,
        /// 末尾に Enter を付けない
        #[arg(long)]
        no_enter: bool,
    },

    /// LAN address book — mDNS で 同 LAN 上の VP world を discover + 永続化 (VP-148 PR-P3-2)
    ///
    /// `vp lan discover` で 列挙、 `vp lan add <alias>` で `~/.config/vp/addresses.toml` に追加、
    /// `vp lan list` / `vp lan remove <alias>` で管理。 後続 PR-P3-3 で cross-machine msg
    /// dispatch の宛先 lookup source として利用。
    #[command(subcommand)]
    Lan(commands::lan::LanCommands),

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
    /// branch が main に merge 済の performer を削除
    Cleanup {
        /// 確認なしで強制削除
        #[arg(long, short)]
        force: bool,
    },
    /// Canvas の表示 lane を切り替える (= mcp__switch_lane の CLI pair、 TheWorld 経由)
    ///
    /// `lane` は project 名 (lane bar に表示される識別子、 例: 'vantage-point', 'creo-memories')。
    /// TheWorld :32000 が稼働している必要あり。 Canvas WebView がいない / 全 disconnect の場合は
    /// `clients: 0` だが exit 0 (= server 側で no-op、 status ok)。
    Switch {
        /// 切り替え先 lane 名 (= project 名)
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
        Commands::Hd(cmd) => commands::hd::execute(cmd, &config),

        Commands::Tmux(cmd) => commands::tmux::execute(cmd, &config),
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
        Commands::Flow(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::flow::run(cmd))
        }
        Commands::Directmsg {
            lane,
            text,
            no_enter,
        } => commands::directmsg::run(&lane, &text, !no_enter),
        Commands::Lan(cmd) => commands::lan::handle_lan_command(cmd),
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
        } => {
            ws::new_performer(&name, &branch, force, isolation).map_err(|e| anyhow::anyhow!(e))?;
            Ok(())
        }
        LaneCommands::Fork {
            name,
            branch,
            force,
            isolation,
        } => {
            ws::fork_performer(&name, &branch, force, isolation).map_err(|e| anyhow::anyhow!(e))?;
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
        LaneCommands::Switch { name } => switch_lane_via_world(&name),
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
    }
}

/// `vp lane ls --detail` 実装: 親 SP の `/api/lanes` を query して pretty JSON で出力。
///
/// SP 不在 (= TheWorld に未登録 / cwd が repo 外) なら error。 `--detail` を要求した時点で
/// SP 稼働を前提とする (= fs-only fallback はせず、 明示的に user に SP 未起動を伝える)。
///
/// MCP `list_lanes` の mailbox_addresses 計算 / project_addresses synthesis までは
/// 実装せず、 SP `/api/lanes` の生 JSON を pretty print する (= SP が持つ live state を
/// 直に出す、 mailbox は SKILL.md doc で計算式を案内する方針)。
fn list_performers_detail() -> Result<()> {
    let (_project_name, port) = resolve_parent_project()
        .map_err(|e| anyhow::anyhow!("SP 解決失敗 (--detail は SP 稼働が前提): {}", e))?;
    let url = format!("http://[::1]:{}/api/lanes", port);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client build failed: {}", e))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| anyhow::anyhow!("SP :{} に到達できません: {}", port, e))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("SP /api/lanes error: {} {}", status, text);
    }
    // SP の raw JSON を pretty print。 mailbox_addresses 計算は省略 (上記 doc 参照)。
    let parsed: serde_json::Value =
        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    println!(
        "{}",
        serde_json::to_string_pretty(&parsed).unwrap_or_default()
    );
    Ok(())
}

/// Canvas lane 切り替え CLI 実装 (= mcp__switch_lane と等価)。
///
/// TheWorld の `/api/canvas/switch_lane` を POST、 接続中の Canvas WS 全 client に
/// `{"type":"switch_lane","lane":...}` をブロードキャストする (mcp.rs:1042 と同じ path)。
fn switch_lane_via_world(name: &str) -> Result<()> {
    // 軽量 validate (= validate_performer_name と同 character class、 但し空チェックのみ強制)。
    // lane 名は project name (conductor lane) or performer name (= `<project>/performer/<name>`) を想定、
    // server 側で実在 lane と照合される (= unknown lane は WS 受信側で no-op)。
    let trimmed = name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("lane name is required (空文字不可)");
    }

    let url = format!(
        "http://[::1]:{}/api/canvas/switch_lane",
        vantage_point::cli::WORLD_PORT
    );
    let body = serde_json::json!({ "lane": trimmed });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client build failed: {}", e))?;

    let resp = client.post(&url).json(&body).send().map_err(|e| {
        anyhow::anyhow!(
            "TheWorld :{} に到達できません: {}",
            vantage_point::cli::WORLD_PORT,
            e
        )
    })?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("TheWorld API error: {} {}", status, text);
    }
    // server は {"status":"ok","lane":"...","clients":N} を返す。 そのまま echo して
    // caller (script / human) が clients 数を確認できるよう stdout に。
    println!("{}", text);
    Ok(())
}

/// VP-124 Phase 1: SP-aware Performer Lane delete を試みる helper。
///
/// `vp lane rm <name>` (= 個別削除) で呼ばれ、 parent SP が稼働中なら HTTP DELETE 経由で
/// `delete_lane_orchestrated` を発火 (= PTY kill + tmux kill + lane rm + SystemEvent broadcast を
/// SP 側で atomically 実行)。 SP 不在 / API failure なら false 返して filesystem-only fallback
/// (= 現挙動の `ws::remove_performer`) に委譲。
///
/// best-effort: 中間 failure (SP unreachable, network error 等) は warn print して false。
/// SP 200 OK のみ true、 SP 4xx / 5xx は failure 扱い。
fn try_sp_delete_performer(performer_name: &str) -> bool {
    let (project_name, port) = match resolve_parent_project() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("  SP delete skipped (parent project resolve failed: {e})");
            return false;
        }
    };

    // address 構築: `<project>/performer/<name>`、 URL encoding は `/` のみ (slug は ASCII safe)。
    let address = format!("{project_name}/performer/{performer_name}");
    let address_enc = address.replace('/', "%2F");
    let url = format!("http://[::1]:{port}/api/lanes?address={address_enc}&cleanup=true");

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  SP delete skipped (http client build failed: {e})");
            return false;
        }
    };

    match client.delete(&url).send() {
        Ok(resp) if resp.status().is_success() => {
            // body は DeletedLaneInfo JSON、 user 向けに要点だけ要約
            let summary = resp
                .json::<serde_json::Value>()
                .ok()
                .map(|v| {
                    let pid = v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
                    let tmux_killed = v
                        .get("tmux_killed")
                        .and_then(|t| t.as_bool())
                        .unwrap_or(false);
                    let cleanup = v
                        .get("cleanup")
                        .and_then(|c| c.as_str())
                        .unwrap_or("(skipped)")
                        .to_string();
                    format!("pid={pid} tmux_killed={tmux_killed} cleanup={cleanup}")
                })
                .unwrap_or_else(|| "(no body)".to_string());
            eprintln!("削除: {address} (SP orchestrated: {summary})");
            true
        }
        Ok(resp) => {
            eprintln!(
                "  SP delete failed (status={}), falling back to fs-only",
                resp.status()
            );
            false
        }
        Err(e) => {
            eprintln!("  SP unreachable ({e}), falling back to fs-only");
            false
        }
    }
}

/// 現在の repo root から parent project 名と SP port を導出
fn resolve_parent_project() -> Result<(String, u16)> {
    let repo_root = lane::config::find_repo_root()
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
