//! `vp flow` subcommand — dev-flow primitives の CLI 入口
//!
//! ## 概要
//!
//! Conductor × Performer × Memory orchestration の core 操作を CLI から呼ぶための薄い wrapper。
//! lanes portless (doc 27 §3.4.5): 全 operation は Daemon :32000 の repo-proxy ask 経由で repo を
//! 操作する (旧 SP HTTP 直結 `/api/lanes` `/api/tmux/*` `/api/health` を撤去)。 cwd から parent
//! repo path を auto-resolve し、 Daemon handshake の identifier に使う。
//!
//! `vp flow handoff <name> --task-spec <file or '-'>` で新規 performer への atomic 手渡し、
//! `vp flow progress` で parallel work 集約 view を表示。
//!
//! MCP tool (`mcp__vantage-point__flow_handoff` / `flow_progress`) と同じ semantics、
//! 両者は同 dispatch method (`lane_create` / `lanes_list` / `lane_delete` / `tmux_*`) を共有する。

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use std::io::Read;

use crate::commands::process_client::{
    daemon_repo_request, daemon_repo_request_with_timeout, resolve_repo_path_from_target,
};

#[derive(Subcommand, Debug)]
pub enum FlowCommands {
    /// Performer 新規作成 + 初手 wire_send + tmux nudge を atomic に実行
    ///
    /// 失敗時は performer を rollback。 既存 3 step (`vp lane new` + `vp wire send` + `tmux send-keys`)
    /// を 1 call に圧縮。
    Handoff {
        /// Performer name (= slug、 例: 'feat-api')
        name: String,
        /// Task spec の入力元: ファイルパス、 もしくは '-' で stdin
        #[arg(long, short)]
        task_spec: String,
        /// Lane clone する branch (省略時は repo 側で `<git-user>/<sanitized-name>` を auto-derive)
        #[arg(long, short)]
        branch: Option<String>,
        /// Lane Agent: 'claude' (default、 Claude CLI) or 'shell'
        #[arg(long, short)]
        agent: Option<String>,
        /// worktree の分岐元 ref（未 push の local branch も可）。省略時は
        /// performer-files.kdl の base-ref → origin/HEAD → main
        #[arg(long)]
        base: Option<String>,
        /// worker の claude model alias（例: 'opus' / 'sonnet' / 'haiku'）。task 難度に
        /// 合わせて指定。省略時は config の default-lane-model（既定 Opus）
        #[arg(long)]
        model: Option<String>,
        /// 実行モード: 'hitl' (default、 nudge 後応答期待) / 'auto' (nudge 後放置)
        #[arg(long, default_value = "hitl")]
        mode: String,
        /// tmux send-keys nudge を発火しない (= 完全 async)
        #[arg(long)]
        no_nudge: bool,
    },
    /// 現 repo の parallel work 集約 view (= 各 performer の git status + 未読 wire 数)
    Progress {
        /// 出力フォーマット: 'json' (default) / 'table'
        #[arg(long, default_value = "json")]
        format: String,
    },
}

/// Entry point — main.rs から呼び出される (tokio runtime 経由)。
pub async fn run(cmd: FlowCommands) -> Result<()> {
    match cmd {
        FlowCommands::Handoff {
            name,
            task_spec,
            branch,
            agent,
            base,
            model,
            mode,
            no_nudge,
        } => {
            handoff(
                &name, &task_spec, branch, agent, base, model, &mode, !no_nudge,
            )
            .await
        }
        FlowCommands::Progress { format } => progress(&format).await,
    }
}

/// cwd から parent repo path + config を解決（daemon repo-proxy handshake の identifier）。
///
/// 旧 `resolve_sp_base`（repo HTTP base URL）の置換。 repo port を引かず repo path を返すので、
/// repo 未起動でも path は決まる（実際の操作は daemon repo-proxy が repo control channel に forward
/// し、 repo 不在なら daemon 側で error になる）。
///
/// `resolve_repo_path_from_target` は内部で `find_for_cwd_blocking`（= `make_runtime().block_on`）
/// を踏むので、 async context (handoff / progress) から直呼びすると nested-runtime panic になる。
/// `spawn_blocking` で blocking thread に逃がして同期解決する。
async fn resolve_repo() -> Result<(String, crate::config::Config)> {
    tokio::task::spawn_blocking(|| {
        let config = crate::config::Config::load().unwrap_or_default();
        let path = resolve_repo_path_from_target(None, &config)?;
        Ok::<_, anyhow::Error>((path, config))
    })
    .await
    .context("resolve_repo task join")?
}

/// stdin か file から task_spec を読む。 '-' は stdin、 それ以外はファイル path。
fn read_task_spec(arg: &str) -> Result<String> {
    if arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("task_spec を stdin から読み込み失敗")?;
        Ok(buf)
    } else {
        std::fs::read_to_string(arg)
            .with_context(|| format!("task_spec ファイル読み込み失敗: {}", arg))
    }
}

/// handoff orchestration: repo 経由で performer 作成 → wire_send → nudge を atomic に
///
/// 引数は `vp flow handoff` の CLI flag をそのまま流す薄い passthrough（構造体に束ねる
/// メリットが薄く、 呼び出しは `run` の 1 箇所のみ）。
#[allow(clippy::too_many_arguments)]
async fn handoff(
    name: &str,
    task_spec_arg: &str,
    branch: Option<String>,
    agent: Option<String>,
    base: Option<String>,
    model: Option<String>,
    mode: &str,
    nudge: bool,
) -> Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("name は必須です");
    }
    if mode != "hitl" && mode != "auto" {
        anyhow::bail!("mode は 'hitl' or 'auto' のみ (got: {})", mode);
    }
    let task_spec = read_task_spec(task_spec_arg)?;
    if task_spec.trim().is_empty() {
        anyhow::bail!("task_spec が空です");
    }

    let (repo_path, _config) = resolve_repo().await?;

    // Step 1: Performer 作成 (daemon repo-proxy ask `lane_create`)
    let mut create_body = serde_json::json!({
        "kind": "performer",
        "name": name,
    });
    if let Some(ref b) = branch.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["branch"] = serde_json::Value::String(b.to_string());
    }
    if let Some(ref s) = agent.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["agent"] = serde_json::Value::String(s.to_string());
    }
    if let Some(ref b) = base.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["base"] = serde_json::Value::String(b.to_string());
    }
    if let Some(ref m) = model.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["model"] = serde_json::Value::String(m.to_string());
    }
    // lane_create は repo 側で git clone を含み数 10 sec かかり得るので outer timeout 60s
    // (MCP add_performer/flow_handoff の quic_call_with_timeout と揃える、 orphan lane race 回避)。
    let lane_info = daemon_repo_request_with_timeout(
        crate::cli::daemon_port(),
        &repo_path,
        "lane_create",
        create_body,
        std::time::Duration::from_secs(60),
    )
    .await
    .context("Performer 作成失敗 (lane_create)")?;
    let repo_name = lane_info
        .pointer("/address/repo")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("lane_create response に address.repo がありません"))?
        .to_string();
    let performer_name = lane_info
        .pointer("/address/name")
        .and_then(|v| v.as_str())
        .unwrap_or(name)
        .to_string();
    let cwd = lane_info
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let derived_branch = lane_info
        .pointer("/address/branch")
        .or_else(|| lane_info.get("branch"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let performer_address = format!("agent@{}/{}", repo_name, performer_name);
    let lane_address = format!("{}/performer/{}", repo_name, performer_name);

    // Step 2: wire_send (Daemon "wire" channel 直結、 L0 portless B-4)。 失敗時は performer rollback。
    // `from` は conductor 相当 (= CLI から起動 = conductor context として送信、 qualified address)。
    // `daemon_wire::call` が transport 失敗 / server error frame の両方を Err にするので、 旧 HTTP の
    // 3 分岐 (send / parse / server error) は 1 つに畳まれる (atomic + rollback の意味論は不変)。
    let from = format!("agent@{}", repo_name);
    let send_payload = serde_json::json!({
        "from": from,
        "to": [performer_address.clone()],
        "body": {
            "kind": "task",
            "task_spec": task_spec,
            "mode": mode,
        },
        "reply_to": serde_json::Value::Null,
    });
    let send_json = match crate::repo::daemon_wire::call("/api/wire/send", send_payload).await {
        Ok(j) => j,
        Err(e) => {
            rollback_performer(&repo_path, &repo_name, &performer_name).await;
            anyhow::bail!("wire_send 失敗 (performer rollback 済): {}", e);
        }
    };
    let wire_msg_id = send_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Step 3: nudge (best-effort、 失敗で全体失敗扱いにしない)
    let nudge_status = if nudge {
        try_nudge(&repo_path, &lane_address).await
    } else {
        "off".to_string()
    };

    // 結果を 1 行 JSON で出力 (機械処理しやすく)
    let result = serde_json::json!({
        "performer_address": performer_address,
        "lane_address": lane_address,
        "wire_msg_id": wire_msg_id,
        "performer_dir": cwd,
        "branch": derived_branch,
        "mode": mode,
        "nudge": nudge_status,
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// nudge — lane_nudge proxy で performer の Claude session に wire_recv を促す (best-effort)。
///
/// tmux decoupling PR1: 旧 2 段 (`tmux_resolve_pane` で pane 解決 → `tmux_send_keys` で送信) を
/// daemon repo-proxy ask `lane_nudge` の 1 発に置換。 lane address を直接渡し、 repo 側の
/// `write_nudge` が PtySlot に literal text + Enter を書く (pane 解決の中間層が消える)。
async fn try_nudge(repo_path: &str, lane_address: &str) -> String {
    let nudge_text = "root から task が届いています。 mcp__vantage-point__wire_recv で確認、 内容に従って着手してください。 質問は wire_send + reply_to で thread 返信。\n";
    match daemon_repo_request(
        crate::cli::daemon_port(),
        repo_path,
        "lane_nudge",
        serde_json::json!({ "lane": lane_address, "text": nudge_text }),
    )
    .await
    {
        Ok(_) => "sent".to_string(),
        Err(e) => format!("nudge 失敗 (best-effort): {}", e),
    }
}

/// Rollback: performer 削除 (best-effort、 失敗は stderr に log)。
///
/// lanes portless: 旧 SP HTTP (`DELETE /api/lanes`) を daemon repo-proxy ask (`lane_delete`) に
/// 移管。 `lane_delete` は不在 performer に "Lane not found" を Err で返すので idempotent no-op 扱い。
async fn rollback_performer(repo_path: &str, repo_name: &str, performer_name: &str) {
    let address = format!("{}/performer/{}", repo_name, performer_name);
    match daemon_repo_request(
        crate::cli::daemon_port(),
        repo_path,
        "lane_delete",
        serde_json::json!({ "address": address, "cleanup": true }),
    )
    .await
    {
        Ok(_) => eprintln!("[flow handoff] rollback: performer {} 削除済", address),
        Err(e) if e.to_string().contains("Lane not found") => {
            eprintln!(
                "[flow handoff] rollback: performer {} は既に gone (no-op)",
                address
            );
        }
        Err(e) => {
            eprintln!(
                "[flow handoff] rollback 失敗: performer {} は残置されています ({})",
                address, e
            );
        }
    }
}

/// progress — 現 repo の parallel work 集約 view を返す
async fn progress(format: &str) -> Result<()> {
    if format != "json" && format != "table" {
        anyhow::bail!("format は 'json' or 'table' のみ (got: {})", format);
    }
    let (repo_path, config) = resolve_repo().await?;
    // repo 名は config 登録名を SSOT とする (basename ではない)。 旧 `/api/health.repo_dir`
    // basename 導出は撤去 (lanes portless)。 rename 時の wire identity mismatch を避けるため MCP
    // flow_progress / list_lanes と同型 (repo_name_from_path)。
    let repo = crate::resolve::repo_name_from_path(&repo_path, &config);

    // lanes (performer_status 込み) を取得 (daemon repo-proxy ask `lanes_list`)
    let lanes_resp = daemon_repo_request(
        crate::cli::daemon_port(),
        &repo_path,
        "lanes_list",
        serde_json::json!({}),
    )
    .await
    .context("lanes_list 失敗")?;
    let lanes_in = lanes_resp
        .get("lanes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut performers: Vec<serde_json::Value> = Vec::new();
    let mut conductor_unread: u64 = 0;
    for lane in lanes_in {
        let kind = lane
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let label = if kind == "root" {
            "root".to_string()
        } else {
            lane.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string()
        };
        let agent_addr = if kind == "root" {
            format!("agent@{}", repo)
        } else {
            format!("agent@{}/{}", repo, label)
        };

        // unread count (cursor 不触り、 Daemon "wire" channel 直結。 best-effort: 失敗は 0)
        let unread_total = match crate::repo::daemon_wire::call(
            "/api/wire/unread-count",
            serde_json::json!({ "agent": agent_addr }),
        )
        .await
        {
            Ok(j) => j.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
            Err(_) => 0,
        };

        if kind == "root" {
            conductor_unread = unread_total;
            continue;
        }

        let state = lane
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cwd = lane.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        let performer_status = lane
            .get("performer_status")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // 5-state FSM derive (= conductor 説示 control surrender model)。
        // wire_latest_msg + performer_status から flow_state / control_surrender / state_reason を推論。
        // Daemon "wire" channel 直結 (best-effort: 失敗は None → FSM は performer_status のみで derive)。
        let latest_view = match crate::repo::daemon_wire::call(
            "/api/wire/latest-msg",
            serde_json::json!({ "agent": agent_addr }),
        )
        .await
        {
            Ok(j) => j
                .get("message")
                .and_then(crate::flow::LatestMsgView::from_json),
            Err(_) => None,
        };
        let performer_status_view = crate::flow::PerformerStatusView::from_json(&performer_status);
        // AwaitingUser 判定: 未 ack needs_user (best-effort、 失敗は None = 判定 off で degrade)
        let needs_user_view = match crate::repo::daemon_wire::call(
            "/api/wire/needs-user-pending",
            serde_json::json!({ "agent": agent_addr }),
        )
        .await
        {
            Ok(j) => j
                .get("message")
                .and_then(crate::flow::LatestMsgView::from_json),
            Err(_) => None,
        };
        let fsm = crate::flow::derive_flow_state(
            latest_view.as_ref(),
            performer_status_view,
            &agent_addr,
            needs_user_view.as_ref(),
        );

        performers.push(serde_json::json!({
            "name": label,
            "address": agent_addr,
            "state": state,
            "cwd": cwd,
            "performer_status": performer_status,
            "unread_wire_count": unread_total,
            "flow_state": fsm.state,
            "control_surrender": fsm.control_surrender,
            "state_reason": fsm.state_reason,
            "last_state_transition_at": fsm.last_state_transition_at,
        }));
    }

    let result = serde_json::json!({
        "repo": repo,
        "root": {
            "address": format!("agent@{}", repo),
            "unread_wire_count": conductor_unread,
        },
        "performers": performers,
    });

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_table(&result);
    }
    Ok(())
}

/// `--format table` の簡易テーブル出力 (機械処理向けじゃない、 human 用)
fn print_table(view: &serde_json::Value) {
    let repo = view.get("repo").and_then(|v| v.as_str()).unwrap_or("?");
    let conductor_unread = view
        .pointer("/root/unread_wire_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    println!("Repo: {}", repo);
    println!("  Conductor unread wire: {}", conductor_unread);
    let mut performers = view
        .get("performers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if performers.is_empty() {
        println!("  (no performers)");
        return;
    }
    // needs-you (awaiting_user) を先頭に浮かせる (= ユーザが見るべき行を最初に。 stable sort
    // なので残りの順序は不変)
    performers
        .sort_by_key(|w| w.get("flow_state").and_then(|v| v.as_str()) != Some("awaiting_user"));
    let needs_you = performers
        .iter()
        .filter(|w| w.get("flow_state").and_then(|v| v.as_str()) == Some("awaiting_user"))
        .count();
    if needs_you > 0 {
        println!(
            "  🙋 needs-you: {} performer(s) がユーザの回答待ち",
            needs_you
        );
    }
    println!();
    println!(
        "{:<24} {:<10} {:<18} {:>7} {:>7} {:>7} {:>7} BRANCH",
        "PERFORMER", "STATE", "MODE", "AHEAD", "BEHIND", "DIRTY", "UNREAD"
    );
    for w in performers {
        let name = w.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let state = w.get("state").and_then(|v| v.as_str()).unwrap_or("?");
        let unread = w
            .get("unread_wire_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let ahead = w
            .pointer("/performer_status/ahead")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let behind = w
            .pointer("/performer_status/behind")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let dirty = w
            .pointer("/performer_status/dirty_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let branch = w
            .pointer("/performer_status/branch")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        // flow_state を emoji label に変換 (serde 経由で FlowState に戻す — 手書き match は
        // 状態追加時の漏れ源 (awaiting_user で発覚) なので撤去。 FSM 未 derive は "-")
        let mode_label = w
            .get("flow_state")
            .cloned()
            .and_then(|v| serde_json::from_value::<crate::flow::FlowState>(v).ok())
            .map(|s| s.label())
            .unwrap_or("-");
        println!(
            "{:<24} {:<10} {:<18} {:>7} {:>7} {:>7} {:>7} {}",
            name, state, mode_label, ahead, behind, dirty, unread, branch
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_task_spec_from_file_returns_contents() {
        let dir = std::env::temp_dir().join(format!("flow-tspec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spec.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# hello\nbody").unwrap();
        let got = read_task_spec(path.to_str().unwrap()).unwrap();
        assert!(got.contains("# hello"));
        assert!(got.contains("body"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_task_spec_from_missing_file_errors() {
        let err = read_task_spec("/no/such/path/xyz").unwrap_err();
        assert!(err.to_string().contains("task_spec ファイル読み込み失敗"));
    }

    /// table 出力は repo + performers の最低限を含む (= flow_state MODE column 込み)
    #[test]
    fn print_table_smoke() {
        // smoke: panic しないことだけ確認 (stdout は捕捉しない、 simple coverage)
        let v = serde_json::json!({
            "repo": "demo",
            "root": { "address": "agent@demo", "unread_wire_count": 0 },
            "performers": [{
                "name": "feat-a",
                "address": "agent@demo/feat-a",
                "state": "Running",
                "cwd": "/tmp/performer",
                "performer_status": {
                    "branch": "mako/feat-a",
                    "ahead": 1,
                    "behind": 0,
                    "dirty_count": 2,
                    "has_upstream": true,
                    "last_commit": "abc init",
                    "is_merged": false,
                },
                "unread_wire_count": 3,
                "flow_state": "hitl_pending",
                "control_surrender": false,
                "state_reason": "performer posted question, awaiting root reply",
                "last_state_transition_at": 1_000_000_000_000_i64,
            }, {
                // awaiting_user: serde 経由の label 変換 + needs-you 先頭 sort の経路を踏む
                "name": "feat-b",
                "address": "agent@demo/feat-b",
                "state": "Running",
                "cwd": "/tmp/performer-b",
                "performer_status": null,
                "unread_wire_count": 0,
                "flow_state": "awaiting_user",
                "control_surrender": false,
                "state_reason": "performer posted needs_user, awaiting the user's answer (unacked)",
                "last_state_transition_at": 1_000_000_000_001_i64,
            }]
        });
        print_table(&v);
    }
}
