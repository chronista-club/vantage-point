//! `vp wire` subcommand — wire accumulation messaging の CLI 入口 (wiremsg R5-2)
//!
//! ## 概要
//!
//! TheWorld 中央 wire store の `wire_recv` を long-poll loop で叩き、 受信した wire message を
//! 1 行 JSON で stdout に出力する。 Claude Code Monitor tool の subscription source として活用
//! される (Monitor は stdout-emitting 何でも push channel になる、 universal subscription primitive)。
//!
//! ## L0 portless B-4 (wire-unison): transport は World "wire" unison channel に直結
//!
//! 旧 SP HTTP proxy (`--url` で SP base を指定 → SP `/api/wire/*` → World relay) は撤去。
//! CLI は `crate::process::world_wire::call` で **World daemon (QUIC, port 32000) の "wire" channel**
//! に直結する (doc 27 §62「全通信 unison channel」)。 World 直結のため address は qualified 前提
//! (bare `"agent"` は中央 store が reject、 正規化は MCP 経路の SP のみが行う)。
//!
//! ## 使い方
//!
//! ```bash
//! # wire address を指定して watch
//! vp wire watch --agent agent@vantage-point
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

use anyhow::Result;
use clap::Subcommand;
use std::time::Duration;

#[derive(Subcommand, Debug)]
pub enum WireCommands {
    /// World wire channel を long-poll で watch、 受信 message を 1 行 JSON で stdout に出す
    ///
    /// Claude Code Monitor の subscription source として使う想定。 SIGTERM / Ctrl-C で graceful exit。
    Watch {
        /// 受信先 wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
        /// 各 long-poll の timeout 秒数 (server 側 max 30、 default 25)
        #[arg(short, long, default_value_t = 25)]
        timeout: u64,
    },
    /// World wire channel から 1-shot で受信 (= mcp__wire_recv の CLI pair)。
    ///
    /// `watch` と異なり loop しない: long-poll を 1 回だけ実行し、 受信した message 一式を
    /// 1 行 JSON ({"messages":[...], "count":N}) で stdout に出して即 exit。
    /// 既読 cursor が進む点は MCP wire_recv と同じ (= 同 message が再配信されない)。
    /// script / one-off check 用。 continuous な subscription は `watch` を使う。
    Recv {
        /// 受信先 wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
        /// long-poll の timeout 秒数 (server 側 max 30、 default 5、 mcp__wire_recv と同 default)
        #[arg(short, long, default_value_t = 5)]
        timeout: u64,
    },
    /// World wire channel に message を送信 (ad-hoc test 用)
    Send {
        /// 宛先 wire address (例: `agent@vantage-point`)。 World 直結のため qualified 前提。
        /// `--world` 指定時は **宛先 world 内部の logical address**（例: `agent@nostos/main`）。
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
        /// 宛先 **remote world** の handle（federation 送信、flow ③）。指定すると TheWorld が
        /// hub relay 経由で遠方 world へ送る。`--to` はその world 内部の logical address になる。
        /// 未指定ならローカル中央 store に送る（従来動作）。
        #[arg(long)]
        world: Option<String>,
    },
    /// 遠方 world の lane 一覧を問い合わせる (federation discovery、 在庫確認、 flow step 2)
    ///
    /// 宛先を知らないとき、 relay 上の request-response で相手 world の lane を列挙する。
    /// 例: `vp wire discover --world taro-box`
    Discover {
        /// 問い合わせる remote world の handle
        #[arg(long)]
        world: String,
    },
    /// 未読の在庫確認 (= mcp__wire_inbox の CLI pair、 read-only で cursor 不触り)
    ///
    /// `recv` と異なり cursor を進めない: 「読まずに在庫だけ確認」する。
    /// `{status, total, by_thread: {root_id: count}}` を pretty JSON で stdout に出す。
    Inbox {
        /// 確認する wire address (例: `agent@vantage-point`)。 必須。
        #[arg(short, long)]
        agent: String,
    },
    /// thread 系譜の取得 (= mcp__wire_thread の CLI pair、 read-only で cursor 不触り)
    ///
    /// 指定 message から prev を root まで辿った系譜 (root-first・chronological) を返す。
    /// 途中参加 thread の backlog 確認用。
    Thread {
        /// 系譜を辿る起点 message id (recv / inbox で得た id)
        #[arg(short, long)]
        message_id: String,
    },
    /// message の ack (R2-a、 command category の受領確認。 = mcp__wire_ack の CLI pair)
    ///
    /// cursor とは独立の ack 台帳に記録する。 command の処理を終えたら ack すること
    /// (未 ack の command は delivery loop (R2-b) の再掲示対象になる)。
    Ack {
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
    /// 注意: CC hook 専用 — stdin を pipe で繋いで使う (TTY 直接実行は即 return する)。
    HookCheck,
    /// 観測（Canvas Pane 可視化、doc 28 §7/§2）: TheWorld 中央 store の全委譲を「会話 thread」
    /// markdown にして stdout に出す read-only コマンド。`mcp__vantage-point__show` に流して
    /// Canvas Pane に表示する静的観測 surface（介入でなく観測でフロー全体を追う）。
    DelegThread,
    /// shell-level supervisor: vp wire watch を loop で再起動。 inner watch が exit しても
    /// auto-restart で監視を継続する (lifecycle resilience)。 Monitor の前段に置いて、
    /// SessionStart hook 等から自動 arm する想定。
    WatchSupervised {
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
        WireCommands::Watch { agent, timeout } => watch(&agent, timeout).await,
        WireCommands::Recv { agent, timeout } => recv(&agent, timeout).await,
        WireCommands::Send {
            to,
            body,
            from,
            reply_to,
            category,
            world,
        } => {
            send(
                &to,
                &body,
                &from,
                reply_to.as_deref(),
                category.as_deref(),
                world.as_deref(),
            )
            .await
        }
        WireCommands::Discover { world } => discover_lanes(&world).await,
        WireCommands::Inbox { agent } => inbox(&agent).await,
        WireCommands::Thread { message_id } => thread(&message_id).await,
        WireCommands::Ack { message_id, agent } => ack(&message_id, &agent).await,
        WireCommands::HookCheck => hook_check().await,
        WireCommands::DelegThread => deleg_thread().await,
        WireCommands::WatchSupervised {
            agent,
            timeout,
            restart_delay,
        } => watch_supervised(&agent, timeout, restart_delay).await,
    }
}

/// World "wire" channel への QUIC 呼び出し (= `world_wire::call` の CLI 薄 wrapper)。
///
/// 旧 SP HTTP proxy (`--url`) を撤去し、 wire/delegation を World daemon 直結に統一 (doc 27 §62)。
/// `world_wire::call` の Err(String) を anyhow に写す (error frame は call が既に Err 化済)。
async fn call_world(path: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
    crate::process::world_wire::call(path, payload)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Supervisor: watch loop の auto-restart wrapper。
/// inner watch が exit するたびに log + sleep + 再 spawn。 Ctrl-C で wrapper ごと停止。
async fn watch_supervised(agent: &str, timeout_secs: u64, restart_delay_secs: u64) -> Result<()> {
    let mut iteration = 0u64;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        iteration += 1;
        eprintln!(
            "[vp wire watch-supervised] iteration={} starting watch (agent={}, timeout={}s)",
            iteration, agent, timeout_secs
        );

        let watch_fut = watch(agent, timeout_secs);
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

async fn watch(agent: &str, timeout_secs: u64) -> Result<()> {
    eprintln!(
        "[vp wire watch] subscribed to World wire channel (agent={}, timeout={}s)",
        agent, timeout_secs
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
            result = poll_recv(agent, timeout_secs) => {
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
async fn recv(agent: &str, timeout_secs: u64) -> Result<()> {
    // server 側 timeout 上限 (= wire_recv: 30s 上限) は client 側でも clamp。
    // mcp__wire_recv と同じ semantic で揃える。
    let clamped_timeout = timeout_secs.min(30);
    let messages = poll_recv(agent, clamped_timeout).await?;
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
///
/// `world_wire::call` が server Err (`{"error":..}` frame) を既に Err に変換するので、 ここでは
/// `messages` 配列の取り出しだけ行う。
async fn poll_recv(agent: &str, timeout_secs: u64) -> Result<Vec<serde_json::Value>> {
    let resp = call_world(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout_secs }),
    )
    .await?;
    let messages = resp
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(messages)
}

/// read-only 系 (inbox / thread / ack) の共通実行部。
/// 応答を pretty JSON で stdout に出す (error frame は `call_world` が Err にする)。
async fn post_and_print(path: &str, payload: serde_json::Value) -> Result<()> {
    let resp = call_world(path, payload).await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

/// wire_unread_count — 未読在庫を表示 (cursor 不触り)
async fn inbox(agent: &str) -> Result<()> {
    post_and_print(
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wire_thread — 系譜 (root-first) を表示 (cursor 不触り)
async fn thread(message_id: &str) -> Result<()> {
    post_and_print(
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
    if lane == crate::process::lanes_state::ROOT_LANE_NAME {
        Some(format!("agent@{project}"))
    } else {
        Some(format!("agent@{project}/{lane}"))
    }
}

/// `VP_SESSION_KEY` env（spawn 時に `stand_spawner` が注入）から「自分がどの session か」を
/// 復元する（純関数、doc 40 §4）。
///
/// **None = 不明**であって root ではない。ここで `unwrap_or(1)` に倒すと「名乗らなかった」と
/// 「root を名乗った」が混ざり、実在しない session の報告を root に落とす事故（session 粒度化で
/// 消したかったもの）を検知できなくなる。root への fallback は SP 側 policy
/// （`ReportTarget::Unspecified`）が 1 箇所で引き受ける。
///
/// - env 不在 / 空 = 旧 binary で spawn 済の slot / VP 外で起動された claude → None
/// - 非数値 / 0 = 壊れた値 → None（黙って 1 に丸めない）
fn session_key_from_env(raw: Option<&str>) -> Option<crate::lane::session_registry::SessionKey> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())?
        .parse::<crate::lane::session_registry::SessionKey>()
        .ok()
        .filter(|k| *k >= 1)
}

/// 未読件数から hookSpecificOutput JSON を作る (純関数、 R2-c)。 未読 0 は None (出力なし)。
fn build_hook_output(
    event_name: &str,
    total: u64,
    deleg_summary: Option<String>,
) -> Option<String> {
    // wire 未読 + 委譲 pull の両断面を 1 つの additionalContext に合流する。
    let mut parts: Vec<String> = Vec::new();
    if total > 0 {
        parts.push(format!(
            "📬 wire: {total} 件未読。 mcp__vantage-point__wire_recv で受信してください \
             (command category は処理後に mcp__vantage-point__wire_ack)。"
        ));
    }
    if let Some(s) = deleg_summary {
        parts.push(s);
    }
    if parts.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": parts.join("\n"),
            }
        })
        .to_string(),
    )
}

/// pull-hook（C、doc 28 §3-2 の at-next-turn tier）: poll で得た undelivered 委譲のうち、
/// この agent が今すぐ動ける（role × state）ものを 1 つの packet に要約する。
///
/// - requester 視点: `done`/`failed`（Outcome 受領→再開/判断）/ `awaiting_response`（質問→respond）。
/// - doer 視点: `active`（task→complete）。
///
/// push（reconcile の send-keys）取りこぼしをターン頭で拾う最終保険。read-only（配達 mark はしない）。
fn delegation_summary(agent: &str, delegations: &[serde_json::Value]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for d in delegations {
        let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let state = d.get("state").and_then(|v| v.as_str()).unwrap_or("");
        let requester = d.get("requester").and_then(|v| v.as_str()).unwrap_or("");
        let doer = d.get("doer").and_then(|v| v.as_str()).unwrap_or("");
        let task = d.get("task").and_then(|v| v.as_str()).unwrap_or("");
        if requester == agent {
            match state {
                "done" => {
                    let r = d
                        .pointer("/outcome/result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    lines.push(format!(
                        "委譲 {id} → Done: {r}。中断していた作業を再開してください。"
                    ));
                }
                "failed" => {
                    let r = d
                        .pointer("/outcome/reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    lines.push(format!(
                        "委譲 {id} → Failed: {r}。再委譲/自分で実施を判断してください。"
                    ));
                }
                "awaiting_response" => {
                    let q = d
                        .pointer("/outcome/question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    lines.push(format!(
                        "委譲 {id} ← 質問: {q}。mcp__vantage-point__respond(id=\"{id}\", answer=...) で回答してください。"
                    ));
                }
                _ => {}
            }
        } else if doer == agent && state == "active" {
            lines.push(format!(
                "委譲 {id}: {task}。mcp__vantage-point__complete(id=\"{id}\", ...) で報告してください。"
            ));
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "📋 未配達の委譲 {} 件:\n{}",
        lines.len(),
        lines.join("\n")
    ))
}

/// 委譲 state を観測 thread 用の絵文字バッジに写す（active=進行中 / done / failed / 質問待ち）。
fn delegation_state_emoji(state: &str) -> &'static str {
    match state {
        "active" => "⏳",
        "done" => "✅",
        "failed" => "❌",
        "awaiting_response" => "❓",
        _ => "•",
    }
}

/// 観測（Canvas Pane 可視化、doc 28 §7/§2）: 全委譲を「会話 thread」markdown に整形する。
///
/// requester↔doer の pair（順不同）ごとに束ね、各委譲を task（依頼）→ outcome（doer の応答）の
/// exchange として表示する。介入でなく観測でフロー全体を追う read-only surface。
/// pure fn（data→markdown）= unit test 可能、`delegation_summary` と違い role 視点では絞らない。
fn delegation_thread_markdown(delegations: &[serde_json::Value]) -> String {
    if delegations.is_empty() {
        return "# 🔀 委譲 thread\n\n_まだ委譲はありません。_\n".to_string();
    }
    // pair（requester↔doer、順不同）ごとに束ねる。a→b と b→a を 1 会話に畳む。
    // 挿入順（= created_at 昇順、route が ORDER BY 済）を保つため Vec で持つ。
    let mut groups: Vec<(String, Vec<&serde_json::Value>)> = Vec::new();
    for d in delegations {
        let requester = d.get("requester").and_then(|v| v.as_str()).unwrap_or("");
        let doer = d.get("doer").and_then(|v| v.as_str()).unwrap_or("");
        let mut pair = [requester, doer];
        pair.sort_unstable();
        let key = format!("{} ⇄ {}", pair[0], pair[1]);
        match groups.iter_mut().find(|(k, _)| k == &key) {
            Some((_, v)) => v.push(d),
            None => groups.push((key, vec![d])),
        }
    }

    let mut out = format!("# 🔀 委譲 thread（{} 件）\n", delegations.len());
    for (pair, items) in &groups {
        out.push_str(&format!("\n## {pair}\n"));
        for d in items {
            let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let state = d.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let requester = d.get("requester").and_then(|v| v.as_str()).unwrap_or("");
            let doer = d.get("doer").and_then(|v| v.as_str()).unwrap_or("");
            let task = d.get("task").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!(
                "\n- **{id}** {} `{state}`\n",
                delegation_state_emoji(state)
            ));
            out.push_str(&format!("  - 🗣️ {requester} → {doer}: {task}\n"));
            // outcome（doer の応答）を state に応じて表示。active はまだ応答なし。
            match state {
                "done" => {
                    let r = d
                        .pointer("/outcome/result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    out.push_str(&format!("  - ✅ {doer} → {requester}: {r}\n"));
                }
                "failed" => {
                    let r = d
                        .pointer("/outcome/reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    out.push_str(&format!("  - ❌ {doer} → {requester}: {r}\n"));
                }
                "awaiting_response" => {
                    let q = d
                        .pointer("/outcome/question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    out.push_str(&format!("  - ❓ {doer} → {requester}: {q}\n"));
                }
                _ => {}
            }
        }
    }
    out
}

/// hook 実体 (R2-c、 チャネル B): 設計 mem_1CbvcJj4ppU3QKH9d7xMpT。
///
/// 全エラー path で Ok(()) を返し何も出力しない (fail-open) — hook の失敗で
/// 会話を邪魔しないことが最優先。 TheWorld "wire" channel 直叩き (qualified address を自前導出
/// するので SP proxy 不要 = 自 SP が落ちていても未読通知は出る)。 hook は会話を block しない
/// よう各 call を 2s で bound する (`world_wire::call` の 40s outer とは別の短い実効上限)。
/// hook event → 会話報告の契機（doc 40 §6 の wire 表現。None = 報告対象外の event）。
///
/// hook は**報告者**であり記録判断を持たない — 宛先 session の解決と F1/F2 guard（resume 失敗
/// `|| claude` fallback の幻 session が健在な旧会話を上書きしない、解剖 memory
/// `cc-session-pointer-self-destruction`）は SP 側 `record_conversation` の 1 箇所。
/// 旧 `should_record_cc_session`（UserPromptSubmit のみ記録 = #795 の鈍器）の後継。
fn conversation_report_kind(event_name: &str) -> Option<&'static str> {
    match event_name {
        "SessionStart" => Some("issued"),
        "UserPromptSubmit" => Some("spoken"),
        _ => None,
    }
}

async fn hook_check() -> Result<()> {
    // TTY からの手動実行ガード (moody 指摘): read_to_string が EOF 待ちで永久 block
    // するため、 hook 専用である旨を案内して即 return する (exit 0)。
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        eprintln!(
            "[vp wire hook-check] CC hook 専用コマンドです (stdin に hook JSON を pipe して使う)。\
             echoes spawn が --settings で自動注入します。"
        );
        return Ok(());
    }

    // stdin の hook JSON から event 名を取る (parse 失敗は fail-open で default)
    let mut input = String::new();
    use std::io::Read;
    let _ = std::io::stdin().read_to_string(&mut input);
    let parsed = serde_json::from_str::<serde_json::Value>(&input).ok();
    let event_name = parsed
        .as_ref()
        .and_then(|v| v.get("hook_event_name").and_then(|e| e.as_str()))
        .unwrap_or("UserPromptSubmit")
        .to_string();

    let project = std::env::var("VP_PROJECT").ok();
    let lane = std::env::var("VP_LANE").ok();
    // doc 40 §4 / doc 46 P5: 自分がどの session の claude かを名乗る（None = 不明。root では
    // ないので、ここで root に丸めない — 判断は SP 側 policy に 1 本化する）。
    let session_key = session_key_from_env(std::env::var("VP_SESSION_KEY").ok().as_deref());

    // doc 40 §4: hook は会話 id の**報告者** — (project, lane, session, session_id, 契機) を
    // World 経由で SP へ送るだけ。宛先 session の解決と記録判断（F1/F2 guard 込みの policy）は
    // SP 側 `session_registry::record_conversation` の 1 箇所が持つ。旧実装の
    // `cc_session::record(VP_LANE)` 直書きは root の session label に追従せず、root≥2 で
    // 書き手/読み手のラベル乖離バグを起こした（doc 40 §1-1 の根治でここから file 書きを撤去）。
    // 失敗は無視（fail-open — 毎 turn 再報告されるので次の発話で self-heal する、doc 40 §9）。
    if let Some(report) = conversation_report_kind(&event_name)
        && let Some(sid) = parsed
            .as_ref()
            .and_then(|v| v.get("session_id").and_then(|s| s.as_str()))
        && let (Some(p), Some(l)) = (project.as_deref(), lane.as_deref())
    {
        // 送信前の no-op 判定（read-only load）: **自分が名乗る session** の会話 id が既に
        // 同値なら送らない（毎ターンの無駄打ち回避 — 旧 `changed` 判定の後継。判定できない
        // 時は送って SP 側の no-op に任せる）。名乗らない場合の比較先は root = SP 側
        // `ReportTarget::Unspecified` の着地先と揃える（ここがズレると「送らないのに
        // 記録もされない」無音の穴になる）。
        let reg = crate::lane::session_registry::load(p, l, "echoes");
        let target_key = session_key.unwrap_or(reg.root);
        let target_conv = reg
            .sessions
            .iter()
            .find(|s| s.key == target_key)
            .and_then(|s| s.conversation.as_deref());
        if target_conv != Some(sid) {
            let mut payload = serde_json::json!({
                "project": p,
                "lane": l,
                "session_id": sid,
                "event": report,
            });
            // 名乗れる時だけ載せる（field 不在 = 不明 = SP 側で root 宛の後方互換扱い）。
            if let Some(key) = session_key {
                payload["session"] = key.into();
            }
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                crate::process::world_wire::call("/api/lane/session-changed", payload),
            )
            .await;
        }
    }

    let Some(agent) = wire_address_from_env(project.as_deref(), lane.as_deref()) else {
        return Ok(()); // VP 外で起動された claude — 何もしない
    };

    // 未読 wire count を 2s で bound して取得。 daemon 不在/wedge/transport 失敗は silent 成功。
    let total = match tokio::time::timeout(
        Duration::from_secs(2),
        crate::process::world_wire::call(
            "/api/wire/unread-count",
            serde_json::json!({ "agent": agent }),
        ),
    )
    .await
    {
        Ok(Ok(v)) => v.get("total").and_then(|t| t.as_u64()).unwrap_or(0),
        _ => return Ok(()), // timeout / transport 失敗 — fail-open
    };

    // pull-hook（C、doc 28 §3-2）: World の undelivered 委譲を poll してターン頭に surface。
    // 失敗は fail-open（委譲 pull が無くても wire 通知は出す）。
    let deleg_summary = match tokio::time::timeout(
        Duration::from_secs(2),
        crate::process::world_wire::call(
            "/api/delegation/poll",
            serde_json::json!({ "agent": agent }),
        ),
    )
    .await
    {
        Ok(Ok(v)) => v
            .get("delegations")
            .and_then(|d| d.as_array().cloned())
            .and_then(|ds| delegation_summary(&agent, &ds)),
        _ => None,
    };

    if let Some(out) = build_hook_output(&event_name, total, deleg_summary) {
        println!("{out}");
    }
    Ok(())
}

/// 観測（Canvas Pane 可視化、doc 28 §7/§2）: TheWorld 中央 store の全委譲を取得し、
/// 「会話 thread」markdown を stdout に出す。`mcp__vantage-point__show` に流して Canvas Pane へ。
/// hook と違い fail-open ではなく、取得失敗は Err にして利用者に分かるようにする（対話 CLI）。
async fn deleg_thread() -> Result<()> {
    let body = call_world("/api/delegation/list", serde_json::json!({}))
        .await
        .map_err(|e| anyhow::anyhow!("delegation list 取得失敗: {e}"))?;
    let ds = body
        .get("delegations")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    println!("{}", delegation_thread_markdown(&ds));
    Ok(())
}

/// wire_ack — message を ack する (R2-a、 ack 台帳に記録)
async fn ack(message_id: &str, agent: &str) -> Result<()> {
    post_and_print(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

async fn send(
    to: &str,
    body: &str,
    from: &str,
    reply_to: Option<&str>,
    category: Option<&str>,
    world: Option<&str>,
) -> Result<()> {
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

    // --world 指定時は federation 送信（flow ③）: TheWorld が hub relay 経由で遠方 world へ送る。
    // `to` はその world 内部の logical address（agent@project/lane）。未指定はローカル中央 store。
    let path = if let Some(remote) = world {
        payload["world"] = serde_json::Value::String(remote.to_string());
        "/api/wire/federate"
    } else {
        "/api/wire/send"
    };

    let resp = call_world(path, payload).await?;
    println!("{}", serde_json::to_string(&resp).unwrap_or_default());
    Ok(())
}

/// 遠方 world の lane 一覧を問い合わせる（federation discovery）。TheWorld 経由で relay 上の
/// request-response を回し、相手 world の lane を列挙する。
async fn discover_lanes(world: &str) -> Result<()> {
    let resp = call_world(
        "/api/wire/discover-lanes",
        serde_json::json!({ "world": world }),
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
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

    /// 会話報告の契機写像（doc 40 §6）: SessionStart = issued（eager 表示）/
    /// UserPromptSubmit = spoken（authoritative）/ その他は報告しない。
    /// F1/F2 の記録判断は SP 側 policy に移った（hook 側は判断を持たない）が、
    /// 「Stop 等の無関係 event で報告しない」ことはここで塞ぐ。
    #[test]
    fn conversation_report_kind_maps_events_to_doc40_semantics() {
        assert_eq!(conversation_report_kind("SessionStart"), Some("issued"));
        assert_eq!(conversation_report_kind("UserPromptSubmit"), Some("spoken"));
        assert_eq!(conversation_report_kind("Stop"), None);
        assert_eq!(conversation_report_kind(""), None);
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
            WireCommands::Recv { timeout, agent } => {
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

    /// R2-a CLI parity: inbox は agent 必須 (World 直結、 url 引数は撤去済)
    #[test]
    fn inbox_parses_agent() {
        let cli = TestCli::try_parse_from(["vp-wire-test", "inbox", "-a", "agent@vp"])
            .expect("parse should succeed");
        match cli.cmd {
            WireCommands::Inbox { agent } => assert_eq!(agent, "agent@vp"),
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
            wire_address_from_env(Some("vp"), Some("root")).as_deref(),
            Some("agent@vp")
        );
        assert_eq!(
            wire_address_from_env(Some("vp"), Some("w1")).as_deref(),
            Some("agent@vp/w1")
        );
        // env 不足 = VP 外で起動された claude → None (fail-open)
        assert_eq!(wire_address_from_env(None, Some("root")), None);
        assert_eq!(wire_address_from_env(Some("vp"), None), None);
        assert_eq!(wire_address_from_env(Some(""), Some("root")), None);
    }

    /// doc 40 §4: hook は `VP_SESSION_KEY` で自分の session を名乗る。**「不明」を root に
    /// 丸めない**ことを固定する — env 不在 / 壊れた値は `None`（= 報告に session field を
    /// 載せない）であって `Some(1)`（= root を名乗った）ではない。両者を混ぜると、実在しない
    /// session の報告が root の会話 id を壊す経路が復活する。
    #[test]
    fn session_key_env_distinguishes_unknown_from_root() {
        assert_eq!(session_key_from_env(Some("1")), Some(1));
        assert_eq!(session_key_from_env(Some("2")), Some(2));
        assert_eq!(session_key_from_env(Some(" 3 ")), Some(3), "前後空白は許容");
        // 不明（root ではない）
        assert_eq!(
            session_key_from_env(None),
            None,
            "env 不在 = 旧 slot / VP 外"
        );
        assert_eq!(session_key_from_env(Some("")), None);
        assert_eq!(session_key_from_env(Some("root")), None, "非数値は丸めない");
        assert_eq!(session_key_from_env(Some("0")), None, "0 は不正な key");
        assert_eq!(session_key_from_env(Some("-1")), None);
    }

    /// R2-c: 未読ありのときだけ hookSpecificOutput JSON を作る
    #[test]
    fn hook_output_only_when_unread() {
        // wire 0 件 + 委譲 0 件 → None。
        assert_eq!(build_hook_output("SessionStart", 0, None), None);
        let out = build_hook_output("UserPromptSubmit", 3, None).expect("3 件未読なら出力");
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additionalContext");
        assert!(ctx.contains("3 件"), "件数を含む: {ctx}");
        assert!(ctx.contains("wire_recv"), "受信導線を含む: {ctx}");
    }

    /// C: wire 0 件でも委譲 pull があれば hook 出力する（合流）。
    #[test]
    fn hook_output_includes_delegation_pull() {
        let summary = Some("📋 未配達の委譲 1 件:\n委譲 dlg-x → Done: R。".to_string());
        let out = build_hook_output("UserPromptSubmit", 0, summary).expect("委譲 pull で出力");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.contains("委譲 dlg-x → Done"),
            "委譲 summary を含む: {ctx}"
        );
    }

    /// C: delegation_summary は role × state で actionable なものだけ要約する。
    #[test]
    fn delegation_summary_by_role_and_state() {
        let agent = "agent@vp";
        let ds = vec![
            // requester 視点 done → 再開導線。
            serde_json::json!({
                "id": "dlg-1", "state": "done", "requester": "agent@vp",
                "doer": "agent@vp/w", "task": "T", "outcome": {"kind": "done", "result": "R"}
            }),
            // requester 視点 awaiting_response → respond 導線。
            serde_json::json!({
                "id": "dlg-2", "state": "awaiting_response", "requester": "agent@vp",
                "doer": "agent@vp/w", "task": "T", "outcome": {"kind": "needsinput", "question": "Q?"}
            }),
            // この agent が doer の active → complete 導線。
            serde_json::json!({
                "id": "dlg-3", "state": "active", "requester": "agent@other",
                "doer": "agent@vp", "task": "Tdoer", "outcome": null
            }),
            // requester 視点 active（doer の wake 待ち）→ requester には surface しない。
            serde_json::json!({
                "id": "dlg-4", "state": "active", "requester": "agent@vp",
                "doer": "agent@vp/w", "task": "T", "outcome": null
            }),
        ];
        let summary = delegation_summary(agent, &ds).expect("actionable あり");
        assert!(summary.contains("dlg-1 → Done"));
        assert!(summary.contains("dlg-2 ← 質問"));
        assert!(summary.contains("dlg-3"));
        assert!(
            summary.contains("3 件"),
            "actionable は 3 件 (dlg-4 は除外): {summary}"
        );
        assert!(
            !summary.contains("dlg-4"),
            "requester+active は surface しない"
        );

        // 関与しない agent → None。
        assert!(delegation_summary("agent@nobody", &ds).is_none());
    }

    /// 観測 thread: pair ごとに束ね、task→outcome の exchange を全件（state 無関係）描く。
    #[test]
    fn delegation_thread_markdown_groups_by_pair_and_shows_exchange() {
        let ds = vec![
            // a⇄w1: done（result 表示）。
            serde_json::json!({
                "id": "dlg-1", "state": "done", "requester": "a@vp",
                "doer": "a@vp/w1", "task": "DB schema", "outcome": {"kind": "done", "result": "完了"}
            }),
            // a⇄w1: awaiting_response（question 表示）— 同 pair に畳まれる。
            serde_json::json!({
                "id": "dlg-2", "state": "awaiting_response", "requester": "a@vp",
                "doer": "a@vp/w1", "task": "API 実装", "outcome": {"kind": "needsinput", "question": "認証は?"}
            }),
            // a⇄w2: active（doer 応答なし）— 別 pair。requester/doer 逆順でも同会話化を確認。
            serde_json::json!({
                "id": "dlg-3", "state": "active", "requester": "a@vp/w2",
                "doer": "a@vp", "task": "lint 修正", "outcome": null
            }),
        ];
        let md = delegation_thread_markdown(&ds);
        assert!(md.contains("委譲 thread（3 件）"), "件数ヘッダ: {md}");
        // pair grouping（順不同 key で a@vp と a@vp/w1 が 1 つの ⇄ 見出しに）。
        assert!(md.contains("## a@vp ⇄ a@vp/w1"));
        assert!(md.contains("## a@vp ⇄ a@vp/w2"), "逆順 pair も正規化: {md}");
        // exchange: task（依頼）と outcome（応答）の両方。
        assert!(md.contains("🗣️ a@vp → a@vp/w1: DB schema"));
        assert!(md.contains("✅ a@vp/w1 → a@vp: 完了"));
        assert!(md.contains("❓ a@vp/w1 → a@vp: 認証は?"));
        // active は task のみ（doer 応答行は出ない）。
        assert!(md.contains("🗣️ a@vp/w2 → a@vp: lint 修正"));

        // 空 → プレースホルダ。
        assert!(delegation_thread_markdown(&[]).contains("まだ委譲はありません"));
    }
}
