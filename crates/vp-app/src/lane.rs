//! Lane — SP 内で起動する PTY セッションの抽象。
//!
//! 関連: `mem_1CaSpvE??` (VP Architecture: 3 段 Stand scope + Lane semantic)
//!
//! ## 構造 (memory rule)
//!
//! - **Lead Lane** (Project あたり 1 つ固定) ─ 中身は `LaneStand` (HD default | TH)
//! - **Worker Lane** (Project あたり n 個) ─ ccws cloned worktree、中身は `LaneStand`
//!
//! ## 表示形 (人間可読)
//!
//! - Lead:   `"vantage-point/lead"`
//! - Worker: `"vantage-point/worker/foo"`

use std::fmt;

/// Lane の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    /// Project あたり 1 つ固定
    Lead,
    /// Project あたり n 個 (ccws cloned worktree)
    Worker,
}

impl fmt::Display for LaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneKind::Lead => write!(f, "lead"),
            LaneKind::Worker => write!(f, "worker"),
        }
    }
}

/// Lane の address — Pool key として使う 3-tuple
///
/// 表示形 (`Display` 実装):
/// - Lead:   `"<project>/lead"`         例: `"vantage-point/lead"`
/// - Worker: `"<project>/worker/<name>"` 例: `"vantage-point/worker/foo"`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaneAddress {
    pub project: String,
    pub kind: LaneKind,
    /// Worker の場合のみ Some (人間可読)。Lead は None。
    pub name: Option<String>,
}

impl LaneAddress {
    /// Lead Lane を構築 (Project あたり 1 つ固定なので name 不要)
    pub fn lead(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Lead,
            name: None,
        }
    }

    /// Worker Lane を構築 (人間可読 name 必須)
    pub fn worker(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Worker,
            name: Some(name.into()),
        }
    }

    /// Lead 判定
    pub fn is_lead(&self) -> bool {
        matches!(self.kind, LaneKind::Lead)
    }
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (LaneKind::Lead, _) => write!(f, "{}/lead", self.project),
            (LaneKind::Worker, Some(n)) => write!(f, "{}/worker/{}", self.project, n),
            (LaneKind::Worker, None) => write!(f, "{}/worker/<unnamed>", self.project),
        }
    }
}

/// Lane で起動する Stand (LaneStand)
///
/// architecture: Lane と Stand は 1:1。Lane あたり 1 つの Stand が起動する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneStand {
    /// HD 📖 Heaven's Door — Claude CLI (default)
    HeavensDoor,
    /// TH ✋ The Hand — 素 shell (zsh / bash 等)
    TheHand,
    // 将来: GoldExperience(GeConfig) — eval-as-pane
    // 将来: PaisleyPark(PpConfig) — canvas 直 mount?
}

impl Default for LaneStand {
    fn default() -> Self {
        // memory rule: Lead/Worker の default は HD (Claude CLI)
        LaneStand::HeavensDoor
    }
}

impl fmt::Display for LaneStand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneStand::HeavensDoor => write!(f, "HD"),
            LaneStand::TheHand => write!(f, "TH"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_address_display() {
        let lead = LaneAddress::lead("vantage-point");
        assert_eq!(lead.to_string(), "vantage-point/lead");
        assert!(lead.is_lead());

        let worker = LaneAddress::worker("vantage-point", "foo");
        assert_eq!(worker.to_string(), "vantage-point/worker/foo");
        assert!(!worker.is_lead());
    }

    #[test]
    fn lane_address_eq_hash() {
        // 同じ project/kind/name なら同一視 (HashMap key として使えること)
        let a = LaneAddress::worker("vp", "foo");
        let b = LaneAddress::worker("vp", "foo");
        assert_eq!(a, b);

        let c = LaneAddress::worker("vp", "bar");
        assert_ne!(a, c);
    }

    #[test]
    fn lane_stand_default_is_hd() {
        // architecture rule: Lane の default Stand は HD (Claude CLI)
        assert_eq!(LaneStand::default(), LaneStand::HeavensDoor);
    }
}
