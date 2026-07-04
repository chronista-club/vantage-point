//! `vp lane capture` / `vp lane nudge` — lane console の read/write CLI（tmux decoupling PR2）
//!
//! 旧 `vp tmux capture` / `vp tmux send-keys` / `vp directmsg` の native 後継。
//! lane address（`<project>/conductor` / `<project>/performer/<name>`）を唯一の宛先語彙とし、
//! World process-proxy ask（`lane_capture` / `lane_nudge`）経由で SP の PtySlot に到達する
//! （tmux session 名 / pane id の第 2 名前空間は廃止）。
//!
//! ```bash
//! vp lane capture vantage-point/conductor          # console の現在画面を読む
//! vp lane nudge vantage-point/performer/sub "続けて"  # text + Enter を注入
//! ```

use anyhow::{Result, bail};

use crate::commands::process_client::{
    resolve_project_path_from_target, world_process_request_blocking,
};
use crate::config::Config;

/// lane address の project 部分を project path に解決する（World ask の handshake identifier）。
fn project_path_for_lane(lane: &str, config: &Config) -> Result<String> {
    let project = lane
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "lane address が不正: '{}' — '<project>/conductor' か '<project>/performer/<name>' を指定",
                lane
            )
        })?;
    resolve_project_path_from_target(Some(project), config)
}

/// lane console の現在画面（Term grid render）を stdout に出す。
pub fn capture(lane: &str, config: &Config) -> Result<()> {
    let path = project_path_for_lane(lane, config)?;
    let resp = world_process_request_blocking(
        crate::cli::world_port(),
        &path,
        "lane_capture",
        serde_json::json!({ "lane": lane }),
    )?;
    match resp.get("content").and_then(|v| v.as_str()) {
        Some(content) => {
            println!("{}", content);
            Ok(())
        }
        None => bail!("lane_capture 応答に content がありません: {}", resp),
    }
}

/// lane の claude / shell に text + Enter を注入する（submit 意味論は SP 側 `deliver_nudge`）。
pub fn nudge(lane: &str, text: &str, config: &Config) -> Result<()> {
    let path = project_path_for_lane(lane, config)?;
    world_process_request_blocking(
        crate::cli::world_port(),
        &path,
        "lane_nudge",
        serde_json::json!({ "lane": lane, "text": text }),
    )?;
    println!("[vp lane nudge] sent → {}", lane);
    Ok(())
}
