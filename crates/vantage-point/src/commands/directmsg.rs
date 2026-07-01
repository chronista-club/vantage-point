//! `vp directmsg` subcommand — tmux send-keys ベースの直接メッセージ（補助チャネル）。
//!
//! VP 開発プロセス redesign（spec: creo-memories `mem_1CbCMuLdGwpN2fJWbHQtbA`）の
//! `directmsg`。agent 間通信は wiremsg（主・retained・後から見返せる）が既定で、
//! directmsg はその補助 — **ephemeral** かつ **SP / DB 非依存**で、緊急時（DB 停止で
//! wiremsg が機能しない）や、残さなくてよい一時連絡に使う。
//!
//! 宛先 lane の tmux session を `tmux list-sessions` の prefix 一致で引き、
//! `tmux send-keys` でその pane に直接テキストを投入する（SP も DB も経由しない）。
//!
//! ```bash
//! vp directmsg vantage-point/conductor "ビルド通った？"
//! vp directmsg vantage-point/performer/sub "止めて" --no-enter
//! ```

use anyhow::{Context, Result, bail};

use crate::process::lanes_state::LaneAddress;

/// lane address 文字列を `LaneAddress` にパースする。
///
/// 受理形式（`LaneAddress` の Display 形）:
/// - `<project>/conductor`
/// - `<project>/performer/<name>`
fn parse_lane(s: &str) -> Result<LaneAddress> {
    let parts: Vec<&str> = s.split('/').collect();
    match parts.as_slice() {
        [project, "conductor"] if !project.is_empty() => Ok(LaneAddress::conductor(*project)),
        [project, "performer", name] if !project.is_empty() && !name.is_empty() => {
            Ok(LaneAddress::performer(*project, *name))
        }
        _ => bail!(
            "lane address の形式が不正: '{}' — '<project>/conductor' か '<project>/performer/<name>' を指定",
            s
        ),
    }
}

/// prefix 一致で lane の tmux session 名を引く。
///
/// lane ごとに 1 stand = 1 session が原則。複数該当時は先頭を使い警告する。
fn resolve_session(prefix: &str) -> Result<String> {
    let out = crate::tmux::tmux_command()
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .context("tmux list-sessions の実行に失敗（tmux 未インストール / server 未起動）")?;
    if !out.status.success() {
        bail!(
            "tmux list-sessions が失敗: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let matches: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|s| s.starts_with(prefix))
        .collect();
    match matches.as_slice() {
        [] => bail!(
            "対象 lane の tmux session が見つからない（prefix '{}'）",
            prefix
        ),
        [one] => Ok((*one).to_string()),
        many => {
            eprintln!(
                "[vp directmsg] prefix '{}' に複数 session が一致: {:?} — 先頭を使用",
                prefix, many
            );
            Ok(many[0].to_string())
        }
    }
}

/// Entry point — main.rs から呼び出される。
pub fn run(lane: &str, text: &str, enter: bool) -> Result<()> {
    let addr = parse_lane(lane)?;
    let session = resolve_session(&addr.tmux_session_prefix())?;

    // `-l` (literal): text を key 名として解釈させず、そのまま流し込む。
    let status = crate::tmux::tmux_command()
        .args(["send-keys", "-t", &session, "-l", text])
        .status()
        .context("tmux send-keys の実行に失敗")?;
    if !status.success() {
        bail!("tmux send-keys が失敗（session={}）", session);
    }

    // Enter は別 send-keys で送る（`-l` と混ぜると Enter も literal 文字列扱いになる）。
    if enter {
        let status = crate::tmux::tmux_command()
            .args(["send-keys", "-t", &session, "Enter"])
            .status()
            .context("tmux send-keys Enter の実行に失敗")?;
        if !status.success() {
            bail!("tmux send-keys Enter が失敗（session={}）", session);
        }
    }

    println!("[vp directmsg] sent → {} ({})", addr, session);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conductor_address() {
        let a = parse_lane("vantage-point/conductor").unwrap();
        assert_eq!(a.tmux_session_prefix(), "vp-vantage-point-conductor-");
    }

    #[test]
    fn parse_performer_address() {
        let a = parse_lane("vantage-point/performer/sub").unwrap();
        assert_eq!(a.tmux_session_prefix(), "vp-vantage-point-sub-");
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_lane("bogus").is_err());
        assert!(parse_lane("proj/").is_err());
        assert!(parse_lane("/conductor").is_err());
        assert!(parse_lane("proj/performer/").is_err());
        assert!(parse_lane("proj/unknown").is_err());
    }

    /// prefix が `tmux_session_name` の戻り値の実 prefix であることを保証
    /// （`resolve_session` の prefix 一致が成立する前提）。
    #[test]
    fn prefix_is_prefix_of_session_name() {
        let a = parse_lane("creo.memories/conductor").unwrap();
        let name = a.tmux_session_name("hd");
        assert!(name.starts_with(&a.tmux_session_prefix()));
    }
}
