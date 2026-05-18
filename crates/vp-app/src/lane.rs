//! Lane — SP 内で起動する PTY セッションの抽象。
//!
//! 関連: `mem_1CaSpvE??` (VP Architecture: 3 段 Stand scope + Lane semantic)
//!
//! ## 構造 (memory rule)
//!
//! - **Lead Lane** (Project あたり 1 つ固定) ─ 中身は `LaneStand` (HD default | TH)
//! - **Worker Lane** (Project あたり n 個) ─ lane cloned worktree、中身は `LaneStand`
//!
//! ## 表示形 (人間可読)
//!
//! - Lead:   `"vantage-point/lead"`
//! - Worker: `"vantage-point/worker/foo"`

use std::fmt;

// v1.0 柱 2 PR-1: ts-rs で sidebar wire 型を TS に export (test build 時のみ)。
#[cfg(test)]
use ts_rs::TS;

/// Lane の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    /// Project あたり 1 つ固定
    Lead,
    /// Project あたり n 個 (lane cloned worktree)
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

/// Lane address の **wire (JSON) 表現** — SP `/api/lanes` レスポンス要素の field
///
/// `LaneAddress` (domain enum-based) と区別する役割:
/// - **`LaneAddressWire`** = JSON 入口、 `kind` を String のまま保持 (vantage-point 側の
///   "lead"/"worker"/将来の任意値 をそのまま deserialize 受け)
/// - **`LaneAddress`** = domain 型、 `LaneKind` enum で型安全な分岐
///
/// 比較・Display には:
/// - 文字列直接ほしい時 → `LaneAddressWire::key()` (旧 `app.rs::lane_address_key` 互換)
/// - 型安全な domain 形ほしい時 → `LaneAddress::from(&wire)` で変換
///
/// R-0 (`docs/design/11-vp-app-refactor.md` § 3.0a / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
///   従来 `client.rs` に居た定義を本 module に統合 (G2 解消、 3 重実装の 1 元化)。
///
/// 関連 memory: mem_1CaSugEk1W2vr5TAdfDn5D (多 scope architecture)、
/// mem_1CaSuu8xMyWqXzLiKHmYdV (使用範囲ベース scope rule)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "web-bundle/src/generated/"))]
pub struct LaneAddressWire {
    #[serde(default)]
    pub project: String,
    /// "lead" | "worker"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl LaneAddressWire {
    /// Display 形 (`<project>/lead` / `<project>/worker/<name>`) を文字列で返す。
    ///
    /// 旧 `app.rs::lane_address_key` を吸収。 JS 側 `laneAddressKey()` と完全に一致させる
    /// (active 比較に使うため、 byte-for-byte 同一が要件)。
    ///
    /// `LaneAddress::Display` と微妙に挙動が違うことに注意:
    /// - `Wire::key()` は kind string をそのまま保持 (unknown kind "magic" → `"project/magic"`)
    /// - `LaneAddress::Display` は LaneKind enum 経由 (unknown kind は From で Lead に collapse)
    ///
    /// JSON wire 互換 (旧 `lane_address_key` と byte-for-byte 同一) を要する場面では本 method、
    /// 型安全な domain 表現が要る場面では `LaneAddress::from(&wire).to_string()` を使う。
    pub fn key(&self) -> String {
        match (self.kind.as_str(), self.name.as_deref()) {
            ("worker", Some(n)) => format!("{}/worker/{}", self.project, n),
            ("worker", None) => format!("{}/worker/<unnamed>", self.project),
            _ => format!("{}/{}", self.project, self.kind),
        }
    }
}

/// Wire (JSON) 形から domain 形 (enum-based) への変換。
///
/// 注意: unknown kind ("magic" 等) は fallback で `LaneKind::Lead` に collapse される
/// (= 情報損失)。 raw kind 文字列を保ちたい用途では `LaneAddressWire::key()` を直接使う。
impl From<&LaneAddressWire> for LaneAddress {
    fn from(wire: &LaneAddressWire) -> Self {
        let kind = match wire.kind.as_str() {
            "worker" => LaneKind::Worker,
            _ => LaneKind::Lead,
        };
        Self {
            project: wire.project.clone(),
            kind,
            name: wire.name.clone(),
        }
    }
}

// `LaneStand` enum は doc 11 PR-B で削除。 stand 識別子は wire 経由で String として
// 受け取る (`crate::client::LaneInfo.stand: String`)、 vp-app 内では直接文字列で扱う。
// 表示用 mapping (旧 Display impl の "HD" / "TH") は app.rs の standDisplayName JS 関数に
// 集約 (`hd` / `shell` / `tmux` / その他 fallback)。

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

    // 旧 lane_stand_default_is_hd test は doc 11 PR-B で廃止。
    // default の概念は config.kdl (`default-stand`) + handler-side の
    // `Config::default_stand_or_echoes()` (PR-pre2 で "hd" → "echoes" rename) に移ったため、
    // vp-app crate test では検証しない。

    /// R-0 wire compat: `LaneAddressWire::key()` は旧 `app.rs::lane_address_key` と
    /// byte-for-byte 同一でなければならない (JS 側 `laneAddressKey()` の active 比較で使うため)。
    #[test]
    fn lane_address_wire_key_lead() {
        let w = LaneAddressWire {
            project: "vantage-point".into(),
            kind: "lead".into(),
            name: None,
        };
        assert_eq!(w.key(), "vantage-point/lead");
    }

    #[test]
    fn lane_address_wire_key_worker_named() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "worker".into(),
            name: Some("foo".into()),
        };
        assert_eq!(w.key(), "vp/worker/foo");
    }

    #[test]
    fn lane_address_wire_key_worker_unnamed() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "worker".into(),
            name: None,
        };
        assert_eq!(w.key(), "vp/worker/<unnamed>");
    }

    #[test]
    fn lane_address_wire_key_unknown_kind_passthrough() {
        // 旧 `lane_address_key` の fallback 挙動: unknown kind は kind string をそのまま使う。
        // Wire::key() はこれを保持する (Display impl と異なる、 詳細は doc 参照)。
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "magic".into(),
            name: None,
        };
        assert_eq!(w.key(), "vp/magic");
    }

    #[test]
    fn lane_address_from_wire_lead() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "lead".into(),
            name: None,
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.kind, LaneKind::Lead);
        assert_eq!(addr.project, "vp");
        assert!(addr.name.is_none());
    }

    #[test]
    fn lane_address_from_wire_worker() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "worker".into(),
            name: Some("foo".into()),
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.kind, LaneKind::Worker);
        assert_eq!(addr.name.as_deref(), Some("foo"));
    }

    #[test]
    fn lane_address_from_wire_unknown_collapses_to_lead() {
        // 情報損失の意図的契約: `LaneAddress` enum で表現できない kind は Lead に折りたたむ。
        // raw kind 保持が必要なら `Wire::key()` 直接使用。
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "magic".into(),
            name: None,
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.kind, LaneKind::Lead);
    }
}
