//! `vp flow` subcommand — dev-flow primitives の CLI 入口
//!
//! ## 概要
//!
//! Lead × Wing × Memory orchestration の core 操作を CLI から呼ぶための薄い wrapper。
//! 全 operation は SP の HTTP endpoint を直接叩く (= QUIC channel 不要)、 cwd から
//! parent project を auto-resolve する。
//!
//! `vp flow handoff <name> --task-spec <file or '-'>` で新規 wing への atomic 手渡し、
//! `vp flow progress` で parallel work 集約 view を表示。
//!
//! MCP tool (`mcp__vantage-point__flow_handoff` / `flow_progress`) と同じ semantics、
//! 両者は同 SP endpoint 経由で composition を実行する。

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use std::io::Read;
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum FlowCommands {
    /// Wing 新規作成 + 初手 wire_send + tmux nudge を atomic に実行
    ///
    /// 失敗時は wing を rollback。 既存 3 step (`vp lane new` + `vp wire send` + `tmux send-keys`)
    /// を 1 call に圧縮。
    Handoff {
        /// Wing name (= slug、 例: 'feat-api')
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
    /// 現 project の parallel work 集約 view (= 各 wing の git status + 未読 wire 数)
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

/// cwd から parent SP の base URL を解決
fn resolve_sp_base() -> Result<String> {
    let process = crate::discovery::find_for_cwd_blocking().ok_or_else(|| {
        anyhow!("parent SP が見つかりません (TheWorld に project 登録済み？ vp start で起動)")
    })?;
    Ok(format!("http://[::1]:{}", process.port))
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

/// handoff orchestration: SP 経由で wing 作成 → wire_send → nudge を atomic に
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

    let base = resolve_sp_base()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("reqwest client build")?;

    // Step 1: Wing 作成 (POST /api/lanes)
    let mut create_body = serde_json::json!({
        "kind": "wing",
        "name": name,
    });
    if let Some(ref b) = branch.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["branch"] = serde_json::Value::String(b.to_string());
    }
    if let Some(ref s) = stand.as_ref().filter(|s| !s.trim().is_empty()) {
        create_body["stand"] = serde_json::Value::String(s.to_string());
    }
    let create_url = format!("{}/api/lanes", base);
    let resp = client
        .post(&create_url)
        .json(&create_body)
        .send()
        .await
        .with_context(|| format!("POST {} 失敗", create_url))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Wing 作成失敗 ({}): {}", status, text);
    }
    let lane_info: serde_json::Value =
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let project_name = lane_info
        .pointer("/address/project")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("/api/lanes response に address.project がありません"))?
        .to_string();
    let wing_name = lane_info
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

    let wing_address = format!("agent@{}/{}", project_name, wing_name);
    let lane_address = format!("{}/wing/{}", project_name, wing_name);

    // Step 2: wire_send (POST /api/wire/send)。 失敗時は wing rollback。
    // `from` は lead 相当 (= CLI から起動 = lead context として送信)。
    let send_url = format!("{}/api/wire/send", base);
    let from = format!("agent@{}", project_name);
    let send_payload = serde_json::json!({
        "from": from,
        "to": [wing_address.clone()],
        "body": {
            "kind": "task",
            "task_spec": task_spec,
            "mode": mode,
        },
        "reply_to": serde_json::Value::Null,
    });
    let send_resp = match client
        .post(&send_url)
        .json(&send_payload)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => r,
        Err(e) => {
            rollback_wing(&client, &base, &project_name, &wing_name).await;
            anyhow::bail!("wire_send 失敗 (wing rollback 済): {}", e);
        }
    };
    let send_json: serde_json::Value = match send_resp.json().await {
        Ok(j) => j,
        Err(e) => {
            rollback_wing(&client, &base, &project_name, &wing_name).await;
            anyhow::bail!("wire_send response parse 失敗 (wing rollback 済): {}", e);
        }
    };
    if send_json.get("status").and_then(|v| v.as_str()) == Some("error") {
        let err = send_json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        rollback_wing(&client, &base, &project_name, &wing_name).await;
        anyhow::bail!("wire_send server error (wing rollback 済): {}", err);
    }
    let wire_msg_id = send_json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Step 3: nudge (best-effort、 失敗で全体失敗扱いにしない)
    let nudge_status = if nudge {
        try_nudge(&client, &base, &lane_address).await
    } else {
        "off".to_string()
    };

    // 結果を 1 行 JSON で出力 (機械処理しやすく)
    let result = serde_json::json!({
        "wing_address": wing_address,
        "lane_address": lane_address,
        "wire_msg_id": wire_msg_id,
        "wing_dir": cwd,
        "branch": derived_branch,
        "mode": mode,
        "nudge": nudge_status,
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// nudge — tmux send-keys で wing の Claude session に wire_recv を促す (best-effort)
async fn try_nudge(client: &reqwest::Client, base: &str, lane_address: &str) -> String {
    // 1. lane address (project/wing/name) を tmux pane id に resolve
    let resolve_url = format!("{}/api/tmux/resolve-pane?q={}", base, lane_address);
    let resolve_resp = match client.get(&resolve_url).send().await {
        Ok(r) => r,
        Err(e) => return format!("pane resolve 失敗 (best-effort): {}", e),
    };
    let resolve_json: serde_json::Value = match resolve_resp.json().await {
        Ok(j) => j,
        Err(e) => return format!("pane resolve parse 失敗 (best-effort): {}", e),
    };
    let pane_id = match resolve_json.get("pane_id").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => return format!("pane 不在 (best-effort): {}", resolve_json),
    };

    // 2. send-keys で nudge text を送信 (HTTP の TmuxSendKeysParams は text + enter)
    let nudge_text = "lead から task が届いています。 mcp__vantage-point__wire_recv で確認、 内容に従って着手してください。 質問は wire_send + reply_to で thread 返信。";
    let send_url = format!("{}/api/tmux/send-keys", base);
    let send_payload = serde_json::json!({
        "pane_id": pane_id,
        "text": nudge_text,
        "enter": true,
    });
    match client.post(&send_url).json(&send_payload).send().await {
        Ok(_) => "sent".to_string(),
        Err(e) => format!("send-keys 失敗 (best-effort): {}", e),
    }
}

/// Rollback: wing 削除 (best-effort、 失敗は stderr に log)
async fn rollback_wing(client: &reqwest::Client, base: &str, project_name: &str, wing_name: &str) {
    let address = format!("{}/wing/{}", project_name, wing_name);
    let address_enc = address.replace('/', "%2F");
    let url = format!("{}/api/lanes?address={}&cleanup=true", base, address_enc);
    match client.delete(&url).send().await {
        Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND => {
            eprintln!("[flow handoff] rollback: wing {} 削除済", address);
        }
        Ok(r) => {
            eprintln!(
                "[flow handoff] rollback 失敗 (HTTP {}): wing {} は残置されています",
                r.status(),
                address
            );
        }
        Err(e) => {
            eprintln!(
                "[flow handoff] rollback HTTP 失敗: wing {} は残置されています ({})",
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
    let base = resolve_sp_base()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("reqwest client build")?;

    // /api/health から project 名
    let health: serde_json::Value = client
        .get(format!("{}/api/health", base))
        .send()
        .await
        .context("GET /api/health 失敗")?
        .json()
        .await
        .context("/api/health parse 失敗")?;
    let project_dir = health
        .get("project_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("/api/health に project_dir なし"))?;
    let project = std::path::Path::new(project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("project_dir basename 取得失敗: {}", project_dir))?
        .to_string();

    // /api/lanes で lanes (wing_status 込み) を取得
    let lanes_resp: serde_json::Value = client
        .get(format!("{}/api/lanes", base))
        .send()
        .await
        .context("GET /api/lanes 失敗")?
        .json()
        .await
        .context("/api/lanes parse 失敗")?;
    let lanes_in = lanes_resp
        .get("lanes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut wings: Vec<serde_json::Value> = Vec::new();
    let mut lead_unread: u64 = 0;
    for lane in lanes_in {
        let kind = lane
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let label = if kind == "lead" {
            "lead".to_string()
        } else {
            lane.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed")
                .to_string()
        };
        let agent_addr = if kind == "lead" {
            format!("agent@{}", project)
        } else {
            format!("agent@{}/{}", project, label)
        };

        // unread count (cursor 不触り)
        let unread_payload = serde_json::json!({ "agent": agent_addr });
        let unread_total = match client
            .post(format!("{}/api/wire/unread-count", base))
            .json(&unread_payload)
            .send()
            .await
        {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(j) => j.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
                Err(_) => 0,
            },
            Err(_) => 0,
        };

        if kind == "lead" {
            lead_unread = unread_total;
            continue;
        }

        let state = lane
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cwd = lane.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        let wing_status = lane
            .get("wing_status")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        wings.push(serde_json::json!({
            "name": label,
            "address": agent_addr,
            "state": state,
            "cwd": cwd,
            "wing_status": wing_status,
            "unread_wire_count": unread_total,
        }));
    }

    let result = serde_json::json!({
        "project": project,
        "lead": {
            "address": format!("agent@{}", project),
            "unread_wire_count": lead_unread,
        },
        "wings": wings,
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
    let lead_unread = view
        .pointer("/lead/unread_wire_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    println!("Project: {}", project);
    println!("  Lead unread wire: {}", lead_unread);
    let wings = view
        .get("wings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if wings.is_empty() {
        println!("  (no wings)");
        return;
    }
    println!();
    println!(
        "{:<24} {:<10} {:>7} {:>7} {:>7} {:>7} {}",
        "WING", "STATE", "AHEAD", "BEHIND", "DIRTY", "UNREAD", "BRANCH"
    );
    for w in wings {
        let name = w.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let state = w.get("state").and_then(|v| v.as_str()).unwrap_or("?");
        let unread = w
            .get("unread_wire_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let ahead = w
            .pointer("/wing_status/ahead")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let behind = w
            .pointer("/wing_status/behind")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let dirty = w
            .pointer("/wing_status/dirty_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let branch = w
            .pointer("/wing_status/branch")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        println!(
            "{:<24} {:<10} {:>7} {:>7} {:>7} {:>7} {}",
            name, state, ahead, behind, dirty, unread, branch
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

    /// table 出力は project + wings の最低限を含む
    #[test]
    fn print_table_smoke() {
        // smoke: panic しないことだけ確認 (stdout は捕捉しない、 simple coverage)
        let v = serde_json::json!({
            "project": "demo",
            "lead": { "address": "agent@demo", "unread_wire_count": 0 },
            "wings": [{
                "name": "feat-a",
                "address": "agent@demo/feat-a",
                "state": "Running",
                "cwd": "/tmp/wing",
                "wing_status": {
                    "branch": "mako/feat-a",
                    "ahead": 1,
                    "behind": 0,
                    "dirty_count": 2,
                    "has_upstream": true,
                    "last_commit": "abc init",
                    "is_merged": false,
                },
                "unread_wire_count": 3,
            }]
        });
        print_table(&v);
    }
}
