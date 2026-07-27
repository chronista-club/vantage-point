//! Vantage Point CLI — AI協働開発プラットフォーム
//!
//! Usage:
//!   vp            # 稼働中インスタンス一覧（vp ps）
//!   vp sp start   # repo サーバーを起動
//!   vp lane capture <lane>  # lane console を読む (tmux 非依存)
//!   vp mcp        # MCPサーバーとして起動（stdio）
//!   vp daemon     # daemon 管理
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # デバッグ表示モード
//!   VANTAGE_PROJECT_DIR=/path/to/repo  # デフォルトrepoディレクトリ
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
    /// 全 Process + daemon を一括再起動
    #[command(alias = "ra")]
    RestartAll,
    /// 稼働中のインスタンス一覧
    #[command(alias = "list")]
    Ps,
    /// session の「今なにを」を 1 行で報告（now-line、doc 51 §1 A3b）。
    /// 宛先は env（VP_REPO / VP_LANE / VP_SESSION_KEY）から自動導出 — AI が自分の
    /// shell tool からサブタスクの切れ目ごとに打つ想定
    Now {
        /// 「今なにを」の 1 行（例: "panic 箇所を特定中"）
        text: String,
        /// lane address 明示（env 不在の手動実行用: "<repo>/root" 等）
        #[arg(long)]
        lane: Option<String>,
        /// session key 明示（省略時 VP_SESSION_KEY → それも無ければ root）
        #[arg(long)]
        session: Option<u32>,
    },
    /// repos.kdl を現実と同期 — ghost repo (dir 実在せず) を除去
    Sync,
    /// 設定と登録済みrepoを表示
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

    /// daemon 管理 — 全 Process を統括する常駐プロセス
    ///
    /// 旧 `vp conductor` alias は撤去 (conductor は lane 役割名に確定、語の衝突回避)。
    Daemon {
        /// 待ち受けポート番号（サブコマンド省略時に使用）
        #[arg(short, long, default_value_t = cli::daemon_port())]
        port: u16,
        /// サブコマンド（省略時は start として動作）
        #[command(subcommand)]
        command: Option<commands::daemon::DaemonCommands>,
    },

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

    /// performer Lane 管理（旧 vp ws、Phase 1 で統合）
    #[command(subcommand, alias = "ws", alias = "workspace")]
    Lane(LaneCommands),

    /// 登録 repo 管理 — daemon に直接 Unison RPC (add/remove/rename/enable/disable/reorder/list)
    #[command(subcommand)]
    Repos(commands::repos::ReposCommands),

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

/// performer Lane コマンド（lane library への薄い wrapper）
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
    /// default は `<name>\t<branch>\t<path>` の tab-separated 簡易出力 (= fs scan、 repo 不要)。
    /// `--detail` で repo `/api/lanes` を query して MCP `list_lanes` 同等の JSON (= state /
    /// agent / pid / cwd / performer_status / mailbox_addresses 付き) を出力する (= repo 稼働中のみ)。
    #[command(alias = "list")]
    Ls {
        /// repo `/api/lanes` から MCP list_lanes 同等の詳細 JSON を取得して出力
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
    ///
    /// 判定は Repo Host（`host::farewell`）が 3 値（削除可能 / 保持 / 要判断）で出す。
    /// 要判断が続いている lane には「何回連続・初回いつ」が付く (doc 44 §7.5 の帳簿)。
    Cleanup {
        /// 確認なしで強制削除
        #[arg(long, short)]
        force: bool,
    },
    /// この repo の見送りの記録を新しい順に表示する (doc 44 §7.5、Repo Host の帳簿)
    ///
    /// 「いつ何を見送ったか」と「判断待ちがいつから何回続いているか」。帳簿は daemon が
    /// 専有する db/machine にあるので daemon 稼働が前提。
    History {
        /// 表示件数の上限 (0 = 無制限)
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 現 repo の vp-app の active Lane を切り替える (= mcp__switch_lane の CLI pair、Unison-native)
    ///
    /// `name` は lane token: 'conductor' (lead) or performer 名 (例: 'feat-api')。現 repo の
    /// local repo に `SwitchLane` を投げ、canvas channel 経由で vp-app がその lane を active 化する。
    /// 該当 repo の repo が稼働している必要あり。unknown lane は vp-app 受信側で no-op。
    Switch {
        /// active 化する lane token ('conductor' or performer 名)
        name: String,
    },
    /// この lane の最後の CC session id を表示 (R3-b、 conversation spawn の --resume 用)
    ///
    /// repo / lane は flag 優先、 無ければ VP_REPO / VP_LANE env から導出。
    /// 未記録 / env 不足なら何も出力せず exit 0 (caller は空文字で fallback 判定)。
    /// id の書き手は UserPromptSubmit hook (`vp wire hook-check`)。
    LastSession {
        /// repo 名 (省略時 VP_REPO env)
        #[arg(long)]
        repo: Option<String>,
        /// lane label: conductor / performer 名 (省略時 VP_LANE env)
        #[arg(long)]
        lane: Option<String>,
    },
    /// resume 失敗の観測記録 (`||` chain 中継専用 — 記録して常に exit 1)
    ///
    /// tui type-ahead `claude --resume 'X' … || vp lane resume-failed 'X' || claude …` から
    /// 呼ばれる。repo / lane は VP_REPO / VP_LANE env から導出。記録に失敗しても exit 1
    /// (chain の fresh fallback を止めない)。手動実行は想定しない。
    ResumeFailed {
        /// 失敗した resume 対象 (session id or 'continue')
        attempted: String,
    },
    /// lane console の現在画面を読む (tmux decoupling: 旧 `vp tmux capture` の後継)
    ///
    /// repo の slot ごとの Term grid (TermAttach) を render して返す。tmux 不要。
    /// doc 46 P5: slot は lane に 1 枚ではなく session ごと。`--session` で同居する別の
    /// console を読む (省略時は root = lane の代表)。枚数は `vp lane slots` で判る。
    Capture {
        /// lane address ("<repo>/root" / "<repo>/performer/<name>")
        lane: String,
        /// 読む slot の session key (省略時 root)
        #[arg(long)]
        session: Option<u32>,
    },
    /// lane が持つ console slot の一覧を表示 (doc 46 P5 — slot は session ごと)
    ///
    /// repo の in-memory な slot 実体を読む (session key / pid / 生死 / root か)。
    /// 「今この lane に端末が何枚あるか」を UI を通さずに確認する口。
    Slots {
        /// lane address ("<repo>/root" / "<repo>/performer/<name>")
        lane: String,
    },
    /// lane に console (slot) をもう 1 枚立てる (doc 46 P5 — 非 root session の producer)
    ///
    /// **新しい session を採番**してそこに console を立てる (doc 46 §1.5「Pane は必ず新しい
    /// session id で始まる」= session ↔ Pane は 1:1)。root (lane の代表 / mailbox の主) は
    /// 動かないので、既存の console はそのまま生き続ける。読むのは `vp lane capture --session`。
    SlotNew {
        /// lane address ("<repo>/root" / "<repo>/performer/<name>")
        lane: String,
        /// engine (agent 名: conversation / codex / grok / opencode / shell。省略時は現 root の engine)
        #[arg(long)]
        agent: Option<String>,
    },
    /// lane の console (slot) を 1 枚閉じる ([`SlotNew`](LaneCommands::SlotNew) の対)
    ///
    /// GUI の名札 ✕ と**同じ動詞** (`conversation_session_remove`)。session を registry から取り除き、
    /// 実体 (PtySlot / chat engine) は reconcile が畳む (doc 53 §12.4)。replay も破棄される。
    /// **root は閉じられない** (lane の代表 = mailbox の主。素に戻すのは `vp lane restart --fresh`)。
    SlotClose {
        /// lane address ("<repo>/root" / "<repo>/performer/<name>")
        lane: String,
        /// 閉じる session (`vp lane slots` の SESSION 列)
        #[arg(long)]
        session: u32,
    },
    /// この repo の開発起点 lane を表示 / 設定する (doc 44 D4、Repo Host の帳簿)
    ///
    /// 引数なしで現在の起点を表示。lane 名を渡すとその lane を起点に指定する。
    /// 指定は **帳簿のポインタ書き換えだけ** — cwd も active lane も engine も動かない (D5)。
    /// 未指定なら予約名 `conductor` が起点（従来挙動）。daemon (Daemon) 稼働が前提。
    Origin {
        /// 起点にする lane 名 (省略時は現在の起点を表示)
        name: Option<String>,
    },
    /// lane の claude / shell に text + Enter を注入 (旧 `vp tmux send-keys` / `vp directmsg` の後継)
    Nudge {
        /// lane address ("<repo>/root" / "<repo>/performer/<name>")
        lane: String,
        /// 注入するテキスト (Enter は自動付与、submit 意味論は repo 側 deliver_nudge)
        text: String,
        /// 送り先 slot の session key (省略時 root = mailbox を名乗る住人、doc 39)
        #[arg(long)]
        session: Option<u32>,
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

    // daemon server 本体 (`vp daemon start` / `vp daemon` / subcommand 省略) は stdout/stderr が
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
        Commands::Now {
            text,
            lane,
            session,
        } => commands::now::report(&text, lane.as_deref(), session, &config),
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
            let cmd = command.unwrap_or(commands::daemon::DaemonCommands::Start { port });
            commands::daemon::execute(cmd)
        }
        // doc 44 P1 (fold-in): `vp sp` は退役。repo は daemon プロセス内の
        // `Arc<AppState>` になり、外から起動する概念が消えた。lifecycle 操作は
        // `vp repos start|stop`（名詞を repo から repo へ移した）。
        // tmux decoupling PR2: `vp hd` / `vp tmux` は退役。 lane の console 操作は
        // `vp lane capture` / `vp lane nudge` (lane 語彙の後継)。
        #[cfg(feature = "midi")]
        Commands::Midi(cmd) => commands::midi::execute(cmd),
        Commands::Db(cmd) => commands::db::execute(cmd),

        Commands::Lane(cmd) => execute_lane(cmd),
        Commands::Repos(cmd) => {
            // repos 操作は daemon に直接 Unison RPC (async)。 auth/wire/flow と同じ
            // per-command Runtime で block_on する。
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(commands::repos::execute(cmd))
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

/// performer Lane 操作を lane library に委譲
///
/// wiremsg R5-4: 旧 msgbox の registry サブシステム (performer 作成/削除時の
/// `performer-{name}@{repo}` actor register/unregister) は撤去済。
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
            // VP-124: repo-aware delete を試みる (orchestration: PTY kill + tmux kill +
            // lane workspace rm + SystemEvent broadcast を 1 HTTP call で完結)。
            // --all は filesystem-only fallback (一括削除は repo 経由する意味なし、 個別 Lane
            // address が必要なため)。 repo 不在 / failure なら現挙動 (ws::remove_performer fs-only)
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
        LaneCommands::History { limit } => {
            ws::show_farewell_history(limit).map_err(|e| anyhow::anyhow!(e))
        }
        LaneCommands::Switch { name } => switch_lane_via_quic(&name),
        LaneCommands::LastSession { repo, lane } => {
            // R3-b → doc 40: 会話 id の SSOT は session registry（root session の conversation）。
            // 旧 cc_session store 直読みは漏斗化で更新されなくなったため registry 経由に切替
            // （load 内の backfill bridge が旧 store も拾う）。未記録 / env 不足は
            // 「出力なし exit 0」 — caller の `[ -n "$RESUME_ID" ]` 判定で fallback させる。
            let repo = repo.or_else(|| std::env::var("VP_REPO").ok());
            let lane = lane.or_else(|| std::env::var("VP_LANE").ok());
            if let (Some(p), Some(l)) = (repo, lane) {
                let reg = vantage_point::lane::session_registry::load(&p, &l, "claude");
                if let Some(id) = reg
                    .sessions
                    .iter()
                    .find(|s| s.key == reg.root)
                    .and_then(|s| s.conversation.as_deref())
                {
                    println!("{id}");
                }
            }
            Ok(())
        }
        LaneCommands::ResumeFailed { attempted } => {
            // 記録して常に exit 1 = `||` chain の中継。この行は slot の scrollback に残り、
            // 「無音で fresh になった」を user からも見えるようにする（観測装置 F4）。
            let repo = std::env::var("VP_REPO").unwrap_or_else(|_| "-".into());
            let lane = std::env::var("VP_LANE").unwrap_or_else(|_| "-".into());
            let _ = vantage_point::lane::resume_failure::append(&repo, &lane, &attempted);
            eprintln!(
                "[vp] resume 失敗: '{attempted}' を継げませんでした — fresh session に fallback します (log: resume_failures.log)"
            );
            std::process::exit(1);
        }
        LaneCommands::Capture { lane, session } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::capture(&lane, session, &config)
        }
        LaneCommands::Slots { lane } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::slots(&lane, &config)
        }
        LaneCommands::SlotNew { lane, agent } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::slot_new(&lane, agent.as_deref(), &config)
        }
        LaneCommands::SlotClose { lane, session } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::slot_close(&lane, session, &config)
        }
        LaneCommands::Origin { name } => lane_origin(name.as_deref()),
        LaneCommands::Nudge {
            lane,
            text,
            session,
        } => {
            let config = Config::load().unwrap_or_default();
            commands::lane_ctl::nudge(&lane, session, &text, &config)
        }
    }
}

/// `vp lane ls --detail` 実装: daemon repo-proxy ask `lanes_list` を query して pretty JSON で出力。
///
/// lanes portless (doc 27 §3.4.5): 旧 SP `/api/lanes` 直叩きを撤去し Daemon :32000 の repo-proxy に
/// 一本化 (`try_sp_delete_performer` と同型、 repo port 解決不要)。 repo 不在 (= daemon に未登録 /
/// cwd が repo 外 / repo 未起動) なら daemon が control channel 逆引き失敗で error を返す。 `--detail` を
/// 要求した時点で repo 稼働を前提とする (= fs-only fallback はせず、 明示的に user に repo 未起動を伝える)。
///
/// MCP `list_lanes` の mailbox_addresses 計算 / repo_addresses synthesis までは実装せず、
/// dispatch `lanes_list` の生 JSON (`{lanes:[...]}`) を pretty print する (mailbox は SKILL.md doc 案内)。
fn list_performers_detail() -> Result<()> {
    // repo_root = repo_path (Daemon handshake の stable identifier)。 repo port は repo-proxy で不要。
    let repo_root = lane::config::find_repo_root()
        .map_err(|e| anyhow::anyhow!("repo root 解決失敗 (--detail は repo 内が前提): {}", e))?;
    let repo_path = repo_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("repo path に invalid UTF-8"))?;

    let resp = vantage_point::commands::process_client::daemon_repo_request_blocking(
        cli::daemon_port(),
        repo_path,
        "lanes_list",
        serde_json::json!({}),
    )
    .map_err(|e| anyhow::anyhow!("lanes_list 失敗 (--detail は repo 稼働が前提): {}", e))?;

    println!(
        "{}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    Ok(())
}

/// active Lane 切り替え CLI 実装 (= mcp__switch_lane の CLI pair)。
///
/// L0 portless: 現 repo の repo に `SwitchLane` RepoMessage を Daemon :32000 の repo-proxy
/// ask で forward する（repo は listen しないので旧来の repo 直結 QUIC は撤去）。daemon が repo_path
/// を path_key に正規化して当該 repo の control channel を逆引きし、`dispatch_repo_method`
/// （"switch_lane" → `handle_process_message`）へ forward → hub.broadcast → topic
/// `process/board/event/switch-lane`（非 retained）→ canvas channel 経由で vp-app が受信し、
/// その lane を active 化する（lane-within-repo の per-repo 切替）。
/// MCP 側も `process_call("switch_lane", …)`（mcp.rs、repo-proxy 経由）で同 dispatch に着地。
fn switch_lane_via_quic(name: &str) -> Result<()> {
    // lane token = "root" (lead) or performer 名。server / vp-app 側で実在 lane と照合
    // （unknown lane は vp-app 受信側で no-op）。
    let trimmed = name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("lane token is required (空文字不可)");
    }

    // repo_root = repo_path (daemon repo-proxy handshake の stable identifier)。
    // L0 portless: repo port 解決は不要（daemon が path_key 逆引きで forward する）。
    let repo_root = lane::config::find_repo_root()
        .map_err(|e| anyhow::anyhow!("find_repo_root failed: {}", e))?;
    let (Some(repo_name), Some(repo_path)) = (
        repo_root.file_name().and_then(|n| n.to_str()),
        repo_root.to_str(),
    ) else {
        anyhow::bail!("repo path contains invalid UTF-8");
    };

    // SwitchLane を daemon repo-proxy ask で repo へ forward（payload = RepoMessage JSON、
    // `{"type":"switch_lane","lane":...}`）。repo 側 dispatch_repo_method が受けて broadcast。
    let msg = vantage_point::protocol::RepoMessage::SwitchLane {
        lane: trimmed.to_string(),
    };
    let payload = serde_json::to_value(&msg)?;
    vantage_point::commands::process_client::daemon_repo_request_blocking(
        cli::daemon_port(),
        repo_path,
        "switch_lane",
        payload,
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "repo {} への switch_lane 送信失敗 (daemon repo-proxy): {}",
            repo_name,
            e
        )
    })?;

    println!(
        "switched active lane to '{}' (repo={}, via daemon repo-proxy)",
        trimmed, repo_name
    );
    Ok(())
}

/// `vp lane origin [<name>]` 実装: Repo Host の帳簿にある開発起点ポインタを読む / 書く。
///
/// `switch_lane_via_quic` と同じ daemon repo-proxy ask 経路（repo port 解決不要）。
/// 起点は repo 単位の 1 本なので lane address ではなく **lane 名**で受ける。
///
/// 表示は「どう決まったか」まで出す（D4 の既定フォールバックと、指した lane が消えた
/// dangling を区別できないと、指定が失われたことに気付けない）。
fn lane_origin(name: Option<&str>) -> Result<()> {
    let repo_root = lane::config::find_repo_root()
        .map_err(|e| anyhow::anyhow!("repo root 解決失敗 (repo 内で実行してください): {}", e))?;
    let Some(repo_path) = repo_root.to_str() else {
        anyhow::bail!("repo path contains invalid UTF-8");
    };

    let (method, payload) = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => ("lane_origin_set", serde_json::json!({ "lane": n })),
        None => ("lane_origin_get", serde_json::json!({})),
    };

    let resp = vantage_point::commands::process_client::daemon_repo_request_blocking(
        cli::daemon_port(),
        repo_path,
        method,
        payload,
    )
    .map_err(|e| anyhow::anyhow!("{} 失敗 (daemon repo-proxy): {}", method, e))?;

    let origin: vantage_point::host::ledger::Origin = serde_json::from_value(resp)
        .map_err(|e| anyhow::anyhow!("{} の応答を解釈できません: {}", method, e))?;

    use vantage_point::host::ledger::OriginSource;
    match &origin.source {
        OriginSource::Default => println!("開発起点: {} (既定 — 未指定)", origin.name),
        OriginSource::Pinned => println!("開発起点: {} (指定済)", origin.name),
        OriginSource::Dangling { lane_id } => println!(
            "開発起点: {} (既定に復帰 — 指定されていた lane が見つかりません: id={})",
            origin.name, lane_id
        ),
    }
    Ok(())
}

/// VP-124 Phase 1: repo-aware Performer Lane delete を試みる helper。
///
/// `vp lane rm <name>` (= 個別削除) で呼ばれ、 parent repo が稼働中なら daemon repo-proxy ask
/// (`lane_delete`) 経由で `delete_lane_orchestrated` を発火 (= PTY kill + tmux kill + lane rm +
/// SystemEvent broadcast を repo 側で atomically 実行)。 repo 不在 / failure なら false 返して
/// filesystem-only fallback (= 現挙動の `ws::remove_performer`) に委譲。
///
/// F6② (doc 27 §3.4.5/§6): 旧 SP 直結 (`DELETE /api/lanes` reqwest) を撤去し Daemon :32000 の
/// repo-proxy に一本化 (repo port 解決不要、 L1 portless 前進)。 best-effort: 全 failure
/// (repo 不在 / lane not found / network) は warn print して false → fs-only に委譲。
fn try_sp_delete_performer(performer_name: &str) -> bool {
    // repo_root = repo_path (Daemon handshake の stable identifier)。 repo port は repo-proxy で不要。
    let repo_root = match lane::config::find_repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  repo delete skipped (repo root resolve failed: {e})");
            return false;
        }
    };
    let (Some(repo_name), Some(repo_path)) = (
        repo_root.file_name().and_then(|n| n.to_str()),
        repo_root.to_str(),
    ) else {
        eprintln!("  repo delete skipped (repo path に invalid UTF-8)");
        return false;
    };

    // address 構築: `<repo>/performer/<name>` (repo 側 parse_address が逆変換)。
    let address = format!("{repo_name}/performer/{performer_name}");
    let payload = serde_json::json!({ "address": address, "cleanup": true });

    match vantage_point::commands::process_client::daemon_repo_request_blocking(
        cli::daemon_port(),
        repo_path,
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
            eprintln!("削除: {address} (repo orchestrated: pid={pid} cleanup={cleanup})");
            true
        }
        Err(e) => {
            // repo 不在 / lane not found / network 等は全て fs-only fallback (旧 non-2xx 挙動踏襲)。
            eprintln!("  repo delete failed ({e}), falling back to fs-only");
            false
        }
    }
}
