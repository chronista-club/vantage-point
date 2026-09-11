//! lane の名前（値型、棚卸し 項目 9-1b で `state.rs` から分離）。
//!
//! `LaneAddress`（`<repo>/lane/<name>` の宛先）。disk / engine / lock を触らない。id 側（`LaneId`）は
//! identity の SSOT がある [`crate::lane::lane_id`]（9-1d で移設 — `crate::lane` → `repo::lane` の
//! code 依存を切るため）。Display 形の逆変換 [`parse_address`] もここ（9-1c で `LanePool` から移設 —
//! `&self` を取らない純パーサだったので runtime 型の名前空間に住む理由が無かった）。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 開発起点 lane の予約名（doc 44 D4）。
///
/// 旧 `LaneKind::Main` の後継だが、**役割ではなく名前**である点が違う。
/// lane 側に「自分は main だ」という状態はなく、この名前を持つ lane が
/// たまたま開発起点である、という関係に退化した（P3 で Host のポインタに移る）。
///
/// この名前は `LaneAddress` の Display 形が旧 main と一致する（`<repo>/root`）
/// ように選んである — 既存の永続 address / wire を無傷で引き継ぐため。
///
/// **定義は `vp-paths` が唯一**（2026-07-21）。vp-app が同名定数を独自に持っていて
/// 「同値でなければ address が食い違う」をコメントの約束で担保していたため、
/// 定義ごと共有 crate へ畳んだ。ここは re-export。
pub use vp_paths::ROOT_LANE_NAME;

/// Lane の address — Pool key
///
/// 表示形 (`Display` 実装): `"<repo>/<name>"`  例: `"vp/root"` / `"vp/foo"`
///
/// doc 44 P2（フラット化）: 旧 `{ repo, kind, name: Option<String> }` の 3-tuple から
/// **`{ repo, name }` の 2-tuple** になった。旧構造は main だけ `name: None` という
/// 非対称を抱えており、それが「lane が役割を自意識する」構造の物理形だった（D4）。
///
/// ⚠️ sub の表示形が `<repo>/sub/<name>` → `<repo>/<name>` に変わる。
/// DB / session.json に残る旧形は [`parse_address`] が受理して新形に正規化する
/// （lead/wing → root/sub の rename 時と同じ手当て）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct LaneAddress {
    pub repo: String,
    /// lane 名（人間可読、例: "foo"）。開発起点は [`ROOT_LANE_NAME`]。
    ///
    /// `default` は P2 以前に永続した descriptor を読むための互換。旧 `LaneAddress` は
    /// main だけ `name` を持たず（`skip_serializing_if` で省略）、DB の `lane.descriptor`
    /// にその形で入っている。既定値を予約名にすると、旧 main レコードは name 欠落 →
    /// `"root"`、旧 sub は `name: "foo"` がそのまま読める（余分な `kind` は
    /// unknown field として無視される）ので、**custom Deserialize なしで旧形が全部読める**。
    #[serde(default = "default_lane_name")]
    pub name: String,
}

/// [`LaneAddress::name`] の serde 既定値（P2 以前の永続 descriptor 互換、上記参照）。
fn default_lane_name() -> String {
    ROOT_LANE_NAME.to_string()
}

impl LaneAddress {
    /// 任意の lane を構築する（フラット化後の canonical な構築子）。
    pub fn new(repo: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            name: name.into(),
        }
    }

    /// 開発起点 lane（予約名 [`ROOT_LANE_NAME`]）を構築する。
    pub fn root(repo: impl Into<String>) -> Self {
        Self::new(repo, ROOT_LANE_NAME)
    }

    /// 名前付き lane を構築する（旧 sub）。
    ///
    /// 旧 API 名を残しているのは呼び出し 100 箇所超の互換のため。フラット化後は
    /// [`Self::new`] と完全に同義で、「sub という種別」はもう存在しない。
    pub fn sub(repo: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(repo, name)
    }

    /// 開発起点 lane か（= 予約名を持つか）。
    pub fn is_root(&self) -> bool {
        self.name == ROOT_LANE_NAME
    }

    /// **wire address の SSOT**（`agent@<repo>` / `agent@<repo>/<name>`）。
    ///
    /// root は lane 部を省く（`mcp/lane.rs` の `mailbox_addresses` と同一の形）。
    /// ⚠️ この写像はもともと呼び手ごとの手書き `format!` に散っていた（8 箇所）。
    /// lane 削除時の wire 離脱（`WiremsgStore::leave_all_threads`）は**ここがずれると
    /// 別人を離脱させる**ので、新しい読み手はこのメソッドを通すこと。
    pub fn wire_agent_address(&self) -> String {
        if self.is_root() {
            format!("agent@{}", self.repo)
        } else {
            format!("agent@{}/{}", self.repo, self.name)
        }
    }

    /// **address 文字列の SSOT**（`<repo>/lane/<name>`）。
    ///
    /// ## ⚠️ なぜ `Display` ではなくこの名前なのか
    ///
    /// `Display` は**人間に見せる**ための trait で、永続化・wire の鍵に使うと 1 つの impl が
    /// 2 仕事を持つ（log を読みやすくしただけで**永続形が黙って変わる**）。形式は契約なので
    /// 명示的な名前で持つ。`Display` はここへ委譲するだけ。
    ///
    /// ⚠️ **client は自分で組み立てない**。この値は daemon が発行して `LaneAddressWire::key`
    /// に載せ、vp-app / webview はそのまま運ぶ。以前は Rust 2 実装 + TS 2 実装が同じ写像を
    /// 持ち、doc に「手で一致させる」と書く運用だった（`vp-app/src/lane_address.rs` の
    /// `key_matches_display` は**実際に食い違った**記録）。
    ///
    /// ⚠️ 分節は 3 つ。読み側 [`parse_address`] は旧形（`<repo>/<name>` /
    /// `<repo>/sub/<name>` / `<repo>/wing/<name>` / `<repo>/lead`）も受理して救済する。
    pub fn canonical(&self) -> String {
        format!("{}/{}/{}", self.repo, LANE_SEGMENT, self.name)
    }

    // `tmux_session_name` / `tmux_session_prefix` (Phase 1a の deterministic tmux 名導出) は
    // tmux decoupling PR2 で退役。 lane の identity は [`Self::canonical`] ただ一つ
    // (design doc §13.2 — sanitize 形は tmux の「`/` 禁止」制約由来だった)。
}

/// ⚠️ **`key` を必ず添えて送る**（daemon が address を発行する側）。
///
/// derive をやめて手で書いているのは、`{repo, name}` に加えて [`Self::canonical`] の
/// 結果を wire に載せるため。これで client（vp-app / webview）は**組み立てを持たない** —
/// 以前は Rust と TS が同じ写像を各々実装し、doc に「byte-for-byte 一致させる」と書く
/// 運用だった。
///
/// ⚠️ 読み側（`Deserialize`）は `key` を**見ない**。unknown field として無視され、
/// domain 型は `{repo, name}` から再構築される = 鍵の二重管理にならない。
impl Serialize for LaneAddress {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = ser.serialize_struct("LaneAddress", 3)?;
        st.serialize_field("repo", &self.repo)?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("key", &self.canonical())?;
        st.end()
    }
}

/// address の名前空間分節。`<repo>/lane/<name>` の `lane`。
///
/// ⚠️ 分節を明示するのは、将来 `<repo>/board/…` のような別種を足したときに衝突させないため。
pub const LANE_SEGMENT: &str = "lane";

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // ⚠️ 形式の定義は [`LaneAddress::canonical`] 一箇所。ここは委譲するだけ。
        f.write_str(&self.canonical())
    }
}

/// Display 形 (`"<repo>/root"` / `"<repo>/sub/<name>"`) をパースして LaneAddress を作る。
/// vp-app の sidebar から `lane:select` IPC の address (= `lane_address_key`) を逆変換するために使う。
pub fn parse_address(s: &str) -> Option<LaneAddress> {
    // 旧世代の**予約名**を現行の予約名へ写す（形の救済と直交する、名前の救済）。
    //
    // 予約名は `conductor` → `root` → `main` と 2 度改名されており、DB / session.json の
    // address 文字列に旧名のまま残る。ここで寄せておくと、起動時の
    // `normalize_legacy_lane_addresses`（parse → to_string の差分検知）が**既存行を
    // 自動で新名へ書き換える** = 名前の migration を別途書かなくてよい。
    //
    // ⚠️ 旧予約名の Sub は存在しえない（`validate_sub_name` が当時から予約名を拒否）
    // ので、この写しが実在の Sub を誤って Main に化けさせることはない。
    fn name_or_root(repo: &str, name: &str) -> LaneAddress {
        if vp_paths::LEGACY_ROOT_LANE_NAMES.contains(&name) {
            LaneAddress::root(repo)
        } else {
            LaneAddress::new(repo, name)
        }
    }
    let parts: Vec<&str> = s.splitn(3, '/').collect();
    match parts.as_slice() {
        // 旧 "lead" は開発起点の旧名 (main rename 前の session.json / wire address 互換)。
        [repo, "lead"] if !repo.is_empty() => Some(LaneAddress::root(*repo)),
        // 旧 2 分節形 "<repo>/<name>" (doc 44 P2 のフラット化形)。canonical が
        // `<repo>/lane/<name>` になった後も、永続 state / wire に残る旧形として受理する。
        [repo, name] if !repo.is_empty() && !name.is_empty() => Some(name_or_root(repo, name)),
        // 旧 3 分節形 "<repo>/sub/<name>" (P2 以前の永続 address / wire) を
        // 新形に正規化して受理する。lead/wing → root/sub の rename 時と同じ手当て
        // で、DB (`lane` / `lane/lifecycle` の address 列) と session.json を無傷で引き継ぐ。
        // canonical: "<repo>/lane/<name>"。名前空間を明示した現行形。
        [repo, LANE_SEGMENT, name] if !repo.is_empty() && !name.is_empty() => {
            Some(name_or_root(repo, name))
        }
        [repo, "sub" | "wing", name] if !repo.is_empty() && !name.is_empty() => {
            Some(name_or_root(repo, name))
        }
        _ => None,
    }
}

// `LaneComponent` enum は doc 11 (PR-B) で削除。 agent 識別子は `String` に統一
// (例: "claude" / "shell")。 tmux decoupling PR2 で agent script 層 (mise task) も廃止され、
// agent は `agent_spawner::build_agent_command` の Rust-native 分岐になった。
//
// `TmuxMode` / `TmuxLaneAddress` (Phase 1a の tmux session registry) は tmux decoupling PR2 で
// 退役 — lane の identity は `LaneAddress` ただ一つ、 process host は PtySlot ただ一つ
// (design doc §13)。

#[cfg(test)]
mod tests {
    use super::*;

    /// wire address の写像を固定する（`mailbox_addresses` と同一の形）。
    ///
    /// ⚠️ **ずれると lane 削除時に別人を wire から離脱させる**（`leave_all_threads` の
    /// 引数がこれ）。root だけ lane 部を省く非対称があるので literal で固定する。
    #[test]
    fn wire_agent_address_matches_mailbox_form() {
        assert_eq!(
            LaneAddress::root("vantage-point").wire_agent_address(),
            "agent@vantage-point",
            "root は lane 部を持たない"
        );
        assert_eq!(
            LaneAddress::sub("vantage-point".to_string(), "research".to_string())
                .wire_agent_address(),
            "agent@vantage-point/research"
        );
    }

    #[test]
    fn lane_address_canonical_has_lane_segment() {
        // address の形式は `<repo>/lane/<name>`。⚠️ 定義は `canonical()` の 1 箇所で、
        // Display はそこへ委譲するだけ（人間向け trait を永続形の SSOT にしない）。
        assert_eq!(LaneAddress::root("vp").canonical(), "vp/lane/main");
        assert_eq!(LaneAddress::sub("vp", "foo").canonical(), "vp/lane/foo");
        // Display が委譲しているか（片方だけ変わると永続と表示がずれる）。
        assert_eq!(LaneAddress::root("vp").to_string(), "vp/lane/main");
    }

    /// ⚠️ **wire に `key` が載る**（daemon が発行する側）。載らないと client が
    /// `{repo}/{name}` へ縮退し、無音で旧形に戻る。
    #[test]
    fn wire_carries_daemon_issued_key() {
        let json = serde_json::to_value(LaneAddress::sub("vp", "foo")).expect("serialize");
        assert_eq!(json["key"], "vp/lane/foo", "key が載っていない: {json}");
        assert_eq!(json["repo"], "vp", "描画用の部品も残す");
        assert_eq!(json["name"], "foo");
    }

    /// ⚠️ **読み側は `key` を見ない**。見ると鍵が二重管理になり、送られた key と
    /// `{repo,name}` から再構築した値が食い違う状態を表現できてしまう。
    #[test]
    fn deserialize_ignores_key_and_rebuilds_from_parts() {
        // 意図的に矛盾した key を混ぜる。無視されて {repo,name} が勝つのが正。
        let v = serde_json::json!({ "repo": "vp", "name": "foo", "key": "うそ/lane/うそ" });
        let addr: LaneAddress = serde_json::from_value(v).expect("deserialize");
        assert_eq!(addr, LaneAddress::sub("vp", "foo"));
        assert_eq!(addr.canonical(), "vp/lane/foo");
    }

    /// doc 44 P2: 旧 `LaneKind` の serde テスト 2 本（snake_case / "worker" 拒否）は型ごと撤去。
    /// 代わりに固定すべきは「**P2 以前に永続した descriptor が読めること**」になった。
    #[test]
    fn legacy_lane_address_deserializes() {
        // 旧 main: name 省略 + kind field あり → 予約名に落ちる
        let main: LaneAddress = serde_json::from_str(r#"{"repo":"vp","kind":"main"}"#).unwrap();
        assert_eq!(main, LaneAddress::root("vp"));
        assert!(main.is_root());

        // 旧 sub: name あり + kind field は unknown として無視される
        let sub: LaneAddress =
            serde_json::from_str(r#"{"repo":"vp","kind":"sub","name":"foo"}"#).unwrap();
        assert_eq!(sub, LaneAddress::new("vp", "foo"));
        assert!(!sub.is_root());

        // 新形（kind なし）
        let flat: LaneAddress = serde_json::from_str(r#"{"repo":"vp","name":"bar"}"#).unwrap();
        assert_eq!(flat, LaneAddress::new("vp", "bar"));
    }
}
