//! `vp wire` subcommand — wire accumulation messaging の CLI 入口 (wiremsg R5-2)
//!
//! ## 概要
//!
//! SP の `/api/wire/recv` を long-poll loop で叩き、 受信した wire message を 1 行 JSON で
//! stdout に出力する。 Claude Code Monitor tool の subscription source として活用される
//! (Monitor は stdout-emitting 何でも push channel になる、 universal subscription primitive)。
//!
//! 旧 `vp mailbox` の wire 版置換。 msgbox (msgs table) ではなく wire accumulation
//! (per-agent cursor recv) に rewire されている。
//!
//! ## 使い方
//!
//! ```bash
//! # wire address を指定して watch
//! vp wire watch --agent agent@vantage-point
//!
//! # SP base URL を明示
//! vp wire watch --url http://127.0.0.1:33002 --agent agent@vantage-point
//!
//! # message 送信 (ad-hoc test 用)
//! vp wire send --to agent@vantage-point --body "hello"
//!
//! # 1-shot 受信 (= mcp__wire_recv 等価、 long-poll 1 回で抜ける)
//! vp wire recv --agent agent@vantage-point --timeout 5
//! ```
//!
//! 各 wire message が 1 行 JSON で stdout に flush される。 Claude Code 側で:
//!
//! ```text
//! Monitor: vp wire watch --agent agent@vantage-point
//! ```
//!
//! と仕掛ければ、 message 到着のたびに agent chat に notification として届く。

use anyhow::{Context, Result};
use clap::Subcommand;
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum WireCommands {
    /// SP の wire accumulation を long-poll で watch、 受信 message を 1 行 JSON で stdout に出す
    ///
    /// Claude Code Monitor の subscription source として使う想定。 SIGTERM / Ctrl-C で graceful exit。
    Watch {
        /// SP の base URL (例: http://127.0.0.1:33002)。 default は Project 0 の SP (33000)。
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 受信先 wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
        /// 各 long-poll の timeout 秒数 (server 側 max 30、 default 25)
        #[arg(short, long, default_value_t = 25)]
        timeout: u64,
    },
    /// SP の wire accumulation を 1-shot で受信 (= mcp__wire_recv の CLI pair)。
    ///
    /// `watch` と異なり loop しない: long-poll を 1 回だけ実行し、 受信した message 一式を
    /// 1 行 JSON ({"messages":[...], "count":N}) で stdout に出して即 exit。
    /// 既読 cursor が進む点は MCP wire_recv と同じ (= 同 message が再配信されない)。
    /// script / one-off check 用。 continuous な subscription は `watch` を使う。
    Recv {
        /// SP の base URL (例: http://127.0.0.1:33002)。 default は Project 0 の SP (33000)。
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 受信先 wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
        /// long-poll の timeout 秒数 (server 側 max 30、 default 5、 mcp__wire_recv と同 default)
        #[arg(short, long, default_value_t = 5)]
        timeout: u64,
    },
    /// SP の wire accumulation に message を送信 (ad-hoc test 用)
    Send {
        /// SP の base URL
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 宛先 wire address (例: `agent@vantage-point`)
        #[arg(short, long)]
        to: String,
        /// 送信 body (string)
        #[arg(short, long)]
        body: String,
        /// 送信元 (default: "vp-cli")
        #[arg(short, long, default_value = "vp-cli")]
        from: String,
        /// reply 先 message id (指定時は新規 thread の root ではなく reply として送信)
        #[arg(short, long)]
        reply_to: Option<String>,
        /// delivery policy selector (command|event|state|data|log)。
        /// command は受信者が ack するまで delivery loop の再掲示対象 (R2-b)
        #[arg(long)]
        category: Option<String>,
    },
    /// 未読の在庫確認 (= mcp__wire_inbox の CLI pair、 read-only で cursor 不触り)
    ///
    /// `recv` と異なり cursor を進めない: 「読まずに在庫だけ確認」する。
    /// `{status, total, by_thread: {root_id: count}}` を pretty JSON で stdout に出す。
    Inbox {
        /// SP の base URL (例: http://127.0.0.1:33002)。 default は Project 0 の SP (33000)。
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 確認する wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
    },
    /// thread 系譜の取得 (= mcp__wire_thread の CLI pair、 read-only で cursor 不触り)
    ///
    /// 指定 message から prev を root まで辿った系譜 (root-first・chronological) を返す。
    /// 途中参加 thread の backlog 確認用。
    Thread {
        /// SP の base URL。 default は Project 0 の SP (33000)。
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 系譜を辿る起点 message id (recv / inbox で得た id)
        #[arg(short, long)]
        message_id: String,
    },
    /// message の ack (R2-a、 command category の受領確認。 = mcp__wire_ack の CLI pair)
    ///
    /// cursor とは独立の ack 台帳に記録する。 command の処理を終えたら ack すること
    /// (未 ack の command は delivery loop (R2-b) の再掲示対象になる)。
    Ack {
        /// SP の base URL。 default は Project 0 の SP (33000)。
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// ack する message id
        #[arg(short, long)]
        message_id: String,
        /// ack する agent address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
    },
    /// claude hook 実体 (R2-c、 チャネル B): stdin の hook JSON を読み、 未読 wire が
    /// あれば additionalContext を stdout に出す。 echoes spawn が --settings で注入する
    /// (決定 D2)。 あらゆる失敗は silent 成功 (fail-open、 会話を邪魔しない)。
    HookCheck,
    /// shell-level supervisor: vp wire watch を loop で再起動。 inner watch が exit しても
    /// auto-restart で監視を継続する (lifecycle resilience)。 Monitor の前段に置いて、
    /// SessionStart hook 等から自動 arm する想定。
    WatchSupervised {
        /// SP の base URL (default: Project 0 の SP)
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 受信先 wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
        /// 各 long-poll の timeout 秒数
        #[arg(short, long, default_value_t = 25)]
        timeout: u64,
        /// inner watch exit 後の re-spawn 待機秒数
        #[arg(long, default_value_t = 2)]
        restart_delay: u64,
    },
}

/// Entry point — main.rs から呼び出される。
pub async fn run(cmd: WireCommands) -> Result<()> {
    match cmd {
        WireCommands::Watch {
            url,
            agent,
            timeout,
        } => watch(&url, &agent, timeout).await,
        WireCommands::Recv {
            url,
            agent,
            timeout,
        } => recv(&url, &agent, timeout).await,
        WireCommands::Send {
            url,
            to,
            body,
            from,
            reply_to,
            category,
        } => {
            send(
                &url,
                &to,
                &body,
                &from,
                reply_to.as_deref(),
                category.as_deref(),
            )
            .await
        }
        WireCommands::Inbox { url, agent } => inbox(&url, &agent).await,
        WireCommands::Thread { url, message_id } => thread(&url, &message_id).await,
        WireCommands::Ack {
            url,
            message_id,
            agent,
        } => ack(&url, &message_id, &agent).await,
        WireCommands::HookCheck => hook_check().await,
        WireCommands::WatchSupervised {
            url,
            agent,
            timeout,
            restart_delay,
        } => watch_supervised(&url, &agent, timeout, restart_delay).await,
    }
}

/// Supervisor: watch loop の auto-restart wrapper。
/// inner watch が exit するたびに log + sleep + 再 spawn。 Ctrl-C で wrapper ごと停止。
async fn watch_supervised(
    url: &str,
    agent: &str,
    timeout_secs: u64,
    restart_delay_secs: u64,
) -> Result<()> {
    let mut iteration = 0u64;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        iteration += 1;
        eprintln!(
            "[vp wire watch-supervised] iteration={} starting watch (url={}, agent={}, timeout={}s)",
            iteration, url, agent, timeout_secs
        );

        let watch_fut = watch(url, agent, timeout_secs);
        tokio::pin!(watch_fut);

        tokio::select! {
            _ = &mut ctrl_c => {
                eprintln!("[vp wire watch-supervised] ctrl-c received, exiting (no restart)");
                return Ok(());
            }
            result = &mut watch_fut => {
                match result {
                    Ok(()) => {
                        // Inner watch exited cleanly (e.g., its own ctrl-c handler ran)。
                        // 通常は inner watch は ctrl-c でしか exit しないので、 wrapper も止める。
                        eprintln!("[vp wire watch-supervised] inner watch exited cleanly, stopping supervisor");
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!(
                            "[vp wire watch-supervised] inner watch failed: {} (restart in {}s)",
                            e, restart_delay_secs
                        );
                    }
                }
            }
        }

        // Restart wait — Ctrl-C 受け取れるよう select で待つ
        let sleep = tokio::time::sleep(Duration::from_secs(restart_delay_secs));
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut ctrl_c => {
                eprintln!("[vp wire watch-supervised] ctrl-c during restart wait, exiting");
                return Ok(());
            }
            _ = &mut sleep => {}
        }
    }
}

async fn watch(url: &str, agent: &str, timeout_secs: u64) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs + 5)) // server timeout + buffer
        .build()
        .context("reqwest client")?;
    let endpoint = format!("{}/api/wire/recv", url.trim_end_matches('/'));

    eprintln!(
        "[vp wire watch] subscribed to {} (agent={}, timeout={}s)",
        endpoint, agent, timeout_secs
    );

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut consecutive_errors = 0u32;

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                eprintln!("[vp wire watch] ctrl-c received, exiting");
                return Ok(());
            }
            result = poll_recv(&client, &endpoint, agent, timeout_secs) => {
                match result {
                    Ok(messages) => {
                        // 受信 message を 1 行 JSON ずつ stdout に出す (Monitor downstream 用に line-buffered)。
                        // {messages:[], count:0} は no-op。
                        for msg_json in &messages {
                            let line = serde_json::to_string(msg_json)
                                .unwrap_or_else(|_| "{\"error\":\"json serialize\"}".into());
                            println!("{}", line);
                        }
                        if !messages.is_empty() {
                            use std::io::Write;
                            let _ = std::io::stdout().flush();
                        }
                        consecutive_errors = 0;
                    }
                    Err(e) => {
                        consecutive_errors += 1;
                        eprintln!(
                            "[vp wire watch] recv error ({}/3): {}",
                            consecutive_errors, e
                        );
                        if consecutive_errors >= 3 {
                            eprintln!("[vp wire watch] 3 consecutive errors, sleeping 5s before retry");
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            consecutive_errors = 0;
                        } else {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            }
        }
    }
}

/// 1-shot recv (= mcp__wire_recv 等価)。 long-poll を 1 回実行し、 受信した messages
/// (timeout 時は空配列) を `{"messages":[...],"count":N}` 形式で stdout に 1 行 JSON で出す。
///
/// `watch` と異なり loop しない: 即 exit する。 server timeout (max 30s) を超える待機はせず、
/// 0 message でも 1 行 `{"messages":[],"count":0}` を出して exit 0 を返す。
///
/// 既読 cursor は server 側で `watch` と共通 (= 同 agent address の wire を `watch` も `recv` も
/// 同じ cursor で読む)。
async fn recv(url: &str, agent: &str, timeout_secs: u64) -> Result<()> {
    // server 側 timeout 上限 (= /api/wire/recv handler: 30s 上限) は client 側でも clamp。
    // mcp__wire_recv と同じ semantic で揃える。
    let clamped_timeout = timeout_secs.min(30);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(clamped_timeout + 5)) // server timeout + buffer
        .build()
        .context("reqwest client")?;
    let endpoint = format!("{}/api/wire/recv", url.trim_end_matches('/'));

    let messages = poll_recv(&client, &endpoint, agent, clamped_timeout).await?;
    let count = messages.len();
    let payload = serde_json::json!({
        "messages": messages,
        "count": count,
    });
    println!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|_| "{\"messages\":[],\"count\":0}".into())
    );
    Ok(())
}

/// 1 回の long-poll。 受信した wire message の配列を返す (timeout 時は空 vec)。
async fn poll_recv(
    client: &reqwest::Client,
    endpoint: &str,
    agent: &str,
    timeout_secs: u64,
) -> Result<Vec<serde_json::Value>> {
    let body = serde_json::json!({
        "agent": agent,
        "timeout": timeout_secs,
    });

    let resp = client
        .post(endpoint)
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    // error response (handler が {"status":"error", "error":...} を返した場合) を bail。
    if resp.get("status").and_then(|v| v.as_str()) == Some("error") {
        let err = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("recv server error: {}", err);
    }

    // {messages:[...], count} → messages 配列を返す。 timeout 時は空。
    let messages = resp
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(messages)
}

/// read-only POST 系 endpoint (inbox / thread / ack) の共通実行部。
/// 応答を pretty JSON で stdout に出し、 `{"status":"error"}` は bail する。
async fn post_and_print(url: &str, path: &str, payload: serde_json::Value) -> Result<()> {
    let endpoint = format!("{}{}", url.trim_end_matches('/'), path);
    let resp = reqwest::Client::new()
        .post(&endpoint)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("POST {}", endpoint))?
        .json::<serde_json::Value>()
        .await
        .context("response JSON parse")?;
    if resp.get("status").and_then(|v| v.as_str()) == Some("error") {
        let err = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("server error: {}", err);
    }
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

/// POST /api/wire/unread-count — 未読在庫を表示 (cursor 不触り)
async fn inbox(url: &str, agent: &str) -> Result<()> {
    post_and_print(
        url,
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// POST /api/wire/thread — 系譜 (root-first) を表示 (cursor 不触り)
async fn thread(url: &str, message_id: &str) -> Result<()> {
    post_and_print(
        url,
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

/// VP_PROJECT / VP_LANE の値から自 wire address を導出する (純関数、 R2-c)
///
/// conductor → `agent@<project>`、 performer → `agent@<project>/<name>`
/// (echoes task の lane_label と一致: conductor / performer 名 / unnamed)。
/// env 不足/空 = VP 外で起動された claude → None (hook は何もしない)。
fn wire_address_from_env(project: Option<&str>, lane: Option<&str>) -> Option<String> {
    let project = project.filter(|s| !s.is_empty())?;
    let lane = lane.filter(|s| !s.is_empty())?;
    if lane == "conductor" {
        Some(format!("agent@{project}"))
    } else {
        Some(format!("agent@{project}/{lane}"))
    }
}

/// 未読件数から hookSpecificOutput JSON を作る (純関数、 R2-c)。 未読 0 は None (出力なし)。
fn build_hook_output(event_name: &str, total: u64) -> Option<String> {
    if total == 0 {
        return None;
    }
    let context = format!(
        "📬 wire: {total} 件未読。 mcp__vantage-point__wire_recv で受信してください \
         (command category は処理後に mcp__vantage-point__wire_ack)。"
    );
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": context,
            }
        })
        .to_string(),
    )
}

/// hook 実体 (R2-c、 チャネル B): 設計 mem_1CbvcJj4ppU3QKH9d7xMpT。
///
/// 全エラー path で Ok(()) を返し何も出力しない (fail-open) — hook の失敗で
/// 会話を邪魔しないことが最優先。 TheWorld 直叩き (qualified address を自前導出
/// するので SP proxy 不要 = 自 SP が落ちていても未読通知は出る)。
async fn hook_check() -> Result<()> {
    // stdin の hook JSON から event 名を取る (parse 失敗は fail-open で default)
    let mut input = String::new();
    use std::io::Read;
    let _ = std::io::stdin().read_to_string(&mut input);
    let event_name = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|e| e.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "UserPromptSubmit".to_string());

    let Some(agent) = wire_address_from_env(
        std::env::var("VP_PROJECT").ok().as_deref(),
        std::env::var("VP_LANE").ok().as_deref(),
    ) else {
        return Ok(()); // VP 外で起動された claude — 何もしない
    };

    // hook は会話を block しないよう短い timeout。 daemon 不在等は silent 成功。
    let world_port = crate::config::Config::load()
        .map(|c| c.port_layout().world_port)
        .unwrap_or(crate::cli::WORLD_PORT);
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return Ok(());
    };
    let resp = client
        .post(format!(
            "http://127.0.0.1:{world_port}/api/wire/unread-count"
        ))
        .json(&serde_json::json!({ "agent": agent }))
        .send()
        .await;
    let total = match resp {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
            .unwrap_or(0),
        Err(_) => return Ok(()), // daemon 不在 — silent 成功 (fail-open)
    };

    if let Some(out) = build_hook_output(&event_name, total) {
        println!("{out}");
    }
    Ok(())
}

/// POST /api/wire/ack — message を ack する (R2-a、 ack 台帳に記録)
async fn ack(url: &str, message_id: &str, agent: &str) -> Result<()> {
    post_and_print(
        url,
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

async fn send(
    url: &str,
    to: &str,
    body: &str,
    from: &str,
    reply_to: Option<&str>,
    category: Option<&str>,
) -> Result<()> {
    let client = reqwest::Client::new();
    let endpoint = format!("{}/api/wire/send", url.trim_end_matches('/'));

    // wire_send payload: to は配列、 body は任意 JSON。
    // CLI は ad-hoc test 用なので body string を `{"text": ...}` object に wrap して送る。
    // R2-b: --category は delivery policy selector (command = ack されるまで再掲示対象)。
    let mut body_obj = serde_json::json!({ "text": body });
    if let Some(cat) = category {
        body_obj["category"] = serde_json::Value::String(cat.to_string());
    }
    let mut payload = serde_json::json!({
        "from": from,
        "to": [to],
        "body": body_obj,
    });
    if let Some(prev_id) = reply_to {
        payload["reply_to"] = serde_json::Value::String(prev_id.to_string());
    }

    let resp = client
        .post(&endpoint)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("send POST {}", endpoint))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("send failed: HTTP {} — {}", status, text);
    }
    println!("{}", text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// 試験用 wrapper: WireCommands を root subcommand として parse する小さい CLI。
    #[derive(Parser, Debug)]
    #[command(name = "vp-wire-test")]
    struct TestCli {
        #[command(subcommand)]
        cmd: WireCommands,
    }

    #[test]
    fn recv_requires_agent() {
        // --agent なしは clap が rejection するはず。
        let r = TestCli::try_parse_from(["vp-wire-test", "recv"]);
        assert!(r.is_err(), "expected error when --agent omitted");
    }

    #[test]
    fn recv_default_timeout_is_5() {
        // mcp__wire_recv と同 default (= timeout 5s)。
        let cli =
            TestCli::try_parse_from(["vp-wire-test", "recv", "--agent", "agent@vantage-point"])
                .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Recv { timeout, agent, .. } => {
                assert_eq!(timeout, 5, "default timeout should be 5s");
                assert_eq!(agent, "agent@vantage-point");
            }
            other => panic!("expected Recv variant, got {:?}", other),
        }
    }

    #[test]
    fn recv_accepts_explicit_timeout() {
        let cli = TestCli::try_parse_from([
            "vp-wire-test",
            "recv",
            "--agent",
            "agent@vantage-point",
            "--timeout",
            "10",
        ])
        .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Recv { timeout, .. } => assert_eq!(timeout, 10),
            other => panic!("expected Recv variant, got {:?}", other),
        }
    }

    #[test]
    fn recv_default_url_points_to_project0_sp() {
        // Project 0 の SP (33000) が default、 watch / send と同 default に揃える。
        let cli = TestCli::try_parse_from(["vp-wire-test", "recv", "--agent", "x"])
            .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Recv { url, .. } => assert_eq!(url, "http://127.0.0.1:33000"),
            other => panic!("expected Recv variant, got {:?}", other),
        }
    }

    /// timeout 上限 (server 30s) を超える値を渡しても clap parse は通り (= 機能要件)、
    /// 実行時に clamp される (recv() impl 内 `.min(30)`)。 ここでは parse の通過だけ確認。
    #[test]
    fn recv_parses_oversized_timeout_relies_on_runtime_clamp() {
        let cli =
            TestCli::try_parse_from(["vp-wire-test", "recv", "--agent", "x", "--timeout", "300"])
                .expect("parse should succeed (clamp 是 runtime)");
        match cli.cmd {
            WireCommands::Recv { timeout, .. } => assert_eq!(timeout, 300),
            other => panic!("expected Recv variant, got {:?}", other),
        }
    }

    /// R2-a CLI parity: inbox は agent 必須、 url default は SP (proxy 経由)
    #[test]
    fn inbox_parses_agent_with_default_url() {
        let cli = TestCli::try_parse_from(["vp-wire-test", "inbox", "-a", "agent@vp"])
            .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Inbox { url, agent } => {
                assert_eq!(agent, "agent@vp");
                assert_eq!(url, "http://127.0.0.1:33000");
            }
            other => panic!("expected Inbox variant, got {:?}", other),
        }
        assert!(
            TestCli::try_parse_from(["vp-wire-test", "inbox"]).is_err(),
            "--agent omitted should fail"
        );
    }

    /// R2-a CLI parity: thread は message_id 必須
    #[test]
    fn thread_parses_message_id() {
        let cli = TestCli::try_parse_from(["vp-wire-test", "thread", "-m", "0196-abc"])
            .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Thread { message_id, .. } => assert_eq!(message_id, "0196-abc"),
            other => panic!("expected Thread variant, got {:?}", other),
        }
    }

    /// R2-b: --category が Send に渡る
    #[test]
    fn send_parses_category() {
        let cli = TestCli::try_parse_from([
            "vp-wire-test",
            "send",
            "-t",
            "agent@vp",
            "-b",
            "x",
            "--category",
            "command",
        ])
        .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Send { category, .. } => {
                assert_eq!(category.as_deref(), Some("command"))
            }
            other => panic!("expected Send variant, got {:?}", other),
        }
    }

    /// R2-a CLI parity: ack は message_id + agent の両方必須
    #[test]
    fn ack_parses_message_id_and_agent() {
        let cli =
            TestCli::try_parse_from(["vp-wire-test", "ack", "-m", "0196-abc", "-a", "agent@vp"])
                .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Ack {
                message_id, agent, ..
            } => {
                assert_eq!(message_id, "0196-abc");
                assert_eq!(agent, "agent@vp");
            }
            other => panic!("expected Ack variant, got {:?}", other),
        }
        assert!(
            TestCli::try_parse_from(["vp-wire-test", "ack", "-m", "0196-abc"]).is_err(),
            "--agent omitted should fail"
        );
    }

    /// R2-c: VP_PROJECT / VP_LANE から自 wire address を導出
    #[test]
    fn hook_address_from_env_values() {
        assert_eq!(
            wire_address_from_env(Some("vp"), Some("conductor")).as_deref(),
            Some("agent@vp")
        );
        assert_eq!(
            wire_address_from_env(Some("vp"), Some("w1")).as_deref(),
            Some("agent@vp/w1")
        );
        // env 不足 = VP 外で起動された claude → None (fail-open)
        assert_eq!(wire_address_from_env(None, Some("conductor")), None);
        assert_eq!(wire_address_from_env(Some("vp"), None), None);
        assert_eq!(wire_address_from_env(Some(""), Some("conductor")), None);
    }

    /// R2-c: 未読ありのときだけ hookSpecificOutput JSON を作る
    #[test]
    fn hook_output_only_when_unread() {
        assert_eq!(build_hook_output("SessionStart", 0), None);
        let out = build_hook_output("UserPromptSubmit", 3).expect("3 件未読なら出力");
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additionalContext");
        assert!(ctx.contains("3 件"), "件数を含む: {ctx}");
        assert!(ctx.contains("wire_recv"), "受信導線を含む: {ctx}");
    }
}
