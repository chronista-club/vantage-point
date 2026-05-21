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
    },
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
        WireCommands::Send {
            url,
            to,
            body,
            from,
            reply_to,
        } => send(&url, &to, &body, &from, reply_to.as_deref()).await,
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

async fn send(url: &str, to: &str, body: &str, from: &str, reply_to: Option<&str>) -> Result<()> {
    let client = reqwest::Client::new();
    let endpoint = format!("{}/api/wire/send", url.trim_end_matches('/'));

    // wire_send payload: to は配列、 body は任意 JSON。
    // CLI は ad-hoc test 用なので body string を `{"text": ...}` object に wrap して送る。
    let mut payload = serde_json::json!({
        "from": from,
        "to": [to],
        "body": { "text": body },
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
