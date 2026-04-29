//! `vp mailbox` subcommand — Mailbox actor messaging の CLI 入口 (Phase 1a)
//!
//! ## Phase 1a (今 sprint): `vp mailbox watch <SP_URL>`
//!
//! SP の `/api/msgbox/recv` を long-poll loop で叩き、 受信した message を 1 行 JSON で
//! stdout に出力する。 Claude Code Monitor tool の subscription source として活用される
//! (Monitor は stdout-emitting 何でも push channel になる、 universal subscription primitive)。
//!
//! 関連 memory:
//! - `vp_mailbox_monitor_agent_inbox.md` (本機能の architectural rationale + Phase plan)
//! - `vp_lane_init_script.md` (対の trunk、 init_script で Lane scripted entrypoint 化)
//!
//! ## 使い方
//!
//! ```bash
//! # SP base URL を指定して watch
//! vp mailbox watch --url http://127.0.0.1:33000
//!
//! # default URL (= TheWorld の SP base、 Project 0)
//! vp mailbox watch
//!
//! # Sender filter
//! vp mailbox watch --from claude-mako
//! ```
//!
//! 各 message が 1 行 JSON で stdout に flush される。 Claude Code 側で:
//!
//! ```
//! Monitor: vp mailbox watch
//! ```
//!
//! と仕掛ければ、 message 到着のたびに agent chat に notification として届く。

use anyhow::{Context, Result};
use clap::Subcommand;
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum MailboxCommands {
    /// SP の msgbox を long-poll で watch、 受信 message を 1 行 JSON で stdout に出す
    ///
    /// Claude Code Monitor の subscription source として使う想定。 SIGTERM / Ctrl-C で graceful exit。
    Watch {
        /// SP の base URL (例: http://127.0.0.1:33002)。 default は Project 0 の SP (33000)。
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 各 long-poll の timeout 秒数 (server 側 max 30、 default 25)
        #[arg(short, long, default_value_t = 25)]
        timeout: u64,
        /// Sender 絞り込み (Some なら 該当 from の msg のみ受信)
        #[arg(short, long)]
        from: Option<String>,
    },
    /// SP の msgbox に message を送信 (Phase 1a 補助、 ad-hoc test 用)
    Send {
        /// SP の base URL
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 宛先 actor address (例: `claude-mako@vantage-point`)
        #[arg(short, long)]
        to: String,
        /// 送信 body (string)
        #[arg(short, long)]
        body: String,
        /// 送信元 (default: "vp-cli")
        #[arg(short, long, default_value = "vp-cli")]
        from: String,
    },
    /// shell-level supervisor: vp mailbox watch を loop で再起動。 inner watch が exit しても
    /// auto-restart で監視を継続する (Phase 1b: lifecycle resilience)。 Monitor の前段に置いて、
    /// SessionStart hook 等から自動 arm する想定。
    WatchSupervised {
        /// SP の base URL (default: Project 0 の SP)
        #[arg(short, long, default_value = "http://127.0.0.1:33000")]
        url: String,
        /// 各 long-poll の timeout 秒数
        #[arg(short, long, default_value_t = 25)]
        timeout: u64,
        /// Sender 絞り込み
        #[arg(short, long)]
        from: Option<String>,
        /// inner watch exit 後の re-spawn 待機秒数
        #[arg(long, default_value_t = 2)]
        restart_delay: u64,
    },
}

/// Entry point — main.rs から呼び出される。
pub async fn run(cmd: MailboxCommands) -> Result<()> {
    match cmd {
        MailboxCommands::Watch { url, timeout, from } => watch(&url, timeout, from.as_deref()).await,
        MailboxCommands::Send {
            url,
            to,
            body,
            from,
        } => send(&url, &to, &body, &from).await,
        MailboxCommands::WatchSupervised {
            url,
            timeout,
            from,
            restart_delay,
        } => watch_supervised(&url, timeout, from.as_deref(), restart_delay).await,
    }
}

/// Supervisor: watch loop の auto-restart wrapper (Phase 1b)。
/// inner watch が exit するたびに log + sleep + 再 spawn。 Ctrl-C で wrapper ごと停止。
async fn watch_supervised(
    url: &str,
    timeout_secs: u64,
    from_filter: Option<&str>,
    restart_delay_secs: u64,
) -> Result<()> {
    let mut iteration = 0u64;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        iteration += 1;
        eprintln!(
            "[vp mailbox watch-supervised] iteration={} starting watch (url={}, timeout={}s)",
            iteration, url, timeout_secs
        );

        let watch_fut = watch(url, timeout_secs, from_filter);
        tokio::pin!(watch_fut);

        tokio::select! {
            _ = &mut ctrl_c => {
                eprintln!("[vp mailbox watch-supervised] ctrl-c received, exiting (no restart)");
                return Ok(());
            }
            result = &mut watch_fut => {
                match result {
                    Ok(()) => {
                        // Inner watch exited cleanly (e.g., its own ctrl-c handler ran)。
                        // 通常は inner watch は ctrl-c でしか exit しないので、 wrapper も止める。
                        eprintln!("[vp mailbox watch-supervised] inner watch exited cleanly, stopping supervisor");
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!(
                            "[vp mailbox watch-supervised] inner watch failed: {} (restart in {}s)",
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
                eprintln!("[vp mailbox watch-supervised] ctrl-c during restart wait, exiting");
                return Ok(());
            }
            _ = &mut sleep => {}
        }
    }
}

async fn watch(url: &str, timeout_secs: u64, from_filter: Option<&str>) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs + 5)) // server timeout + buffer
        .build()
        .context("reqwest client")?;
    let endpoint = format!("{}/api/msgbox/recv", url.trim_end_matches('/'));

    eprintln!(
        "[vp mailbox watch] subscribed to {} (timeout={}s, from={:?})",
        endpoint, timeout_secs, from_filter
    );

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut consecutive_errors = 0u32;

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                eprintln!("[vp mailbox watch] ctrl-c received, exiting");
                return Ok(());
            }
            result = poll_recv(&client, &endpoint, timeout_secs, from_filter) => {
                match result {
                    Ok(Some(msg_json)) => {
                        // Print one JSON line per message (line-buffered for Monitor downstream).
                        // Use println! for stdout + manual flush.
                        let line = serde_json::to_string(&msg_json)
                            .unwrap_or_else(|_| "{\"error\":\"json serialize\"}".into());
                        println!("{}", line);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                        consecutive_errors = 0;
                    }
                    Ok(None) => {
                        // timeout / closed — no message this poll、 just continue loop
                        consecutive_errors = 0;
                    }
                    Err(e) => {
                        consecutive_errors += 1;
                        eprintln!(
                            "[vp mailbox watch] recv error ({}/3): {}",
                            consecutive_errors, e
                        );
                        if consecutive_errors >= 3 {
                            eprintln!("[vp mailbox watch] 3 consecutive errors, sleeping 5s before retry");
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

async fn poll_recv(
    client: &reqwest::Client,
    endpoint: &str,
    timeout_secs: u64,
    from_filter: Option<&str>,
) -> Result<Option<serde_json::Value>> {
    let body = serde_json::json!({
        "timeout": timeout_secs,
        "from": from_filter,
    });

    let resp = client
        .post(endpoint)
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let status = resp
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match status {
        "ok" => Ok(resp.get("message").cloned()),
        "timeout" | "closed" => Ok(None),
        _ => {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("recv server error: {}", err)
        }
    }
}

async fn send(url: &str, to: &str, body: &str, from: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let endpoint = format!("{}/api/msgbox/send", url.trim_end_matches('/'));

    let payload = serde_json::json!({
        "from": from,
        "to": to,
        "body": body,
    });

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
