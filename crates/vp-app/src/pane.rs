//! Pane data model (VP-95 / VP-94 epic Phase 1)
//!
//! vp-app の sidebar accordion + main area dynamic pane の土台になる data model。
//! Mac の `apple/VantagePoint/Sources/PaneModel.swift` とは設計が違う:
//!
//! - Mac: 再帰 `PaneNode` tree + `PaneLayoutMap` (LayoutKind: h/vSplit/overlay/tab)
//! - Windows: **flat な project → panes 構造** + main area は単一 wry WebView 内で
//!   HTML/CSS split (β 戦略)
//!
//! Mac の `PaneKind` (agent/canvas/preview/shell) とは命名揃える、ただし split
//! 構造は持たない。split は P3 で main area HTML 側に追加する。
//!
//! IPC との関係:
//! - `SidebarState` を JSON serialize して sidebar webview に渡す
//! - sidebar webview からは `pane:select` / `project:toggle` / `pane:add` を IPC で受ける
//! - widget slot は `WidgetKind` で切替、payload は kind ごとに別 (Activity は
//!   `ActivitySnapshot`、後続 B/C は別構造)

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Pane の種別 (Mac の `PaneKind` と命名揃える)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    /// Heaven's Door 📖 — Claude CLI セッション
    Agent,
    /// Paisley Park 🧭 — Markdown / HTML / 画像
    Canvas,
    /// file / image / URL の read-only preview
    Preview,
    /// 素 shell PTY (将来 WSL distro switcher と組み合わせる、VP-97)
    Shell,
}

impl PaneKind {
    /// sidebar 表示用の icon (絵文字、後で SF Symbol 風 SVG に置換可)
    pub fn icon(self) -> &'static str {
        match self {
            Self::Agent => "📖",
            Self::Canvas => "🧭",
            Self::Preview => "📄",
            Self::Shell => "⚙",
        }
    }

    /// sidebar 表示用のデフォルト label
    pub fn default_label(self) -> &'static str {
        match self {
            Self::Agent => "Lead Agent",
            Self::Canvas => "Canvas",
            Self::Preview => "Preview",
            Self::Shell => "Shell",
        }
    }
}

/// 1 つの pane content unit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: String, // UUID v4 文字列
    pub kind: PaneKind,
    pub title: String,
    /// Preview kind の URL (file:// or https://)、他 kind では None
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preview_url: Option<String>,
}

impl Pane {
    /// 新しい pane を生成 (新規 UUID 付与)
    pub fn new(kind: PaneKind, title: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind,
            title: title.into(),
            preview_url: None,
        }
    }

    /// kind デフォルト label で生成
    pub fn with_default_label(kind: PaneKind) -> Self {
        Self::new(kind, kind.default_label())
    }
}

/// プロジェクト単位の pane state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPaneState {
    /// 正規化パス (HashMap key 兼)
    pub path: String,
    pub name: String,
    pub panes: Vec<Pane>,
    /// 現在 active な pane id (None = pane 未選択)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub active_pane_id: Option<String>,
    /// accordion 開閉状態 (default = 閉じる)
    #[serde(default)]
    pub expanded: bool,
}

impl ProjectPaneState {
    /// 新規 project state (Lead Agent 1 つ + accordion 閉じる)
    pub fn new(path: impl Into<String>, name: impl Into<String>) -> Self {
        let lead = Pane::with_default_label(PaneKind::Agent);
        let lead_id = lead.id.clone();
        Self {
            path: path.into(),
            name: name.into(),
            panes: vec![lead],
            active_pane_id: Some(lead_id),
            expanded: false,
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
    pub projects: Vec<ProjectPaneState>,
    /// 現在表示中の widget kind
    pub widget: WidgetKind,
    /// Activity widget payload (widget == Activity の時のみ有効)
    #[serde(default)]
    pub activity: ActivitySnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_kind_serializes_lowercase() {
        let json = serde_json::to_string(&PaneKind::Agent).unwrap();
        assert_eq!(json, "\"agent\"");
        let parsed: PaneKind = serde_json::from_str("\"shell\"").unwrap();
        assert_eq!(parsed, PaneKind::Shell);
    }

    #[test]
    fn project_state_starts_with_lead_agent() {
        let p = ProjectPaneState::new("/path", "demo");
        assert_eq!(p.panes.len(), 1);
        assert_eq!(p.panes[0].kind, PaneKind::Agent);
        assert_eq!(p.active_pane_id.as_ref(), Some(&p.panes[0].id));
        assert!(!p.expanded);
    }

    #[test]
    fn sidebar_state_serializes_round_trip() {
        let mut s = SidebarState::default();
        s.projects.push(ProjectPaneState::new("/a", "alpha"));
        s.activity.world_online = true;
        s.activity.project_count = 1;
        let json = serde_json::to_string(&s).unwrap();
        let parsed: SidebarState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.projects.len(), 1);
        assert!(parsed.activity.world_online);
        assert_eq!(parsed.widget, WidgetKind::Activity);
    }
}
