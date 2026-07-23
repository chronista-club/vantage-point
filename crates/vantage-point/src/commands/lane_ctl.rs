//! `vp lane capture` / `vp lane nudge` — lane console の read/write CLI（tmux decoupling PR2）
//!
//! 旧 `vp tmux capture` / `vp tmux send-keys` / `vp directmsg` の native 後継。
//! lane address（`<project>/root` / `<project>/performer/<name>`）を唯一の宛先語彙とし、
//! World process-proxy ask（`lane_capture` / `lane_nudge`）経由で SP の PtySlot に到達する
//! （tmux session 名 / pane id の第 2 名前空間は廃止）。
//!
//! ```bash
//! vp lane capture vantage-point/root          # console の現在画面を読む
//! vp lane capture vantage-point/root --session 2  # 同居する 2 枚目の console を読む
//! vp lane slots vantage-point/root            # この lane の slot 一覧（枚数 / pid / root か）
//! vp lane slot-new vantage-point/root         # console をもう 1 枚立てる（新 session）
//! vp lane nudge vantage-point/performer/sub "続けて"  # text + Enter を注入
//! ```
//!
//! ## doc 46 P5 — slot は lane に 1 枚ではなく session ごと
//!
//! `pty_slots` が `(lane, session)` key になり、1 つの lane に複数の console が同居できる。
//! `--session` 省略時は **root**（lane の代表 = mailbox `agent@<lane>` を名乗る住人、doc 39）で、
//! 従来の挙動と完全に同じ。表示（vp-app）は当面ミニマム（1 枚ずつ）なので、
//! **枚数と中身をここ（CLI）から読める**ようにしてある（doc 47 §7 成立条件②）。

use anyhow::{Result, bail};

use crate::commands::process_client::{
    resolve_project_path_from_target, world_process_request_blocking,
};
use crate::config::Config;

/// lane address の project 部分を project path に解決する（World ask の handshake identifier）。
/// `commands::now`（`vp now`）も同じ解決を使う。
pub(crate) fn project_path_for_lane(lane: &str, config: &Config) -> Result<String> {
    let project = lane
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "lane address が不正: '{}' — '<project>/root' か '<project>/performer/<name>' を指定",
                lane
            )
        })?;
    resolve_project_path_from_target(Some(project), config)
}

/// slot console の現在画面（Term grid render）を stdout に出す。`session=None` は root。
pub fn capture(lane: &str, session: Option<u32>, config: &Config) -> Result<()> {
    let path = project_path_for_lane(lane, config)?;
    let resp = world_process_request_blocking(
        crate::cli::world_port(),
        &path,
        "lane_capture",
        serde_json::json!({ "lane": lane, "session": session }),
    )?;
    // slot が 2 枚以上ある lane では「今どれを読んだか」が判らないと誤読するので、
    // 明示指定時 / 複数枚時だけ 1 行の見出しを stderr に出す（stdout は grid のみ = pipe 安全）。
    let slots: Vec<u64> = resp
        .get("slots")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(serde_json::Value::as_u64).collect())
        .unwrap_or_default();
    if session.is_some() || slots.len() > 1 {
        eprintln!(
            "[vp lane capture] {lane} session={} (slots: {slots:?})",
            session
                .map(|s| s.to_string())
                .unwrap_or_else(|| "root".into())
        );
    }
    match resp.get("content").and_then(|v| v.as_str()) {
        Some(content) => {
            println!("{}", content);
            Ok(())
        }
        None => bail!("lane_capture 応答に content がありません: {}", resp),
    }
}

/// lane が持つ console slot の一覧を表示する（doc 46 P5 — slot は session ごと）。
///
/// 表示（vp-app）を通さずに「今この lane に端末が何枚あるか」を読む口。
pub fn slots(lane: &str, config: &Config) -> Result<()> {
    let path = project_path_for_lane(lane, config)?;
    let resp = world_process_request_blocking(
        crate::cli::world_port(),
        &path,
        "lane_slots",
        serde_json::json!({ "lane": lane }),
    )?;
    let slots = resp
        .get("slots")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!("{lane}: {} slot", slots.len());
    if slots.is_empty() {
        println!("  (console 未配線 — chat mode の lane / spawn 失敗した lane は 0 枚が正常)");
        return Ok(());
    }
    println!(
        "  {:>7}  {:>7}  {:<6}  {:<5}  ATTACHED",
        "SESSION", "PID", "STATE", "ROOT"
    );
    for s in &slots {
        let get_u64 = |k: &str| s.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
        let get_bool = |k: &str| s.get(k).and_then(serde_json::Value::as_bool) == Some(true);
        println!(
            "  {:>7}  {:>7}  {:<6}  {:<5}  {}",
            get_u64("session"),
            get_u64("pid"),
            if get_bool("alive") { "alive" } else { "dead" },
            if get_bool("root") { "root" } else { "-" },
            if get_bool("attached") { "yes" } else { "no" },
        );
    }
    Ok(())
}

/// 新しい console（slot）を 1 枚立てる（doc 46 P5 producer）。
///
/// 立つのは **新しい session**（doc 46 §1.5「Pane は必ず新しい session id で始まる」）。
/// root（= lane の代表 / mailbox の主）は動かないので、既存 console はそのまま。
pub fn slot_new(lane: &str, stand: Option<&str>, config: &Config) -> Result<()> {
    let path = project_path_for_lane(lane, config)?;
    let resp = world_process_request_blocking(
        crate::cli::world_port(),
        &path,
        "lane_slot_new",
        serde_json::json!({ "lane": lane, "stand": stand }),
    )?;
    let get_u64 = |k: &str| resp.get(k).and_then(serde_json::Value::as_u64);
    let (Some(session), Some(pid)) = (get_u64("session"), get_u64("pid")) else {
        bail!("lane_slot_new 応答が不正です: {}", resp);
    };
    println!(
        "[vp lane slot-new] {lane}: session={session} pid={pid}（この lane の console {} 枚）",
        get_u64("count").unwrap_or(0)
    );
    println!("  読む: vp lane capture {lane} --session {session}");
    Ok(())
}

/// slot の claude / shell に text + Enter を注入する（submit 意味論は SP 側 `deliver_nudge`）。
/// `session=None` は root（wire mailbox `agent@<lane>` を名乗る住人、doc 39）。
pub fn nudge(lane: &str, session: Option<u32>, text: &str, config: &Config) -> Result<()> {
    let path = project_path_for_lane(lane, config)?;
    world_process_request_blocking(
        crate::cli::world_port(),
        &path,
        "lane_nudge",
        serde_json::json!({ "lane": lane, "text": text, "session": session }),
    )?;
    match session {
        Some(s) => println!("[vp lane nudge] sent → {lane} (session={s})"),
        None => println!("[vp lane nudge] sent → {lane}"),
    }
    Ok(())
}
