//! SP / tmux 起動ユーティリティ
//!
//! `vp sp start` / `vp hd start` 等から共有されるヘルパー関数群。
//! - `spawn_sp_detached()` — SP を detached subprocess として起動
//! - `create_tmux_session()` — Claude CLI 入りの tmux セッション作成
//! - `wait_for_ready()` / `is_server_responding()` / `is_sp_for_project_responding()` — TCP 疎通 / project 照合チェック
//!
//! 旧 `vp start` の TUI 経路（`execute` / `StartOptions` / `resolve_project` / `run_tui` 等）は
//! VP-165 PR-1b で削除（caller ゼロの死コード）。現在の TUI は `tui/app.rs`、port 解決の
//! `resolve_port` は VP-165 PR-5 で TheWorld 経由（`/api/world/port_for`）に統一予定。

use anyhow::Result;

// =============================================================================
// tmux 操作
// =============================================================================

fn tmux_session_exists(name: &str) -> bool {
    std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
        .args(["has-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux セッション作成（Claude CLI を中で起動、ステータスバー非表示）
///
/// `--continue` 付きで起動し、即死した場合は `--continue` なしでフォールバック。
pub fn create_tmux_session(
    name: &str,
    project_dir: &str,
    cols: u16,
    rows: u16,
    process_port: u16,
) -> Result<()> {
    let mut mise_envs = collect_mise_env(project_dir);
    // VP_PROCESS_PORT を注入 → CC が起動する MCP プロセスに自動伝播
    mise_envs.push(("VP_PROCESS_PORT".to_string(), process_port.to_string()));

    // まず --continue 付きで試行
    let created = try_create_tmux_claude(name, project_dir, cols, rows, &mise_envs, true)?;
    if !created {
        anyhow::bail!("tmux セッション作成に失敗: {}", name);
    }

    // セッションが即死していないか確認（claude --continue が壊れたセッションで落ちるケース）
    // lane パフォーマー環境ではセッション履歴がなく --continue が即死するため、十分に待つ
    std::thread::sleep(std::time::Duration::from_millis(1500));
    if !tmux_session_exists(name) {
        tracing::warn!("claude --continue が即死。--continue なしでフォールバック");
        let created = try_create_tmux_claude(name, project_dir, cols, rows, &mise_envs, false)?;
        if !created {
            anyhow::bail!("tmux セッション作成に失敗（フォールバック）: {}", name);
        }
    }

    if !mise_envs.is_empty() {
        tracing::info!("mise env: {} 変数を tmux セッションに注入", mise_envs.len());
    }

    // TUI が自前のヘッダー/フッターを持つため tmux ステータスバーを非表示
    let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
        .args(["set-option", "-t", name, "status", "off"])
        .status();

    // VP-83 refinement 53 / Tier 1 Chain tuning:
    // ─ escape-time 0: Esc 応答即時化 (vi-like TUI 体験改善)
    // ─ focus-events on: terminal focus 変化を Claude CLI 等に forward
    //   → NSWindow become-key で TUI redraw → HD 入力 area の 2 行問題緩和の期待
    // ─ terminal-overrides *:Tc: 24-bit truecolor を 256 色にダウングレードさせない
    let tmux_bin = crate::tmux::tmux_bin().unwrap_or("tmux");
    let _ = std::process::Command::new(tmux_bin)
        .args(["set-option", "-t", name, "escape-time", "0"])
        .status();
    let _ = std::process::Command::new(tmux_bin)
        .args(["set-option", "-t", name, "focus-events", "on"])
        .status();
    let _ = std::process::Command::new(tmux_bin)
        .args(["set-option", "-ga", "terminal-overrides", ",*:Tc"])
        .status();

    // mise 環境変数を tmux セッションにも set-environment（後続ペイン用）
    for (key, value) in &mise_envs {
        let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
            .args(["set-environment", "-t", name, key, value])
            .status();
    }

    Ok(())
}

/// tmux new-session で Claude CLI を起動（成功なら true）
pub fn try_create_tmux_claude(
    name: &str,
    project_dir: &str,
    cols: u16,
    rows: u16,
    mise_envs: &[(String, String)],
    with_continue: bool,
) -> Result<bool> {
    let mut args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        name.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
        "-c".to_string(),
        project_dir.to_string(),
    ];
    for (key, value) in mise_envs {
        args.push("-e".to_string());
        args.push(format!("{}={}", key, value));
    }
    // zsh -lc でラップ: tmux の直接 exec ではシェル初期化が走らず
    // claude が依存する PATH/環境変数が不足して即死するケースを回避
    let mut claude_cmd = "claude --dangerously-skip-permissions".to_string();
    if with_continue {
        claude_cmd.push_str(" --continue");
    }
    args.push("zsh".to_string());
    args.push("-lc".to_string());
    args.push(claude_cmd);

    let status = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
        .args(&args)
        .status()?;

    Ok(status.success())
}

/// mise env を project_dir で評価し、環境変数の (key, value) ペアを返す
///
/// mise が未インストール or .mise.toml がなければ空 Vec を返す（ベストエフォート）。
pub fn collect_mise_env(project_dir: &str) -> Vec<(String, String)> {
    let mise_bin = dirs::home_dir()
        .map(|h| h.join(".local/bin/mise"))
        .unwrap_or_else(|| "mise".into());

    let output = std::process::Command::new(&mise_bin)
        .args(["env", "--shell", "bash"])
        .current_dir(project_dir)
        .output();

    let Ok(output) = output else {
        return vec![];
    };
    if !output.status.success() {
        return vec![];
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // "export KEY=VALUE" → (KEY, VALUE)
            let line = line.strip_prefix("export ")?;
            let (key, value) = line.split_once('=')?;
            // クォート除去
            let value = value.trim_matches('\'').trim_matches('"');
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

// =============================================================================
// SP（Star Platinum）サーバー管理
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
