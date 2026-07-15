//! lane 単位の stand（engine 種別）永続 — performer の stand を SP 再起動をまたいで保つ
//!
//! ## 背景（bug mem_1Cd4M7i5Enp3HHMLVYayRe、2026-07-16）
//!
//! performer の stand は従来 in-memory（LanePool の LaneInfo.stand）にしか無く、SP 再起動後の
//! boot bootstrap（server.rs）は disk scan した全 performer を config の `default_stand`
//! （= echoes）で spawn していた。cursor-engine.md の既知制約「performer の stand は SP 再起動を
//! またいで永続しない」の実体で、GUI「+ Add Performer」の stand 落ちと同根。
//!
//! ## 設計（console_mode / session_store と同じ per-lane state file パターン）
//!
//! - **書き手**: `create_performer_orchestrated`（routes/lanes.rs）が stand 解決直後に record。
//!   全 create 入口（GUI watcher / MCP add_performer / CLI flow handoff）がここを通る choke point
//! - **読み手**: SP boot bootstrap（server.rs）が SpawnLane Cmd の stand に使う
//!   （記録不在 = 旧 lane / 手動 `vp lane new` は従来どおり config default に fallback）
//! - 置き場: `vp_state_dir()/lane_stands/<project>__<lane>`（1 lane 1 file 1 行 = stand 名）
//! - 検証: stand 名は EngineKind の対応表に限らない自由文字列（"shell" 等 engine なし stand も
//!   ある）ため、形式検証（英数・ハイフン・アンダースコア）のみ。書き読み両側で同じ検証
//!   （session_store の共通原則）

use std::path::Path;

use super::session_store::SessionStore;

/// stand 名の正規形（英数 + ハイフン + アンダースコア、非空）。
/// 既知 stand（echoes/cursor/codex/agy/shell/hd）は全て通る。壊れた file を spawn に渡さない防壁。
fn is_valid_stand(stand: &str) -> bool {
    !stand.is_empty()
        && stand
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

const STORE: SessionStore = SessionStore::new("lane_stands", is_valid_stand);

/// stand を記録する（上書き、1 行）。形式外は書かずに Ok（既存の正常な記録を壊さない）。
pub fn record_in(base: &Path, project: &str, lane: &str, stand: &str) -> std::io::Result<()> {
    STORE.record_in(base, project, lane, stand)
}

/// 記録された stand を返す（無い / 形式外は None → caller が config default に fallback）。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    STORE.last_in(base, project, lane)
}

/// 記録を消す（未記録なら no-op）。lane 削除時の掃除用。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    STORE.clear_in(base, project, lane)
}

/// 本番 base（vp_state_dir）での record（create_performer_orchestrated から呼ぶ）。
pub fn record(project: &str, lane: &str, stand: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, stand)
}

/// 本番 base での last（SP boot bootstrap から呼ぶ）。
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base での clear（lane 削除経路から呼ぶ）。
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
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
        // 上書き（GUI で作り直した時は新しい stand が勝つ）
        record_in(tmp.path(), "vp", "feat-x", "cursor").expect("record 2");
        assert_eq!(
            last_in(tmp.path(), "vp", "feat-x").as_deref(),
            Some("cursor")
        );
    }

    /// 形式検証: 既知 stand は全て通り、injection 形は書き読み両側で弾かれる。
    #[test]
    fn validation_accepts_known_stands_and_rejects_garbage() {
        for s in ["echoes", "cursor", "codex", "agy", "shell", "hd"] {
            assert!(is_valid_stand(s), "既知 stand は通る: {s}");
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "w1", "good-stand").expect("record");
        record_in(tmp.path(), "vp", "w1", "bad stand'; rm").expect("形式外は no-op");
        assert_eq!(
            last_in(tmp.path(), "vp", "w1").as_deref(),
            Some("good-stand"),
            "形式外の上書きから正常な記録が守られる"
        );
    }

    /// clear は冪等 + 他 lane を巻き添えにしない。
    #[test]
    fn clear_is_idempotent_and_scoped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "a", "codex").expect("record");
        record_in(tmp.path(), "vp", "b", "cursor").expect("record");
        clear_in(tmp.path(), "vp", "a").expect("clear");
        clear_in(tmp.path(), "vp", "a").expect("二重 clear は no-op");
        assert_eq!(last_in(tmp.path(), "vp", "a"), None);
        assert_eq!(last_in(tmp.path(), "vp", "b").as_deref(), Some("cursor"));
    }
}
