//! CC activity poll (R3-a / Phase A、 設計 mem_1CbXZyCiqrdgteGhRFDaHW)
//!
//! `claude agents --json` で live session を取得し、 delivery loop に
//! LaneActivity (設計 mem_1CbvcJj4ppU3QKH9d7xMpT 決定 D4) を供給する。
//! **read-only の poll のみ** — dispatch (`--bg`) は R3-c。
//!
//! ## Research Preview の schema 変動リスク
//!
//! 「全パース防御 + 失敗 = None」で抱える。 呼び出し側 (DeliveryActor) は None を
//! degraded mode (R2-b 挙動: lane Running → nudge) に fallback するため、 schema が
//! 変わっても配信は止まらず精度が落ちるだけ。
//!
//! ## 実機確認済み schema (claude 2.1.173、 2026-06-11)
//!
//! top-level は配列。 interactive = `{pid, cwd, kind:"interactive", sessionId,
//! status: idle|busy|waiting}` / background = `{cwd, kind:"background", sessionId,
//! state: done|failed|blocked, id, name}`。

// =============================================================================
// data / calculations — parse は純関数 (単体テスト対象)
// =============================================================================

/// interactive session の CC 状態 (agents --json の status)
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CcState {
    Idle,
    Busy,
    Waiting,
}

/// 1 interactive session (delivery 判定 + R3-b の resume 土台に使う最小構成)
#[derive(Debug, Clone)]
pub(crate) struct CcSession {
    pub cwd: String,
    pub state: CcState,
    /// `claude --resume <id>` 再利用の収穫経路 (R3-b で LaneInfo へ保存予定)
    #[allow(dead_code)] // R3-b で保存・利用する (収穫だけ先行)
    pub session_id: Option<String>,
}

/// agents --json の出力を parse する (純関数)。 interactive のみ返す。
///
/// 防御方針: top-level が配列でない / JSON でない → None (= schema 変動とみなし
/// degraded fallback)。 entry 単位の欠落 field は skip、 未知 status は Busy 扱い
/// (保守的 = nudge しない方に倒す。 誤 nudge より誤待機の方が無害 — 30s tick で再判定される)。
pub(crate) fn parse_agents_json(raw: &str) -> Option<Vec<CcSession>> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let arr = v.as_array()?;
    let mut out = Vec::new();
    for item in arr {
        if item.get("kind").and_then(|k| k.as_str()) != Some("interactive") {
            continue;
        }
        let Some(cwd) = item.get("cwd").and_then(|c| c.as_str()) else {
            continue;
        };
        let state = match item.get("status").and_then(|s| s.as_str()) {
            Some("idle") => CcState::Idle,
            Some("waiting") => CcState::Waiting,
            _ => CcState::Busy,
        };
        out.push(CcSession {
            cwd: cwd.to_string(),
            state,
            session_id: item
                .get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from),
        });
    }
    Some(out)
}

/// cwd で session を引く (純関数)。 同一 cwd に複数 session がある場合は
/// 最初の 1 件 (agents --json の列挙順) を使う。
pub(crate) fn state_for_cwd(sessions: &[CcSession], cwd: &str) -> Option<CcState> {
    sessions.iter().find(|s| s.cwd == cwd).map(|s| s.state)
}

// =============================================================================
// actions — poll の I/O 層
// =============================================================================

/// `claude agents --json` を実行して parse する。 あらゆる失敗は None
/// (= degraded fallback)。 delivery pulse を block しないよう 5s timeout。
pub(crate) async fn poll_cc_activity() -> Option<Vec<CcSession>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new("claude")
            .args(["agents", "--json"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_agents_json(&String::from_utf8_lossy(&output.stdout))
}

// =============================================================================
// Tests — 純関数のみ (poll の I/O は E2E で検証)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_interactive_sessions() {
        let json = r#"[
            {"pid":null,"cwd":"/r/nexus","kind":"background","sessionId":"bg-1","state":"blocked","id":"x","name":"t"},
            {"pid":123,"cwd":"/r/vp","kind":"interactive","sessionId":"s-1","status":"idle"},
            {"pid":124,"cwd":"/r/nexus","kind":"interactive","sessionId":"s-2","status":"busy"},
            {"pid":125,"cwd":"/r/hub","kind":"interactive","sessionId":"s-3","status":"waiting"},
            {"pid":126,"cwd":"/r/odd","kind":"interactive","sessionId":"s-4","status":"brand-new-state"}
        ]"#;
        let sessions = parse_agents_json(json).expect("parse");
        assert_eq!(sessions.len(), 4, "interactive のみ (background は対象外)");
        assert_eq!(sessions[0].cwd, "/r/vp");
        assert_eq!(sessions[0].state, CcState::Idle);
        assert_eq!(sessions[0].session_id.as_deref(), Some("s-1"));
        assert_eq!(sessions[1].state, CcState::Busy);
        assert_eq!(sessions[2].state, CcState::Waiting);
        // 未知 status は Busy 扱い (保守的 = nudge しない方に倒す)
        assert_eq!(sessions[3].state, CcState::Busy);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_agents_json("not json").is_none());
        // 配列以外は schema 変動とみなす (degraded fallback)
        assert!(parse_agents_json(r#"{"agents":[]}"#).is_none());
        assert_eq!(parse_agents_json("[]").expect("空配列は valid").len(), 0);
    }

    #[test]
    fn activity_lookup_matches_by_cwd() {
        let sessions = vec![CcSession {
            cwd: "/r/vp".into(),
            state: CcState::Idle,
            session_id: Some("s-1".into()),
        }];
        assert_eq!(state_for_cwd(&sessions, "/r/vp"), Some(CcState::Idle));
        assert_eq!(state_for_cwd(&sessions, "/r/other"), None);
    }
}
