//! vp sp — Star Platinum サーバー管理
//!
//! SP（HTTP/QUIC サーバー）のライフサイクル管理。
//! tmux セッションの管理は hd.rs に分離。

use anyhow::Result;
use clap::Subcommand;

use crate::cli::DebugModeArg;
use crate::config::Config;
use crate::protocol::DebugMode;

#[derive(Subcommand)]
pub enum SpCommands {
    /// SP サーバーを起動（フォアグラウンドでブロック実行）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long)]
        port: Option<u16>,
        /// デバッグモード
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,
        /// プロジェクトディレクトリ（省略時は cwd）
        #[arg(long, short = 'C')]
        project_dir: Option<String>,
    },
    /// SP サーバーを停止
    Stop,
    /// SP サーバーの状態 + Stand 一覧を表示
    Status,
    /// SP サーバーを再起動
    Restart,
}

/// vp sp コマンドを実行
pub fn execute(cmd: SpCommands, config: &Config) -> Result<()> {
    match cmd {
        SpCommands::Start {
            port,
            debug,
            project_dir,
        } => {
            let dir = project_dir.unwrap_or_else(|| {
                std::env::current_dir()
                    .expect("cwd 取得失敗")
                    .to_string_lossy()
                    .to_string()
            });
            sp_start(&dir, port, debug, config)
        }
        SpCommands::Stop => {
            let cwd = std::env::current_dir()?;
            sp_stop(&cwd.to_string_lossy(), config)
        }
        SpCommands::Status => {
            let cwd = std::env::current_dir()?;
            sp_status(&cwd.to_string_lossy(), config)
        }
        SpCommands::Restart => {
            let cwd = std::env::current_dir()?;
            sp_restart(&cwd.to_string_lossy(), config)
        }
    }
}

/// SP サーバーを起動（フォアグラウンド、ブロック実行）
///
/// 既に起動中なら何もしない。未起動ならサーバーを起動してブロックする。
/// バックグラウンド実行が必要な場合は呼び出し元が detached subprocess として spawn する。
fn sp_start(
    project_dir: &str,
    explicit_port: Option<u16>,
    debug: Option<DebugModeArg>,
    config: &Config,
) -> Result<()> {
    // VP-189 follow-up: projects.kdl を sync (起点 dir 登録 + ghost 除去)。
    // 「起点 dir → SP」 のメンタルモデル: 全 SP 起動経路 (GUI / vp sp start 直 /
    // daemon spawn) がこの sp_start に収束するため、 ここ 1 点で漏れなく同期できる。
    //
    // PR-D: daemon 在なら notify_world_sync 経由で daemon 側 (DB) が直列化するため file lock 不要。
    // daemon 不在フォールバック (kdl 直書き) 時のみ write 競合がありうるが、 現状
    // default=1 (sequential) なので実害なし。 並列化時は daemon 経由を前提にする。
    // PR-D: 起点登録 + ghost 除去を daemon (db/world 真実源) 経由で。 daemon 不在は kdl フォールバック。
    let sync_outcome = match crate::world_client::notify_world_sync(Some(project_dir)) {
        Some(o) => Ok(o),
        None => crate::projects_file::ProjectsFile::sync(Some(std::path::Path::new(project_dir))),
    };
    match sync_outcome {
        Ok(outcome) => {
            if let Some(name) = &outcome.added {
                println!("📁 project を登録しました: {name}");
            }
            for name in &outcome.removed {
                println!("🧹 ghost project を除去: {name}");
            }
        }
        Err(e) => tracing::warn!("projects の sync に失敗 (SP 起動は続行): {e}"),
    }

    let mut port = explicit_port.unwrap_or_else(|| resolve_port(project_dir, config));
    let debug_mode = debug.map(DebugMode::from).unwrap_or_default();

    // Port collision 解決: port を誰かが listening してるなら、
    //   (a) 自 project の SP なら → 既に起動済みで skip
    //   (b) 他 project の SP もしくは non-SP なら → port 衝突、 alternative port を探して retry
    //   (c) 未占有 → そのまま spawn
    // 旧 bug: (b) を (a) と区別せず skip → 永遠に起動しない (bikeboy 2026-04-29 観測)。
    if is_server_responding(port) {
        if is_sp_for_project_responding(port, project_dir) {
            println!("✅ SP サーバーは既に起動済み (port={})", port);
            return Ok(());
        }
        // explicit_port 指定時は collision を fail とする (user 意図優先)、
        // 自動 resolve なら別 port を探す
        if explicit_port.is_some() {
            anyhow::bail!(
                "⚠️ port {} は他 SP が占有中 (project_dir 不一致)。 別 port を指定するか、 該当 SP を停止してください",
                port
            );
        }
        let alt = crate::resolve::find_available_port().ok_or_else(|| {
            anyhow::anyhow!(
                "port {} が他 SP に占有されており、 33000-33010 範囲に空き port なし",
                port
            )
        })?;
        println!(
            "⚠️ port {} は他 project の SP に占有されてるため、 port {} で起動します",
            port, alt
        );
        port = alt;
    }

    // TheWorld 自動起動
    if let Err(e) = crate::daemon::process::ensure_daemon_running(crate::cli::world_port()) {
        tracing::warn!("TheWorld 自動起動失敗（SP は続行）: {}", e);
    }

    println!("⭐ SP サーバー起動 (port={})...", port);

    let cap_config = crate::process::CapabilityConfig {
        project_dir: project_dir.to_string(),
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { crate::process::run(port, debug_mode, cap_config).await })
}

/// SP サーバーを停止
///
/// SP-portless: SP は HTTP listener を撤去したため、停止は proven pattern
/// `cli::stop_process` に委譲する。これは PID を World registry から引き、graceful を
/// World :32000 process-proxy "shutdown"（reverse-routing で SP control channel に到達）
/// で送り、timeout で `force_kill` にフォールバックする。
/// 旧 `/api/shutdown` POST も `kill_process_on_port`(lsof) も portless SP には届かず
/// 停止不能だった（SP は port を listen しない）。
fn sp_stop(project_dir: &str, _config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));

    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(crate::cli::stop_process(running.port))?;
    } else {
        println!("ℹ️  SP サーバーは稼働していません");
    }

    Ok(())
}

/// SP サーバーの状態 + HD セッション一覧を表示
///
/// SP-portless: 稼働判定は World registry（`discovery`、単一の真実源）ベース。
/// 旧 SP `/api/health` 直 GET による起動時刻 / Stand 一覧表示は、HTTP listener 撤去で
/// 届かず silent fail していたため撤去した（World process-proxy 経由の stands 取得は
/// 将来の enhancement）。
fn sp_status(project_dir: &str, config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));
    let project_name = crate::resolve::project_name_from_path(&normalized, config);

    println!("⭐ SP ステータス: {}", project_name);
    println!();

    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
        // port は portless 下では listen socket ではなく registry 上の論理 identity
        println!(
            "   サーバー: ✅ running (port={}, pid={})",
            running.port, running.pid
        );
    } else {
        println!("   サーバー: ❌ not running");
    }

    // (tmux decoupling PR2: 旧「HD セッション情報の参考表示」は tmux と共に退役 —
    //  lane の状態は `vp lane ls --detail` / `vp lane capture` で確認する)

    Ok(())
}

/// SP サーバーを再起動
fn sp_restart(project_dir: &str, config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));

    let saved_port = crate::discovery::find_by_project_blocking(&normalized).map(|r| r.port);

    println!("🔄 SP サーバーを再起動します...");
    sp_stop(project_dir, config)?;

    // ポート開放をポーリング（最大5秒）
    if let Some(port) = saved_port {
        for _ in 0..50 {
            if !is_server_responding(port) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        sp_start(project_dir, Some(port), None, config)
    } else {
        sp_start(project_dir, None, None, config)
    }
}

/// プロジェクトディレクトリからポートを解決
fn resolve_port(project_dir: &str, config: &Config) -> u16 {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));
    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
        return running.port;
    }
    if let Some(idx) = config.find_project_index(&normalized) {
        crate::resolve::port_for_configured(idx, config).unwrap_or(33000 + idx as u16)
    } else {
        crate::resolve::find_available_port().unwrap_or(33005)
    }
}

// =============================================================================
// SP 起動・疎通 probe helper (refactor R1-2 で旧 commands/start.rs から移設)
// =============================================================================

/// SP を detached subprocess として spawn
///
/// `vp sp start -C <dir> [-p <port>]` を独立プロセスとして起動。
/// 呼び出し元が終了しても SP は生存する。
pub fn spawn_sp_detached(project_dir: &str, port: Option<u16>) -> Result<()> {
    let vp_bin = crate::cli::which_vp()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "vp".into());

    let mut args = vec!["sp".to_string(), "start".to_string()];
    args.push("-C".to_string());
    args.push(project_dir.to_string());
    if let Some(p) = port {
        args.push("-p".to_string());
        args.push(p.to_string());
    }

    std::process::Command::new(&vp_bin)
        .args(&args)
        // GUI/launchd 起動の最小 PATH が SP → mise → claude へ伝播するのを spawn 最上流で断つ。
        .env("PATH", crate::spawn_env::augmented_spawn_path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("SP spawn 失敗: {}", e))?;

    Ok(())
}

/// SP の HTTP サーバーが応答するまでポーリング（最大5秒）
pub fn wait_for_ready(port: u16) -> Result<()> {
    let max_attempts = 50; // 100ms × 50 = 5秒

    for i in 0..max_attempts {
        match std::net::TcpStream::connect_timeout(
            &format!("[::1]:{}", port).parse().unwrap(),
            std::time::Duration::from_millis(100),
        ) {
            Ok(_) => {
                tracing::info!("SP ready (attempt {})", i + 1);
                return Ok(());
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    tracing::warn!("SP readiness check timed out, proceeding anyway");
    Ok(())
}

/// SP サーバーが応答するかチェック（TCP 接続テスト）
pub fn is_server_responding(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("[::1]:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}

/// 指定 port に listening してる SP が **特定の project_dir 用** かを verify。
///
/// `is_server_responding` は port 占有のみ check、 「誰の SP か」 を区別しない。
/// 結果として、 unrelated project の SP が同 port を掴んでる時に false positive で
/// 「既に起動済み」 判定 → spawn skip → 永遠に起動しない bug が発生していた
/// (bikeboy が config 未登録で auto-port が他 project と衝突したケースで観察、 2026-04-29)。
///
/// 本関数は:
/// 1. TCP 疎通 (= `is_server_responding`) で fast skip
/// 2. `/api/health` の `project_dir` field を fetch
/// 3. `Config::normalize_path` で 両 path を canonicalize して比較
///
/// → port の SP が **正しく自 project 用** なら true、 そうでなければ false。
///
/// 別 thread で current_thread runtime を立てる構造: 呼び出し元が既に tokio runtime を
/// 持ってる場合 (panic: Cannot start a runtime from within a runtime) を避ける為。
pub fn is_sp_for_project_responding(port: u16, project_dir: &str) -> bool {
    if !is_server_responding(port) {
        return false;
    }
    let target = crate::config::Config::normalize_path(std::path::Path::new(project_dir));
    let url = format!("http://127.0.0.1:{}/api/health", port);
    let result = std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return false,
        };
        rt.block_on(async {
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(500))
                .build()
            {
                Ok(c) => c,
                Err(_) => return false,
            };
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(_) => return false,
            };
            let json: serde_json::Value = match resp.json().await {
                Ok(j) => j,
                Err(_) => return false,
            };
            let Some(actual_dir) = json.get("project_dir").and_then(|v| v.as_str()) else {
                return false;
            };
            let actual = crate::config::Config::normalize_path(std::path::Path::new(actual_dir));
            actual == target
        })
    })
    .join();
    result.unwrap_or(false)
}
