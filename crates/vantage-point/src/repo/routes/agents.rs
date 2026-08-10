//! Agent discovery — repo-proxy ask `agents_list`（doc 11 §4.1 PR-C, F6④ で HTTP 撤去）
//!
//! ## 役割
//!
//! sidebar の `+ Add Sub` で agent dropdown 表示するための data source。
//! entry point は daemon repo-proxy ask `agents_list`
//! （`unison_server::handle_stands_list` → `list_agents`）。
//!
//! ## tmux decoupling PR2: 静的テーブル化
//!
//! 旧実装は `.mise/tasks/vp/agent/{name}` の script file 群を `mise tasks ls --json`
//! subprocess（~100ms + TTL 30s cache）で discovery していた。 agent script 層の廃止
//! （design doc §13.2 — agent は `agent_spawner::build_agent_command` の Rust-native 分岐）に伴い、
//! 一覧も built-in の静的テーブルになった。 mise / install-root 解決 / cache は全て不要。
//!
//! ## wire format
//!
//! ```json
//! {
//!   "agents": [
//!     {"name": "claude", "description": "..."},
//!     {"name": "shell",  "description": "..."}
//!   ]
//! }
//! ```

use serde::Serialize;

/// Sidebar 側に返す agent 1 entry の schema。
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    /// agent 名（例: `"claude"` / `"shell"`）
    pub name: String,
    /// 表示用の説明
    pub description: String,
    /// gui（chat GUI）の host を持つか（doc 38 Phase 3、additive field）。
    /// chat 系 UI（session tab の「+」menu）はこれで shell（engine なし）を除外する —
    /// dogfood で「作れるが submit がエラーになるだけの dead-end tab」が実発生したため。
    /// sidebar の Add Sub（lane 作成）は全 agent が正当なので filter しない。
    pub chat_capable: bool,
}

/// built-in agent 一覧（engine 群 + shell）。
///
/// engine 軸は [`crate::conversation::EngineKind`] が SSOT（doc 37）— **ここに engine を手書きで
/// 足さない**。新 engine は `EngineKind::ALL` に足せば dropdown に自動で載る（cursor 追加時に
/// この静的 vec が取り残されて「GUI から作れない engine」が生まれた同型ミスの再発防止、
/// moody 指摘 2026-07-15）。engine を持たない `"shell"`（shell のみ）だけをここで足す。
pub fn list_agents() -> Vec<AgentInfo> {
    let mut agents: Vec<AgentInfo> = crate::conversation::EngineKind::ALL
        .iter()
        .map(|k| AgentInfo {
            name: k.agent_name().to_string(),
            description: k.description().to_string(),
            chat_capable: k.chat_capable(),
        })
        .collect();
    agents.push(AgentInfo {
        name: "shell".to_string(),
        description: "VP Agent: bare login shell 🤚 — shell のみ（旧 TheHand）".to_string(),
        chat_capable: false,
    });
    agents
}

#[cfg(test)]
mod tests {
    use super::*;

    /// built-in agent が返り、 name が lane_create の `agent` field にそのまま使える形式。
    /// engine 群は EngineKind::ALL 由来（= 新 engine を足すとこの assert も自然に伸びる）。
    #[test]
    fn list_stands_returns_builtin() {
        let agents = list_agents();
        let names: Vec<&str> = agents.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["claude", "codex", "grok", "opencode", "shell"]);
        assert!(agents.iter().all(|s| !s.description.is_empty()));
    }

    /// doc 38 Phase 3: chat_capable が EngineKind の能力表と一致し、shell は false。
    /// 「+」menu の filter がこの flag を信頼する（GUI に engine 対応表を複製しない）。
    #[test]
    fn chat_capable_follows_engine_capability_table() {
        let agents = list_agents();
        let cap = |name: &str| agents.iter().find(|s| s.name == name).unwrap().chat_capable;
        assert!(cap("claude"));
        assert!(cap("codex"));
        assert!(cap("grok"), "grok は常駐 AcpAgentHost（doc 42）");
        assert!(
            cap("opencode"),
            "opencode は grok と同じ常駐 AcpAgentHost（doc 43）"
        );
        assert!(!cap("shell"), "shell は engine なし（shell のみ）");
    }
}
