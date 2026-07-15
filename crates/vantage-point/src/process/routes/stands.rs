//! Stand discovery — process-proxy ask `stands_list`（doc 11 §4.1 PR-C, F6④ で HTTP 撤去）
//!
//! ## 役割
//!
//! sidebar の `+ Add Performer` で stand dropdown 表示するための data source。
//! entry point は World process-proxy ask `stands_list`
//! （`unison_server::handle_stands_list` → `list_stands`）。
//!
//! ## tmux decoupling PR2: 静的テーブル化
//!
//! 旧実装は `.mise/tasks/vp/stand/{name}` の script file 群を `mise tasks ls --json`
//! subprocess（~100ms + TTL 30s cache）で discovery していた。 stand script 層の廃止
//! （design doc §13.2 — stand は `stand_spawner::build_stand_command` の Rust-native 分岐）に伴い、
//! 一覧も built-in の静的テーブルになった。 mise / install-root 解決 / cache は全て不要。
//!
//! ## wire format
//!
//! ```json
//! {
//!   "stands": [
//!     {"name": "echoes", "description": "..."},
//!     {"name": "shell",  "description": "..."}
//!   ]
//! }
//! ```

use serde::Serialize;

/// Sidebar 側に返す stand 1 entry の schema。
#[derive(Debug, Clone, Serialize)]
pub struct StandInfo {
    /// stand 名（例: `"echoes"` / `"shell"`）
    pub name: String,
    /// 表示用の説明
    pub description: String,
}

/// built-in stand 一覧（engine 群 + shell）。
///
/// engine 軸は [`crate::echoes::EngineKind`] が SSOT（doc 37）— **ここに engine を手書きで
/// 足さない**。新 engine は `EngineKind::ALL` に足せば dropdown に自動で載る（cursor 追加時に
/// この静的 vec が取り残されて「GUI から作れない engine」が生まれた同型ミスの再発防止、
/// moody 指摘 2026-07-15）。engine を持たない `"shell"`（床のみ）だけをここで足す。
pub fn list_stands() -> Vec<StandInfo> {
    let mut stands: Vec<StandInfo> = crate::echoes::EngineKind::ALL
        .iter()
        .map(|k| StandInfo {
            name: k.stand_name().to_string(),
            description: k.description().to_string(),
        })
        .collect();
    stands.push(StandInfo {
        name: "shell".to_string(),
        description: "VP Stand: bare login shell 🤚 — 床のみ（旧 TheHand）".to_string(),
    });
    stands
}

#[cfg(test)]
mod tests {
    use super::*;

    /// built-in stand が返り、 name が lane_create の `stand` field にそのまま使える形式。
    /// engine 群は EngineKind::ALL 由来（= 新 engine を足すとこの assert も自然に伸びる）。
    #[test]
    fn list_stands_returns_builtin() {
        let stands = list_stands();
        let names: Vec<&str> = stands.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["echoes", "cursor", "codex", "agy", "shell"]);
        assert!(stands.iter().all(|s| !s.description.is_empty()));
    }
}
