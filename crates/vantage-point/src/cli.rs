//! CLIヘルパー関数
//!
//! インスタンス管理、デバッグ設定、ユーティリティ関数を提供する。

use anyhow::Result;

/// daemon (:32000) の `/api/health` レスポンス parser。
///
/// L0 finale: repo は HTTP listener を撤去したが、 **daemon は HTTP を保持**するため (daemon/process.rs
/// が Daemon 自身の health を読む)、 この struct は残す。 旧 SP /api/health 用の用途 (check_status /
/// scan_instances) は撤去済。
#[derive(serde::Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub pid: u32,
    #[serde(default)]
    pub repo_dir: Option<String>,
}

/// daemon（Daemon 統合）のデフォルトポート。
///
/// VP_PROFILE 分離 (dev/brew 混在根治): brew=32000 / dev=32100。 repo portless なので実 listener は
/// daemon 単一 → この 1 本を profile でずらせば daemon bind / app connect / repo→daemon connect が
/// 芋づるで追随する。 定義は `vp_paths::default_daemon_port()` (全 crate 共有の SSOT)。
pub fn daemon_port() -> u16 {
    vp_paths::default_daemon_port()
}

/// 稼働中 repo を一覧表示する（`vp ps`）。
///
/// 真実源は Daemon registry（`discovery::list` = Daemon :32000 が維持する稼働 repo 一覧）。
///
/// # 列の意味論（doc 44 §5.3）
///
/// fold-in で **PORT / PID 列は情報量を失った** — repo は daemon と同一プロセスなので
/// pid は全行 Daemon 自身、port は listen しないので常に 0 になる。どちらも
/// 「repo = プロセス」という前提の上にあった表示で、その前提を fold-in が消した。
///
/// 代わりに repo 間の実体的な差である **LANES（何本のラインを抱えているか）** と
/// **STATUS（そのうち動いているものがあるか = active / idle）** を出す。
/// lane 個別の詳細（kind / agent / pid / state）は `vp lane` が持つ。
pub fn list_instances(config: &crate::config::Config) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // control plane は Unison に寄せる方針（KDL schema + drift テスト + MCP tool 合成が
        // 付いてくる）。processes / lanes とも 1 接続の別 stream で引く。
        let client = crate::daemon::client::DaemonControlClient::connect(daemon_port(), 3)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "daemon (port {}) に接続できません。 `vp daemon start` で起動してください: {}",
                    daemon_port(),
                    e
                )
            })?;

        // processes 取得の失敗は「repo ゼロ」と意味が違う（daemon 障害 / registry stream
        // 不通）。握り潰すと 14 repo 稼働中でも「repo なし」と誤誘導するため、明示的に
        // エラーとして上げる。lanes は失敗しても LANES 列を `-` に degrade できる（下参照）。
        let processes = client.processes_list().await.map_err(|e| {
            anyhow::anyhow!("稼働 repo の取得に失敗しました（daemon に届いていない可能性）: {e}")
        })?;
        if processes.is_empty() {
            println!("No running repos found.");
            return Ok(());
        }
        // lanes は取れなくても repo 一覧は出す。失敗時は空集計 → 各行 LANES=`-`。
        let lane_counts = match client.lanes_list().await {
            Ok(lanes) => crate::discovery::count_lanes_by_repo_entries(&lanes),
            Err(e) => {
                eprintln!("⚠ lane 一覧の取得に失敗（LANES は - で表示）: {e}");
                Default::default()
            }
        };
        let instances: Vec<String> = processes
            .iter()
            .filter_map(|p| {
                p.get("repo_path")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();

        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| dunce::canonicalize(&p).ok())
            .map(|p| p.display().to_string());

        // repo 名は長さの幅が大きい（`claude-plugin-chronista-style` = 29 文字）ので、
        // 固定幅だと溢れて LANES 列がずれる。実データから列幅を決める。
        let names: Vec<String> = instances
            .iter()
            .map(|path| crate::resolve::repo_name_from_path(path, config))
            .collect();
        let w = names
            .iter()
            .map(|n| n.chars().count())
            .max()
            .unwrap_or(0)
            .max("REPO".len());

        println!();
        println!("  {:<w$} {:>5}  STATUS", "REPO", "LANES");
        println!("  {:<w$} {:>5}  ──────", "─".repeat(w), "─────");

        for (inst, name) in instances.iter().zip(&names) {
            let is_cwd = if let Some(cwd_str) = &cwd {
                let canonical_proj = dunce::canonicalize(inst)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| inst.clone());
                // separator は OS 依存 (Windows は `\`)。 `/` 決め打ちだと Windows で
                // 子ディレクトリからの `← cwd` マーカーが出ない。
                let prefix = format!("{}{}", canonical_proj, std::path::MAIN_SEPARATOR);
                cwd_str == &canonical_proj || cwd_str.starts_with(&prefix)
            } else {
                false
            };
            let marker = if is_cwd { "  ← cwd" } else { "" };
            // daemon に問い合わせできなかった場合は lane 数不明として `-` を出す
            // （repo 一覧そのものは出せるべきなので、lane 取得失敗で表を潰さない）。
            let (lanes, status) = match lane_counts.get(name) {
                Some(c) if c.running > 0 => (c.total.to_string(), "active"),
                Some(c) => (c.total.to_string(), "idle"),
                None => ("-".to_string(), "idle"),
            };
            println!("  {:<w$} {:>5}  {}{}", name, lanes, status, marker);
        }
        println!();
        println!("詳細: vp lane list");
        Ok(())
    })
}

/// tracing verbosity レベル（`VANTAGE_DEBUG=none|simple|detail` 用）。
///
/// doc 44 P1 (fold-in): 旧 `protocol::DebugMode` は「-d デバッグパネル」と「VANTAGE_DEBUG
/// ログ詳細度」の 2 用途を兼ねていた。前者（DebugInfo broadcast / 旧 WebUI パネル）は
/// end-to-end で dead だったため撤去し、生きている後者（tracing レベル選択）だけを
/// ここへローカル化した。none→warn / simple→info / detail→debug に対応する。
/// wire には乗らないので serde 不要。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugMode {
    #[default]
    None,
    Simple,
    Detail,
}

/// Parse debug mode from environment variable
pub fn parse_debug_env() -> Option<DebugMode> {
    std::env::var("VANTAGE_DEBUG")
        .ok()
        .and_then(|v| match v.to_lowercase().as_str() {
            "none" | "off" | "0" | "false" => Some(DebugMode::None),
            "simple" | "1" | "true" => Some(DebugMode::Simple),
            "detail" | "detailed" | "2" | "verbose" => Some(DebugMode::Detail),
            _ => None,
        })
}

/// Initialize tracing with VP_LOG support
/// VP_LOG環境変数またはDebugModeに基づいてログレベルを設定
/// - VP_LOG=debug|info|warn|error が優先
/// - 未設定の場合、debug_modeに基づいて設定:
///   - None -> warn
///   - Simple -> info
///   - Detail -> debug
///
/// `tui_mode` が true の場合、ログ出力を stderr ではなくファイルにリダイレクト。
/// TUI (ratatui) の alternate screen にサーバーログが漏れるのを防ぐ。
pub fn init_tracing(debug_mode: DebugMode, tui_mode: bool) {
    // panic を log に載せる hook。 subscriber より先に install してよい (hook が発火するのは
    // 実行時 = subscriber 設置後)。 tokio task 内 panic が無言で機能を殺す既存問題への
    // 最小の対処 — cf. `crate::panic_hook` の module doc。
    crate::panic_hook::install();

    // daemon ログローテーション設定。 vp-app 側の `log_init::LOG_MAX_BYTES` /
    // `LOG_KEEP_FILES` と同値に保つこと (2 crate に依存関係が無いため定数を物理共有できず、
    // 各 crate に複製している。 恒久的には vp-paths へ寄せる候補 = Observability Phase B)。
    const DAEMON_LOG_MAX_BYTES: usize = 10 * 1024 * 1024; // 10 MB
    const DAEMON_LOG_KEEP_FILES: usize = 5;
    // VP_LOGが設定されていない場合、debug_modeに基づいてRUST_LOGを設定
    // SAFETY: main()開始直後、他スレッド起動前に呼ばれるため安全
    if std::env::var("VP_LOG").is_err() && std::env::var("RUST_LOG").is_err() {
        // DebugMode::None (= 通常運転) は set しない — 後段の default EnvFilter
        // (vantage_point=info + 依存 crate の chatty log 抑制) に落とす。
        // 旧実装は None でも `vantage_point=warn` を焼き込んでいたため後段 default が
        // 到達不能になり、daemon の INFO (DeviceRegistry/QUIC 起動等の運転記録) が全起動経路で
        // 恒久的に沈黙していた (log 出力先とは独立の第 2 の結線切れ)。
        match debug_mode {
            DebugMode::None => {}
            DebugMode::Simple => unsafe {
                std::env::set_var("RUST_LOG", "vantage_point=info");
            },
            DebugMode::Detail => unsafe {
                std::env::set_var("RUST_LOG", "vantage_point=debug");
            },
        }
    } else if let Ok(vp_log) = std::env::var("VP_LOG") {
        // VP_LOG -> RUST_LOG に変換
        unsafe {
            std::env::set_var("RUST_LOG", format!("vantage_point={}", vp_log));
        }
    }

    // d (log 整理 minimum, mem_1CaSiJkD9HATDY2srrv6D4 Phase B-2 step):
    // RUST_LOG 未指定時の default を **絞り込み** verbose を削減。
    // vantage_point=info を残しつつ、 dependency crate の chatty な debug log を抑制。
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "vantage_point=info,vp_app=info,\
             hyper=warn,hyper_util=warn,reqwest=warn,h2=warn,\
             tokio_tungstenite=warn,tungstenite=warn,\
             quinn=warn,quinn_proto=warn,quinn_udp=warn,\
             rustls=warn,rustls_post_quantum=warn",
        )
    });

    // VP-101 follow-up (Windows daemon support):
    // 環境変数 `VP_DAEMON_LOG_FILE` が指定されていれば、そのパスに直接書き込む。
    // Win-native vp-app から daemon を spawn する際に Logs/daemon.kdl.log 等を渡す想定。
    // GUI subsystem では stderr が NUL 化されるので file writer 必須。
    if let Ok(path_str) = std::env::var("VP_DAEMON_LOG_FILE")
        && !path_str.is_empty()
    {
        let path = std::path::PathBuf::from(&path_str);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // ログローテーション (file-rotate): 容量ベースで切替、 件数ベースで GC して daemon log の膨張を防ぐ。
        // 現行ファイルは `VP_DAEMON_LOG_FILE` が指す bare 名のまま (日付名にしない) なので、
        // tail -F / 既存 reader を壊さない。 rotate 世代は `daemon.kdl.log.1`.. へ逃がす。
        // Mutex で包むと tracing-subscriber が MakeWriter として扱える (WorkerGuard 不要)。
        let appender = std::sync::Mutex::new(file_rotate::FileRotate::new(
            &path,
            file_rotate::suffix::AppendCount::new(DAEMON_LOG_KEEP_FILES),
            file_rotate::ContentLimit::Bytes(DAEMON_LOG_MAX_BYTES),
            file_rotate::compression::Compression::None,
            None,
        ));
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_ansi(false)
            .with_writer(appender)
            .init();
        return;
    }

    if tui_mode {
        // TUI モード: ファイルに出力（stderr 汚染を防止）
        if let Some(path) = crate::trace_log::log_file_path() {
            let tracing_path = path.with_file_name("tracing.log");
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&tracing_path)
            {
                Ok(file) => {
                    tracing_subscriber::fmt()
                        .with_env_filter(env_filter)
                        .with_target(false)
                        .with_ansi(false)
                        .with_writer(file)
                        .init();
                    return;
                }
                Err(e) => {
                    eprintln!("[vp] tracing ログファイル作成失敗: {e}, stderr にフォールバック");
                }
            }
        }
    }

    // 通常モード: stderr に出力
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();
}

/// vpバイナリのパスを取得
pub fn which_vp() -> Option<std::path::PathBuf> {
    // 1. ~/.cargo/bin/vp
    if let Some(home) = dirs::home_dir() {
        let cargo_path = home.join(".cargo/bin/vp");
        if cargo_path.exists() {
            return Some(cargo_path);
        }
    }

    // 2. /usr/local/bin/vp
    let usr_local = std::path::PathBuf::from("/usr/local/bin/vp");
    if usr_local.exists() {
        return Some(usr_local);
    }

    // 3. PATH経由
    if let Ok(output) = std::process::Command::new("which").arg("vp").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }

    None
}

/// VantagePoint.app のパスを検索
pub fn find_vantage_point_app() -> Option<std::path::PathBuf> {
    // 1. /Applications
    let system_app = std::path::PathBuf::from("/Applications/VantagePoint.app");
    if system_app.exists() {
        return Some(system_app);
    }

    // 2. ~/Applications
    if let Some(home) = dirs::home_dir() {
        let user_app = home.join("Applications/VantagePoint.app");
        if user_app.exists() {
            return Some(user_app);
        }
    }

    // 3. Xcode DerivedData（Xcodeビルド優先）
    if let Some(home) = dirs::home_dir() {
        let derived_data = home.join("Library/Developer/Xcode/DerivedData");
        if let Ok(entries) = derived_data.read_dir() {
            for entry in entries.filter_map(|e| e.ok()) {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("VantagePoint-")
                {
                    let app_path = entry.path().join("Build/Products/Debug/VantagePoint.app");
                    if app_path.exists() {
                        return Some(app_path);
                    }
                }
            }
        }
    }

    // 4. 開発リポジトリ（~/repos/vantage-point-mac/）
    if let Some(home) = dirs::home_dir() {
        let dev_repo_app = home.join("repos/vantage-point-mac/VantagePoint/VantagePoint.app");
        if dev_repo_app.exists() {
            return Some(dev_repo_app);
        }
    }

    None
}
