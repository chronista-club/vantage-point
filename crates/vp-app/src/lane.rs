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

// doc 44 P2: `LaneKind`（Conductor / Performer）は撤去。server 側 `LaneKind` と対の型で、
// 同じ理由で消える（D4「lane 自身は役割状態を持たない」）。

/// 開発起点 lane の予約名（doc 44 D4）。server 側 `CONDUCTOR_LANE_NAME` と同値でなければ
/// address が食い違う（wire の byte-for-byte 一致が要件）。
pub const CONDUCTOR_LANE_NAME: &str = "conductor";

/// Lane の address — Pool key として使う 2-tuple
///
/// 表示形 (`Display` 実装): `"<project>/<name>"`  例: `"vantage-point/conductor"` / `"vantage-point/foo"`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaneAddress {
    pub project: String,
    /// lane 名（人間可読）。開発起点は [`CONDUCTOR_LANE_NAME`]。
    pub name: String,
}

impl LaneAddress {
    /// 任意の lane を構築する（フラット化後の canonical な構築子）。
    pub fn new(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            name: name.into(),
        }
    }

    /// 開発起点 Lane を構築（予約名）
    pub fn conductor(project: impl Into<String>) -> Self {
        Self::new(project, CONDUCTOR_LANE_NAME)
    }

    /// 名前付き Lane を構築（旧 performer）
    pub fn performer(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(project, name)
    }

    /// 開発起点 lane か（= 予約名を持つか）
    pub fn is_conductor(&self) -> bool {
        self.name == CONDUCTOR_LANE_NAME
    }
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.project, self.name)
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
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct LaneAddressWire {
    #[serde(default)]
    pub project: String,
    /// lane 名。開発起点は [`CONDUCTOR_LANE_NAME`]。
    ///
    /// doc 44 P2: `kind` を廃し `name` 必須に。`default` は P2 以前の payload / 永続 state
    /// 互換で、旧 conductor は `name` を持たないため予約名に落ちる（server 側 `LaneAddress`
    /// の serde default と同じ手当て）。
    #[serde(default = "default_lane_name")]
    pub name: String,
}

/// [`LaneAddressWire::name`] の serde 既定値（P2 以前の payload 互換）。
fn default_lane_name() -> String {
    CONDUCTOR_LANE_NAME.to_string()
}

impl LaneAddressWire {
    /// Display 形 (`<project>/<name>`) を文字列で返す。
    ///
    /// 旧 `app.rs::lane_address_key` を吸収。 JS 側 `laneAddressKey()` と完全に一致させる
    /// (active 比較に使うため、 byte-for-byte 同一が要件)。
    ///
    /// doc 44 P2 以前は kind に応じて 2 形（`<project>/conductor` と
    /// `<project>/performer/<name>`）を出し分けており、`LaneAddress::Display` との
    /// 微妙な差（unknown kind の扱い）も抱えていた。フラット化で両者は同一の 1 形に畳まれ、
    /// この method と `LaneAddress::from(&wire).to_string()` は常に同じ文字列を返す。
    pub fn key(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }
}

/// Wire (JSON) 形から domain 形への変換。
///
/// doc 44 P2: kind が消えたため、変換は純粋な field コピーになった（旧実装が抱えていた
/// 「unknown kind は Conductor に collapse = 情報損失」も同時に消滅）。
impl From<&LaneAddressWire> for LaneAddress {
    fn from(wire: &LaneAddressWire) -> Self {
        Self {
            project: wire.project.clone(),
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
        // doc 44 P2: フラット化後の表示形は `<project>/<name>`
        assert_eq!(performer.to_string(), "vantage-point/foo");
        assert!(!performer.is_conductor());
    }

    #[test]
    fn lane_address_eq_hash() {
        // 同じ project/name なら同一視 (HashMap key として使えること)
        let a = LaneAddress::performer("vp", "foo");
        let b = LaneAddress::performer("vp", "foo");
        assert_eq!(a, b);

        let c = LaneAddress::performer("vp", "bar");
        assert_ne!(a, c);
    }

    /// R-0 wire compat: `LaneAddressWire::key()` は JS 側 `laneAddressKey()` と
    /// byte-for-byte 同一でなければならない (active 比較で使うため)。
    #[test]
    fn lane_address_wire_key_is_flat() {
        let conductor = LaneAddressWire {
            project: "vantage-point".into(),
            name: "conductor".into(),
        };
        assert_eq!(conductor.key(), "vantage-point/conductor");

        let named = LaneAddressWire {
            project: "vp".into(),
            name: "foo".into(),
        };
        assert_eq!(named.key(), "vp/foo");
    }

    /// doc 44 P2: `key()` と `LaneAddress::from(&wire).to_string()` が一致すること。
    ///
    /// 旧実装では両者が微妙に食い違っていた（`key()` は kind 文字列を素通し、
    /// `Display` は unknown kind を Conductor に collapse）。フラット化でこの差は消えた。
    #[test]
    fn wire_key_and_domain_display_agree() {
        for name in ["conductor", "foo", "feat-x"] {
            let w = LaneAddressWire {
                project: "vp".into(),
                name: name.into(),
            };
            assert_eq!(w.key(), LaneAddress::from(&w).to_string());
        }
    }

    #[test]
    fn lane_address_from_wire() {
        let w = LaneAddressWire {
            project: "vp".into(),
            name: "foo".into(),
        };
        let addr = LaneAddress::from(&w);
        assert_eq!(addr.project, "vp");
        assert_eq!(addr.name, "foo");
        assert!(!addr.is_conductor());
    }

    /// P2 以前の payload（`name` を持たない conductor）が予約名に落ちること。
    #[test]
    fn legacy_wire_without_name_defaults_to_conductor() {
        let w: LaneAddressWire =
            serde_json::from_str(r#"{"project":"vp","kind":"conductor"}"#).unwrap();
        assert_eq!(w.name, CONDUCTOR_LANE_NAME);
        assert_eq!(w.key(), "vp/conductor");

        // 旧 performer payload は name をそのまま引き継ぐ（`kind` は unknown field として無視）
        let p: LaneAddressWire =
            serde_json::from_str(r#"{"project":"vp","kind":"performer","name":"foo"}"#).unwrap();
        assert_eq!(p.key(), "vp/foo");
    }
}
