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

/// built-in stand 一覧（stand は Rust-native）。
pub fn list_stands() -> Vec<StandInfo> {
    vec![
        StandInfo {
            name: "echoes".to_string(),
            description: "VP Stand: Echoes 💬 — login shell の床 + Claude CLI 自動起動".to_string(),
        },
        StandInfo {
            name: "cursor".to_string(),
            description: "VP Stand: Cursor Agent 🖱️ — login shell の床 + cursor-agent 自動起動"
                .to_string(),
        },
        StandInfo {
            name: "shell".to_string(),
            description: "VP Stand: bare login shell 🤚 — 床のみ（旧 TheHand）".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// built-in stand が返り、 name が POST /api/lanes の `stand` field にそのまま使える形式。
    #[test]
    fn list_stands_returns_builtin() {
        let stands = list_stands();
        let names: Vec<&str> = stands.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["echoes", "cursor", "shell"]);
        assert!(stands.iter().all(|s| !s.description.is_empty()));
    }
}
