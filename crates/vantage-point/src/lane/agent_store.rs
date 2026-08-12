//! lane 単位の agent（engine 種別）永続 — sub の agent を repo 再起動をまたいで保つ
//!
//! ## 背景（bug mem_1Cd4M7i5Enp3HHMLVYayRe、2026-07-16）
//!
//! sub の agent は従来 in-memory（LanePool の LaneInfo.agent）にしか無く、repo 再起動後の
//! boot bootstrap（server.rs）は disk scan した全 sub を config の `default_agent`
//! （= conversation）で spawn していた。「sub の agent は repo 再起動をまたいで永続しない」の
//! 既知制約の実体で、GUI「+ Add Sub」の agent 落ちと同根。
//!
//! ## 設計（console_mode / session_store と同じ per-lane state file パターン）
//!
//! - **書き手**: `create_sub_orchestrated`（routes/lanes.rs）が agent 解決直後に record。
//!   全 create 入口（GUI watcher / MCP add_sub / CLI flow handoff）がここを通る choke point
//! - **読み手**: repo boot bootstrap（server.rs）が SpawnLane Cmd の agent に使う
//!   （記録不在 = 旧 lane / 手動 `vp lane new` は従来どおり config default に fallback）
//! - 置き場: `vp_state_dir()/lane_stands/<repo>__<lane>`（1 lane 1 file 1 行 = agent 名）
//! - 検証: agent 名は EngineKind の対応表に限らない自由文字列（"shell" 等 engine なし agent も
//!   ある）ため、形式検証（英数・ハイフン・アンダースコア）のみ。書き読み両側で同じ検証
//!   （session_store の共通原則）

use std::path::Path;

use super::session_store::SessionStore;

/// agent 名の正規形（英数 + ハイフン + アンダースコア、非空）。
/// 現行 agent（conversation/codex/grok/shell/hd）+ 撤去済み engine の legacy 文字列（cursor/agy 等）も
/// 自由文字列として全て通る（EngineKind allowlist に縛られない）。壊れた file を spawn に渡さない防壁。
fn is_valid_stand(agent: &str) -> bool {
    !agent.is_empty()
        && agent
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

const STORE: SessionStore = SessionStore::new("lane_stands", is_valid_stand);

/// agent を記録する（上書き、1 行）。形式外は書かずに Ok（既存の正常な記録を壊さない）。
pub fn record_in(base: &Path, repo: &str, lane: &str, agent: &str) -> std::io::Result<()> {
    STORE.record_in(base, repo, lane, agent)
}

/// 記録された agent を返す（無い / 形式外は None → caller が config default に fallback）。
pub fn last_in(base: &Path, repo: &str, lane: &str) -> Option<String> {
    STORE.last_in(base, repo, lane)
}

/// 記録を消す（未記録なら no-op）。lane 削除時の掃除用。
pub fn clear_in(base: &Path, repo: &str, lane: &str) -> std::io::Result<()> {
    STORE.clear_in(base, repo, lane)
}

/// 本番 base（vp_state_dir）での record（create_sub_orchestrated から呼ぶ）。
pub fn record(repo: &str, lane: &str, agent: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), repo, lane, agent)
}

/// 本番 base での last（repo boot bootstrap から呼ぶ）。
pub fn last(repo: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), repo, lane)
}

/// 本番 base での clear（lane 削除経路から呼ぶ）。
pub fn clear(repo: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), repo, lane)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// record → last の roundtrip + 記録不在は None（= caller が default に fallback する契約）。
    #[test]
    fn record_and_last_roundtrip_with_absent_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(last_in(tmp.path(), "vp", "feat-x"), None, "未記録は None");
        record_in(tmp.path(), "vp", "feat-x", "codex").expect("record");
        assert_eq!(
            last_in(tmp.path(), "vp", "feat-x").as_deref(),
            Some("codex")
        );
        // 上書き（GUI で作り直した時は新しい agent が勝つ）
        record_in(tmp.path(), "vp", "feat-x", "grok").expect("record 2");
        assert_eq!(last_in(tmp.path(), "vp", "feat-x").as_deref(), Some("grok"));
    }

    /// 形式検証: 現行 agent + 撤去済み engine の legacy 文字列は全て通り（graceful degradation —
    /// agent は EngineKind allowlist に縛られない自由文字列）、injection 形は書き読み両側で弾かれる。
    #[test]
    fn validation_accepts_known_stands_and_rejects_garbage() {
        for s in ["claude", "codex", "grok", "shell", "hd", "cursor", "agy"] {
            assert!(is_valid_stand(s), "現行/legacy agent は通る: {s}");
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "w1", "good-agent").expect("record");
        record_in(tmp.path(), "vp", "w1", "bad agent'; rm").expect("形式外は no-op");
        assert_eq!(
            last_in(tmp.path(), "vp", "w1").as_deref(),
            Some("good-agent"),
            "形式外の上書きから正常な記録が守られる"
        );
    }

    /// clear は冪等 + 他 lane を巻き添えにしない。
    #[test]
    fn clear_is_idempotent_and_scoped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "a", "codex").expect("record");
        record_in(tmp.path(), "vp", "b", "grok").expect("record");
        clear_in(tmp.path(), "vp", "a").expect("clear");
        clear_in(tmp.path(), "vp", "a").expect("二重 clear は no-op");
        assert_eq!(last_in(tmp.path(), "vp", "a"), None);
        assert_eq!(last_in(tmp.path(), "vp", "b").as_deref(), Some("grok"));
    }
}
