//! `vp now` — session の「今なにを」自己申告 CLI（doc 51 §1 A3b — now-line のメイン供給）
//!
//! AI が自分の shell tool から 1 行で「今」を環境へ報告する口（表象の共有の AI→人方向）。
//! MCP でなく CLI なのは意図的: shell tool は全 engine が必ず持つ（= MCP 設定不要で
//! engine 中立）、`vp wire` / `vp flow` と同じ家風、binary 直で daemon に届く。
//!
//! ```bash
//! vp now "panic 箇所を特定中 — pty_slot の lock 順を確認"
//! ```
//!
//! 宛先の識別は **spawn 時注入の env**（Act I slot = stand_spawner / Act II host が注入）:
//! `VP_REPO` + `VP_LANE`（lane address の導出）+ `VP_SESSION_KEY`（session の名乗り、
//! doc 40 §4 の hook identity と同じ）。env の無い場所（lane の外の手動実行）は
//! `--lane` / `--session` で明示する。
//!
//! 表示は GUI の now-line（名札直下の動的一行）。`TurnCompleted` で消える —
//! 「今」は turn より長生きしないので、サブタスクの切れ目ごとに打ち直す運用。

use anyhow::{Result, bail};

use crate::commands::lane_ctl::repo_path_for_lane;
use crate::commands::process_client::daemon_repo_request_blocking;
use crate::config::Config;

/// env（VP_REPO / VP_LANE）から lane address（Display 形）を導く。
/// stand_spawner の注入と対の読み手（値の形は `<repo>/root` / `<repo>/performer/<name>`）。
fn lane_addr_from_env() -> Option<String> {
    let repo = std::env::var("VP_REPO").ok().filter(|s| !s.is_empty())?;
    let label = std::env::var("VP_LANE").ok().filter(|s| !s.is_empty())?;
    if label == crate::repo::lanes_state::ROOT_LANE_NAME {
        Some(format!("{repo}/{label}"))
    } else {
        Some(format!("{repo}/performer/{label}"))
    }
}

/// 「今なにを」1 行を daemon へ送る（`session_now` method）。
///
/// `lane` / `session` 省略時は env から導出。session が env にも無い場合は None のまま送り、
/// daemon 側が root（lane の代表）に読み替える。
pub fn report(
    text: &str,
    lane_override: Option<&str>,
    session_override: Option<u32>,
    config: &Config,
) -> Result<()> {
    if text.trim().is_empty() {
        bail!("vp now: text が空です — 「今なにを」を 1 行で書いてください");
    }
    let lane = match lane_override {
        Some(l) => l.to_string(),
        None => lane_addr_from_env().ok_or_else(|| {
            anyhow::anyhow!(
                "VP_REPO / VP_LANE が未設定です — lane の外からは `vp now --lane <repo>/root \"...\"` で明示してください"
            )
        })?,
    };
    let session = session_override.or_else(|| {
        std::env::var("VP_SESSION_KEY")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    let path = repo_path_for_lane(&lane, config)?;
    let resp = daemon_repo_request_blocking(
        crate::cli::daemon_port(),
        &path,
        "session_now",
        serde_json::json!({ "lane": lane, "session": session, "text": text }),
    )?;
    // 出力は最小 1 行（呼び手は AI = token 節約。人の確認にも足る情報だけ）。
    let session = resp
        .get("session")
        .and_then(serde_json::Value::as_u64)
        .map(|s| format!("#{s}"))
        .unwrap_or_default();
    println!("[vp now] ✓ {lane}{session}");
    Ok(())
}
