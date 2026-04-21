//! CreoUI — 見せ方 hint (Component 単位、2026-04-22 確定)
//!
//! 1 つの [`super::content::CreoContent`] に対して 1 つの `CreoUI` が付く。
//! Pane / Canvas 全体の layout hint は将来 `ContainerUI` として分離予定 (R0 では切らない)。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::event::ActorRef;

/// Render hint attached to a single CreoContent component.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreoUI {
    #[serde(default)]
    pub layout: CreoLayout,

    #[serde(default)]
    pub emphasis: CreoEmphasis,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<CreoPlacement>,

    /// Reserved — multi-user Canvas (VP-63+) で活性化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<CreoOwnership>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CreoLayout {
    #[default]
    Masonry,
    Grid,
    Focus,
    Stream,
    Sidebar,
    Inline,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreoEmphasis {
    #[serde(default)]
    pub pinned: bool,
    /// -2 .. +2 (0 = normal)
    #[serde(default)]
    pub priority: i8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreoPlacement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f32>,
}

/// Reserved — 将来の multi-user Canvas で `owner` / `pinned_by` を使う。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreoOwnership {
    pub owner: ActorRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_by: Vec<ActorRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_default_is_masonry() {
        assert_eq!(CreoUI::default().layout, CreoLayout::Masonry);
    }

    #[test]
    fn layout_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&CreoLayout::Focus).unwrap(),
            "\"focus\""
        );
        assert_eq!(
            serde_json::to_string(&CreoLayout::Sidebar).unwrap(),
            "\"sidebar\""
        );
    }

    #[test]
    fn ui_minimal_skips_all_none() {
        let ui = CreoUI::default();
        let json = serde_json::to_value(&ui).unwrap();
        assert_eq!(json["layout"], "masonry");
        assert!(json.get("placement").is_none());
        assert!(json.get("ownership").is_none());
        assert!(json.get("expires_at").is_none());
        assert!(json.get("tags").is_none(), "empty vec should be skipped");
    }

    #[test]
    fn emphasis_priority_range_is_i8() {
        let e = CreoEmphasis {
            pinned: true,
            priority: 2,
            badge: Some("hot".into()),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["pinned"], true);
        assert_eq!(json["priority"], 2);
        assert_eq!(json["badge"], "hot");
    }
}
