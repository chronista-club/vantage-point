//! Sidebar 表示用 data model (Architecture v4 Process recursive 移行版)
//!
//! 旧版 (~ 2026-04-26) では Mac 由来の `Pane` / `PaneKind` (Agent/Canvas/Preview/Shell) を
//! ProcessPaneState 内に持っていた。 Architecture v4 (mem_1CaTpCQH8iLJ2PasRcPjHv) で
//! **SP `/api/lanes` が SSOT** になったので、 vp-app local の Pane data model は撤去し、
//! このファイルは sidebar の accordion 状態 + widget payload + active selection だけを
//! 保持する役に絞った。
//!
//! ## sidebar 描画
//!
//! - Project (= Runtime Process) accordion: `ProcessPaneState`
//! - Lane (= Session Process / Lead/Worker): `SidebarState.lanes_by_project` (SP fetch 結果)
//! - Stand (= Worker Process / HD/TH/...): Lane の中身として並列 row
//!
//! つまり Pane は廃止、 階層は **Project → Lane → Stand** に統一。
//!
//! ## active selection
//!
//! `SidebarState.active_lane_address` で 1 つだけ active な Lane を持つ。
//! 形式は Lane address の Display 表現 (`"<project>/lead"` / `"<project>/worker/<name>"`)。

use serde::{Deserialize, Serialize};

/// プロジェクト単位の sidebar accordion 状態 (Architecture v4: Process kind=Runtime)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessPaneState {
    /// 正規化パス (HashMap key 兼)
    pub path: String,
    pub name: String,
    /// accordion 開閉状態 (default = 閉じる)
    #[serde(default)]
    pub expanded: bool,
    /// Process state (running/dead/spawning 等、 TheWorld fetch から merge される)
    /// sidebar JS が state badge 表示に使う
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// SP の listen port (Phase 2: Lane terminal connect で使う)。
    /// running 時のみ Some、 dead 時は None。 ProcessInfo.port を merge して保持。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl ProcessPaneState {
    /// 新規 project state (accordion は閉じた状態で生成)
    pub fn new(path: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            expanded: false,
            state: None, // ProcessesLoaded handler で fetch 後 merge
            port: None,  // 同上
        }
    }
}

/// sidebar 上部 widget slot に表示する widget の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WidgetKind {
    /// D — Activity / Stand Status (MVP)
    #[default]
    Activity,
    /// B — MsgBox / Inbox (P4 で実装、placeholder)
    Msgbox,
    /// C — Notes / Scratchpad (P4 で実装、placeholder)
    Notes,
}

/// Activity widget の payload
///
/// 5-10 秒間隔で Rust 側が `/api/health` + `/api/world/projects` +
/// `/api/world/processes` を fetch して更新、sidebar に push する。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    /// TheWorld daemon 到達可否
    pub world_online: bool,
    /// `/api/health` から取得 (オフライン時 None)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub world_version: Option<String>,
    /// daemon の起動時刻 (ISO 8601、オフライン時 None)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub world_started_at: Option<String>,
    /// 登録プロジェクト数
    pub project_count: usize,
    /// 稼働中 process 数 (`/api/world/processes`)
    pub running_process_count: usize,
}

/// Sidebar 全体の state (sidebar webview に渡す)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SidebarState {
    /// Runtime Process (= 旧 "projects") の list
    /// Architecture v4: mem_1CaTpCQH8iLJ2PasRcPjHv、JSON wire は serde alias で互換維持
    #[serde(alias = "projects")]
    pub processes: Vec<ProcessPaneState>,
    /// 現在表示中の widget kind
    #[serde(default)]
    pub widget: WidgetKind,
    /// Activity widget payload (widget == Activity の時のみ有効)
    #[serde(default)]
    pub activity: ActivitySnapshot,
    /// project_path → Lane list (SP `/api/lanes` から fetch)
    /// 関連 memory: mem_1CaSugEk1W2vr5TAdfDn5D (多 scope architecture)
    /// 起動時に再 fetch されるので disk persistence は実質意味薄いが、Serialize は維持
    #[serde(default)]
    pub lanes_by_project: std::collections::HashMap<String, Vec<crate::client::LaneInfo>>,
    /// 現在 active な Lane の address (Display 形 `"<project>/lead"` 等)
    /// app 全体で 1 つだけ。 `lane:select` IPC で更新される。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lane_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_state_starts_collapsed() {
        let p = ProcessPaneState::new("/path", "demo");
        assert_eq!(p.path, "/path");
        assert_eq!(p.name, "demo");
        assert!(!p.expanded);
        assert!(p.state.is_none());
    }

    #[test]
    fn sidebar_state_serializes_round_trip() {
        let mut s = SidebarState::default();
        s.processes.push(ProcessPaneState::new("/a", "alpha"));
        s.activity.world_online = true;
        s.activity.project_count = 1;
        s.active_lane_address = Some("alpha/lead".into());
        let json = serde_json::to_string(&s).unwrap();
        let parsed: SidebarState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.processes.len(), 1);
        assert!(parsed.activity.world_online);
        assert_eq!(parsed.widget, WidgetKind::Activity);
        assert_eq!(parsed.active_lane_address.as_deref(), Some("alpha/lead"));
    }

    #[test]
    fn sidebar_state_accepts_legacy_projects_alias() {
        // 旧 disk persistence (key="projects") から読めること
        let json = r#"{"projects":[{"path":"/x","name":"x","expanded":true}]}"#;
        let parsed: SidebarState = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.processes.len(), 1);
        assert_eq!(parsed.processes[0].path, "/x");
        assert!(parsed.processes[0].expanded);
    }
}
