//! Lane — SP 内で起動する PTY セッションの抽象。
//!
//! 関連: `mem_1CaSpvE??` (VP Architecture: 3 段 Stand scope + Lane semantic)
//!
//! ## 構造 (memory rule)
//!
//! - **Conductor Lane** (Project あたり 1 つ固定) ─ 中身は `LaneStand` (HD default | TH)
//! - **Performer Lane** (Project あたり n 個) ─ lane cloned worktree、中身は `LaneStand`
//!
//! ## 表示形 (人間可読)
//!
//! - Conductor: `"vantage-point/conductor"`
//! - Performer: `"vantage-point/performer/foo"`

use std::fmt;

// v1.0 柱 2 PR-1: ts-rs で sidebar wire 型を TS に export (test build 時のみ)。
#[cfg(test)]
use ts_rs::TS;

/// Lane の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    /// Project あたり 1 つ固定
    Conductor,
    /// Project あたり n 個 (lane cloned worktree)。
    Performer,
}

impl fmt::Display for LaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneKind::Conductor => write!(f, "conductor"),
            LaneKind::Performer => write!(f, "performer"),
        }
    }
}

/// Lane の address — Pool key として使う 3-tuple
///
/// 表示形 (`Display` 実装):
/// - Conductor: `"<project>/conductor"`         例: `"vantage-point/conductor"`
/// - Performer: `"<project>/performer/<name>"`  例: `"vantage-point/performer/foo"`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaneAddress {
    pub project: String,
    pub kind: LaneKind,
    /// Performer の場合のみ Some (人間可読)。Conductor は None。
    pub name: Option<String>,
}

impl LaneAddress {
    /// Conductor Lane を構築 (Project あたり 1 つ固定なので name 不要)
    pub fn conductor(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Conductor,
            name: None,
        }
    }

    /// Performer Lane を構築 (人間可読 name 必須)
    pub fn performer(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Performer,
            name: Some(name.into()),
        }
    }

    /// Conductor 判定
    pub fn is_conductor(&self) -> bool {
        matches!(self.kind, LaneKind::Conductor)
    }
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (LaneKind::Conductor, _) => write!(f, "{}/conductor", self.project),
            (LaneKind::Performer, Some(n)) => write!(f, "{}/performer/{}", self.project, n),
            (LaneKind::Performer, None) => write!(f, "{}/performer/<unnamed>", self.project),
        }
    }
}

/// Lane address の **wire (JSON) 表現** — SP `/api/lanes` レスポンス要素の field
///
/// `LaneAddress` (domain enum-based) と区別する役割:
/// - **`LaneAddressWire`** = JSON 入口、 `kind` を String のまま保持 (vantage-point 側の
///   "conductor"/"performer"/将来の任意値 をそのまま deserialize 受け)
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
    /// "conductor" | "performer"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl LaneAddressWire {
    /// Display 形 (`<project>/conductor` / `<project>/performer/<name>`) を文字列で返す。
    ///
    /// 旧 `app.rs::lane_address_key` を吸収。 JS 側 `laneAddressKey()` と完全に一致させる
    /// (active 比較に使うため、 byte-for-byte 同一が要件)。
    ///
    /// `LaneAddress::Display` と微妙に挙動が違うことに注意:
    /// - `Wire::key()` は kind string をそのまま保持 (unknown kind "magic" → `"project/magic"`)
    /// - `LaneAddress::Display` は LaneKind enum 経由 (unknown kind は From で Conductor に collapse)
    ///
    /// JSON wire 互換 (旧 `lane_address_key` と byte-for-byte 同一) を要する場面では本 method、
    /// 型安全な domain 表現が要る場面では `LaneAddress::from(&wire).to_string()` を使う。
    pub fn key(&self) -> String {
        match (self.kind.as_str(), self.name.as_deref()) {
            ("performer", Some(n)) => format!("{}/performer/{}", self.project, n),
            ("performer", None) => format!("{}/performer/<unnamed>", self.project),
            _ => format!("{}/{}", self.project, self.kind),
        }
    }
}

/// Wire (JSON) 形から domain 形 (enum-based) への変換。
///
/// 注意: unknown kind ("magic" 等) は fallback で `LaneKind::Conductor` に collapse される
/// (= 情報損失)。 raw kind 文字列を保ちたい用途では `LaneAddressWire::key()` を直接使う。
impl From<&LaneAddressWire> for LaneAddress {
    fn from(wire: &LaneAddressWire) -> Self {
        let kind = match wire.kind.as_str() {
            // 旧 "wing" も Performer に受理 (conductor/performer rename 前の wire / session 互換)
            "performer" | "wing" => LaneKind::Performer,
            // "conductor" / 旧 "lead" / unknown → Conductor に collapse
            _ => LaneKind::Conductor,
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
        let conductor = LaneAddress::conductor("vantage-point");
        assert_eq!(conductor.to_string(), "vantage-point/conductor");
        assert!(conductor.is_conductor());

        let performer = LaneAddress::performer("vantage-point", "foo");
        assert_eq!(performer.to_string(), "vantage-point/performer/foo");
        assert!(!performer.is_conductor());
    }

    #[test]
    fn lane_address_eq_hash() {
        // 同じ project/kind/name なら同一視 (HashMap key として使えること)
        let a = LaneAddress::performer("vp", "foo");
        let b = LaneAddress::performer("vp", "foo");
        assert_eq!(a, b);

        let c = LaneAddress::performer("vp", "bar");
        assert_ne!(a, c);
    }

    /// R-0 wire compat: `LaneAddressWire::key()` は旧 `app.rs::lane_address_key` と
    /// byte-for-byte 同一でなければならない (JS 側 `laneAddressKey()` の active 比較で使うため)。
    #[test]
    fn lane_address_wire_key_conductor() {
        let w = LaneAddressWire {
            project: "vantage-point".into(),
            kind: "conductor".into(),
            name: None,
        };
        assert_eq!(w.key(), "vantage-point/conductor");
    }

    #[test]
    fn lane_address_wire_key_performer_named() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "performer".into(),
            name: Some("foo".into()),
        };
        assert_eq!(w.key(), "vp/performer/foo");
    }

    #[test]
    fn lane_address_wire_key_performer_unnamed() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "performer".into(),
            name: None,
        };
        assert_eq!(w.key(), "vp/performer/<unnamed>");
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
    fn lane_address_from_wire_conductor() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "conductor".into(),
            name: None,
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.kind, LaneKind::Conductor);
        assert_eq!(addr.project, "vp");
        assert!(addr.name.is_none());
    }

    #[test]
    fn lane_address_from_wire_performer() {
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "performer".into(),
            name: Some("foo".into()),
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.kind, LaneKind::Performer);
        assert_eq!(addr.name.as_deref(), Some("foo"));
    }

    #[test]
    fn lane_address_from_wire_unknown_collapses_to_conductor() {
        // 情報損失の意図的契約: `LaneAddress` enum で表現できない kind は Conductor に折りたたむ。
        // raw kind 保持が必要なら `Wire::key()` 直接使用。
        let w = LaneAddressWire {
            project: "vp".into(),
            kind: "magic".into(),
            name: None,
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.kind, LaneKind::Conductor);
    }
}
