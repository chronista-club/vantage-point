//! `vp flow` subcommand — dev-flow primitives の CLI 入口
//!
//! ## 概要
//!
//! Conductor × Performer × Memory orchestration の core 操作を CLI から呼ぶための薄い wrapper。
//! lanes portless (doc 27 §3.4.5): 全 operation は World :32000 の process-proxy ask 経由で SP を
//! 操作する (旧 SP HTTP 直結 `/api/lanes` `/api/tmux/*` `/api/health` を撤去)。 cwd から parent
//! project path を auto-resolve し、 World handshake の identifier に使う。
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
    resolve_project_path_from_target, world_process_request, world_process_request_with_timeout,
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
        /// Lane clone する branch (省略時は SP 側で `<git-user>/<sanitized-name>` を auto-derive)
        #[arg(long, short)]
        branch: Option<String>,
        /// Lane Stand: 'echoes' (default、 Claude CLI) or 'shell'
        #[arg(long, short)]
        stand: Option<String>,
        /// 実行モード: 'hitl' (default、 nudge 後応答期待) / 'auto' (nudge 後放置)
        #[arg(long, default_value = "hitl")]
        mode: String,
        /// tmux send-keys nudge を発火しない (= 完全 async)
        #[arg(long)]
        no_nudge: bool,
    },
    /// 現 project の parallel work 集約 view (= 各 performer の git status + 未読 wire 数)
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
            stand,
            mode,
            no_nudge,
        } => handoff(&name, &task_spec, branch, stand, &mode, !no_nudge).await,
        FlowCommands::Progress { format } => progress(&format).await,
    }
}

/// cwd から parent project path + config を解決（World process-proxy handshake の identifier）。
///
/// 旧 `resolve_sp_base`（SP HTTP base URL）の置換。 SP port を引かず project path を返すので、
/// SP 未起動でも path は決まる（実際の操作は World process-proxy が SP control channel に forward
/// し、 SP 不在なら World 側で error になる）。
///
/// `resolve_project_path_from_target` は内部で `find_for_cwd_blocking`（= `make_runtime().block_on`）
/// を踏むので、 async context (handoff / progress) から直呼びすると nested-runtime panic になる。
/// `spawn_blocking` で blocking thread に逃がして同期解決する。
async fn resolve_project() -> Result<(String, crate::config::Config)> {
    tokio::task::spawn_blocking(|| {
        let config = crate::config::Config::load().unwrap_or_default();
        let path = resolve_project_path_from_target(None, &config)?;
        Ok::<_, anyhow::Error>((path, config))
    })
    .await
    .context("resolve_project task join")?
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

/// handoff orchestration: SP 経由で performer 作成 → wire_send → nudge を atomic に
async fn handoff(
    name: &str,
    task_spec_arg: &str,
    branch: Option<String>,
    stand: Option<String>,
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

    let (project_path, _config) = resolve_project().await?;

    // Step 1: Performer 作成 (World process-proxy ask `lane_create`)
    let mut create_body = serde_json::json!({
        "kind": "performer",
        "name": name,
    });
    if let Some(ref b) = branch.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["branch"] = serde_json::Value::String(b.to_string());
    }
    if let Some(ref s) = stand.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["stand"] = serde_json::Value::String(s.to_string());
    }
    // lane_create は SP 側で git clone を含み数 10 sec かかり得るので outer timeout 60s
    // (MCP add_performer/flow_handoff の quic_call_with_timeout と揃える、 orphan lane race 回避)。
    let lane_info = world_process_request_with_timeout(
        crate::cli::WORLD_PORT,
        &project_path,
        "lane_create",
        create_body,
        std::time::Duration::from_secs(60),
    )
    .await
    .context("Performer 作成失敗 (lane_create)")?;
    let project_name = lane_info
        .pointer("/address/project")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("lane_create response に address.project がありません"))?
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

    let performer_address = format!("agent@{}/{}", project_name, performer_name);
    let lane_address = format!("{}/performer/{}", project_name, performer_name);

    // Step 2: wire_send (World "wire" channel 直結、 L0 portless B-4)。 失敗時は performer rollback。
    // `from` は conductor 相当 (= CLI から起動 = conductor context として送信、 qualified address)。
    // `world_wire::call` が transport 失敗 / server error frame の両方を Err にするので、 旧 HTTP の
    // 3 分岐 (send / parse / server error) は 1 つに畳まれる (atomic + rollback の意味論は不変)。
    let from = format!("agent@{}", project_name);
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
    let send_json = match crate::process::world_wire::call("/api/wire/send", send_payload).await {
        Ok(j) => j,
        Err(e) => {
            rollback_performer(&project_path, &project_name, &performer_name).await;
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
        try_nudge(&project_path, &lane_address).await
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

/// nudge — tmux send-keys で performer の Claude session に wire_recv を促す (best-effort)。
///
/// lanes portless: 旧 SP HTTP (`/api/tmux/resolve-pane` + `/api/tmux/send-keys`) を World
/// process-proxy ask (`tmux_resolve_pane` + `tmux_send_keys`) に移管。 dispatch の payload shape は
/// resolve=`{query}`→`{pane_id, meta:{label}}` / send=`{pane_id, keys}` (MCP flow_handoff と同型)。
async fn try_nudge(project_path: &str, lane_address: &str) -> String {
    // 1. lane address (project/performer/name) を tmux pane id に resolve
    let resolve_json = match world_process_request(
        crate::cli::WORLD_PORT,
        project_path,
        "tmux_resolve_pane",
        serde_json::json!({ "query": lane_address }),
    )
    .await
    {
        Ok(j) => j,
        Err(e) => return format!("pane resolve 失敗 (best-effort): {}", e),
    };
    let pane_id = match resolve_json.get("pane_id").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => return format!("pane 不在 (best-effort): {}", resolve_json),
    };

    // 2. send-keys で nudge text を送信 (dispatch tmux_send_keys は `keys` field)
    let nudge_text = "conductor から task が届いています。 mcp__vantage-point__wire_recv で確認、 内容に従って着手してください。 質問は wire_send + reply_to で thread 返信。\n";
    match world_process_request(
        crate::cli::WORLD_PORT,
        project_path,
        "tmux_send_keys",
        serde_json::json!({ "pane_id": pane_id, "keys": nudge_text }),
    )
    .await
    {
        Ok(_) => "sent".to_string(),
        Err(e) => format!("send-keys 失敗 (best-effort): {}", e),
    }
}

/// Rollback: performer 削除 (best-effort、 失敗は stderr に log)。
///
/// lanes portless: 旧 SP HTTP (`DELETE /api/lanes`) を World process-proxy ask (`lane_delete`) に
/// 移管。 `lane_delete` は不在 performer に "Lane not found" を Err で返すので idempotent no-op 扱い。
async fn rollback_performer(project_path: &str, project_name: &str, performer_name: &str) {
    let address = format!("{}/performer/{}", project_name, performer_name);
    match world_process_request(
        crate::cli::WORLD_PORT,
        project_path,
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

/// progress — 現 project の parallel work 集約 view を返す
async fn progress(format: &str) -> Result<()> {
    if format != "json" && format != "table" {
        anyhow::bail!("format は 'json' or 'table' のみ (got: {})", format);
    }
    let (project_path, config) = resolve_project().await?;
    // project 名は config 登録名を SSOT とする (basename ではない)。 旧 `/api/health.project_dir`
    // basename 導出は撤去 (lanes portless)。 rename 時の wire identity mismatch を避けるため MCP
    // flow_progress / list_lanes と同型 (project_name_from_path)。
    let project = crate::resolve::project_name_from_path(&project_path, &config);

    // lanes (performer_status 込み) を取得 (World process-proxy ask `lanes_list`)
    let lanes_resp = world_process_request(
        crate::cli::WORLD_PORT,
        &project_path,
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
        let label = if kind == "conductor" {
            "conductor".to_string()
        } else {
            lane.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string()
        };
        let agent_addr = if kind == "conductor" {
            format!("agent@{}", project)
        } else {
            format!("agent@{}/{}", project, label)
        };

        // unread count (cursor 不触り、 World "wire" channel 直結。 best-effort: 失敗は 0)
        let unread_total = match crate::process::world_wire::call(
            "/api/wire/unread-count",
            serde_json::json!({ "agent": agent_addr }),
        )
        .await
        {
            Ok(j) => j.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
            Err(_) => 0,
        };

        if kind == "conductor" {
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
        // World "wire" channel 直結 (best-effort: 失敗は None → FSM は performer_status のみで derive)。
        let latest_view = match crate::process::world_wire::call(
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
        let fsm = crate::flow::derive_flow_state(
            latest_view.as_ref(),
            performer_status_view,
            &agent_addr,
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
        "project": project,
        "conductor": {
            "address": format!("agent@{}", project),
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
    let project = view.get("project").and_then(|v| v.as_str()).unwrap_or("?");
    let conductor_unread = view
        .pointer("/conductor/unread_wire_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    println!("Project: {}", project);
    println!("  Conductor unread wire: {}", conductor_unread);
    let performers = view
        .get("performers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if performers.is_empty() {
        println!("  (no performers)");
        return;
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
        // flow_state を emoji label に変換 (= "idle" → "⏸ idle" 等、 FSM 未 derive の performer は "-")
        let mode_label = w
            .get("flow_state")
            .and_then(|v| v.as_str())
            .and_then(|s| match s {
                "idle" => Some(crate::flow::FlowState::Idle),
                "working" => Some(crate::flow::FlowState::Working),
                "hitl_pending" => Some(crate::flow::FlowState::HitlPending),
                "completed" => Some(crate::flow::FlowState::Completed),
                "stuck" => Some(crate::flow::FlowState::Stuck),
                _ => None,
            })
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

    /// table 出力は project + performers の最低限を含む (= flow_state MODE column 込み)
    #[test]
    fn print_table_smoke() {
        // smoke: panic しないことだけ確認 (stdout は捕捉しない、 simple coverage)
        let v = serde_json::json!({
            "project": "demo",
            "conductor": { "address": "agent@demo", "unread_wire_count": 0 },
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
                "state_reason": "performer posted question, awaiting conductor reply",
                "last_state_transition_at": 1_000_000_000_000_i64,
            }]
        });
        print_table(&v);
    }
}
