//! Lane に host される Stand の **minimal marker trait** + Registry (PR-δ-1、 VP-135)
//!
//! LSCM (`docs/design/12-stand-architecture.md` §13) Stand 自己診断 design + doc 13
//! (`docs/design/13-paisley-park-revival.md` §9) PR-δ boundary invariant
//! 「**LaneCapabilities が PP 含めて N Stand を host できる generic interface**」 の foundation。
//!
//! ## 設計分岐 (session ヒアリング 2026-05-06)
//!
//! - **B 路線** (speculative generic): trait 抽出 + mock Stand B で「N Stand host」
//!   invariant を test 化
//! - **i 路線** (minimal marker first): passive marker trait のみ定義、 actor lifecycle
//!   (spawn / shutdown / diagnose) は PR-γ (GE Lane 移管) で必要が出た段階で trait 拡張
//!
//! ## 本 PR (PR-δ-1) の scope
//!
//! - trait 定義 + Registry struct **型のみ** 落とす
//! - 既存挙動への影響 **ゼロ** (`LaneCapabilities` への統合は PR-δ-2)
//! - PP impl 追加は PR-δ-2、 mock Stand B + invariant test は PR-δ-3
//!
//! ## PR-δ 全体 roadmap (doc 13 §9)
//!
//! | sub-PR | scope | status |
//! |--------|------|--------|
//! | PR-δ-1 | LaneStandHost trait + LaneStandRegistry 受け皿 | ✅ Done (#288、 VP-135) |
//! | PR-δ-2 | PP を LaneStandHost impl に rewire + LaneCapabilities 統合 | ✅ Done (#289、 VP-136) |
//! | PR-δ-3 | mock Stand B test + 「N Stand host」 invariant test | ✅ Done (#290、 VP-137) |
//! | PR-δ-4 | cleanup (catalog 更新、 命名規約 doc 明示化、 stale 表記 sweep) | ✅ Done (VP-138) |
//!
//! ## VP-159 PR-1 で trait 名 rename (= `LaneStand` → `LaneStandHost`)
//!
//! VP-159 (= H1 Stand/Service framework trait 化) で **ECS entity bound actor** 用の `Stand`
//! trait (`capability::stand_service::Stand`) を新設するに当たり、 既存「Lane に host される
//! 受動的 marker」 と namespace 衝突するため、 本 trait を `LaneStandHost` に rename した
//! (struct `LaneStandRegistry` は「Lane Stand の registry」 のまま温存、 中身が
//! `Arc<dyn LaneStandHost>` の collection と読める)。
//!
//! 2 概念の区別:
//! - **`LaneStandHost`** (本 trait): Lane に hosted、 LaneStandRegistry で N 個 host する passive marker
//! - **`Stand`** (`capability::stand_service::Stand`): ECS entity bound、 自律的 active actor
//!
//! ## 関連
//!
//! - parent: VP-116 (doc 13 起草)
//! - doc 13 §9 PR-δ technical design
//! - PR-α-1 (#265、 VP-111) — 受け皿 pattern 先例
//! - PR-β-1 (#274、 VP-119) — 受け皿 pattern 直近先例
//! - VP-159 PR-1 — 本 trait の rename + `Stand` / `Service` trait 受け皿 (上記 namespace 区別)
//! - 旧 `LaneStand` enum は doc 11 (PR-B) で削除済 (`lanes_state.rs:55` 参照)、
//!   本 trait 名は元 `LaneStand` だったが VP-159 で `LaneStandHost` に rename

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

/// Lane に host される Stand の **minimal marker trait** (PR-δ-1、 VP-159 で rename)。
///
/// `Any` super-trait で **downcast をサポート** (caller が specific Stand state を
/// 取り出すため)。 PR-δ-1 は **passive marker** のみで、 actor lifecycle メソッドは
/// 追加しない (i 路線、 PR-γ で必要が出た段階で trait 拡張)。
///
/// VP-159 (= H1 Stand/Service framework) で `Stand` trait
/// (`capability::stand_service::Stand`) との namespace 衝突を避けるため、 元
/// `LaneStand` から `LaneStandHost` に rename した。
///
/// ## impl 例 (PR-δ-2 で導入済)
///
/// ```rust,ignore
/// pub struct PaisleyParkStand {
///     state: tokio::sync::RwLock<PaisleyParkState>,
/// }
///
/// impl LaneStandHost for PaisleyParkStand {
///     fn stand_kind(&self) -> &'static str { "paisley_park" }
///     fn as_any(&self) -> &dyn Any { self }
/// }
/// ```
pub trait LaneStandHost: Any + Send + Sync + 'static {
    /// stand_kind ID (例: `"paisley_park"` / `"gold_experience"` / `"mock_b"`)。
    ///
    /// `stands.rs` の `StandAlias.id` とは **別 namespace**: `StandAlias.id` は外部 API
    /// パス (例: `PAISLEY_PARK.id = "board"`、doc 52 §6 で canvas→board 改名) 用で、 こちらは
    /// Registry の HashMap key として使われる **内部 Stand ID**。 snake_case の安定キーという規約は
    /// 共通だが、 値は乖離する (例: PP は `StandAlias.id = "board"` vs `stand_kind() = "paisley_park"`)。
    /// PR-δ-4 (VP-138) で 2 namespace 意図的分離を明示化。
    fn stand_kind(&self) -> &'static str;

    /// `&dyn Any` への型強制 (downcast 用)。
    ///
    /// `LaneStandRegistry::get_typed::<T>(kind)` で specific Stand 型に downcast する際、
    /// `Arc<dyn LaneStandHost>::as_any()` 経由で `&dyn Any` を取得し `downcast_ref::<T>()` する。
    /// trait 自体に `Any` 制約があるが、 `&dyn LaneStandHost` から `&dyn Any` への自動 cast は
    /// Rust の trait object 制約により不可、 explicit な変換 method が必要 (`Any` 慣用句)。
    // 要確認（audit 2026-07-18、先行実装の可能性）: get_typed downcast 経路が未活性。
    #[allow(dead_code)]
    fn as_any(&self) -> &dyn Any;
}

/// Lane-hosted Stand の registry (PR-δ-1 受け皿)。
///
/// `HashMap<&'static str, Arc<dyn LaneStandHost>>` で **N 個 host** する。 key は
/// `LaneStandHost::stand_kind()` の戻り値 (= stand id)。 Arc-shared なので caller が typed
/// reference を保持しつつ Registry にも置ける (= state mutation は impl 内 RwLock 等の
/// interior mutability で行う設計、 PR-δ-2 PP impl で具体化)。
///
/// PR-δ-2 で `LaneCapabilities` が field として保持、 `populate_lane` で Lane 起動時に
/// PP を insert する flow に置き換わる。
#[derive(Default)]
pub struct LaneStandRegistry {
    stands: HashMap<&'static str, Arc<dyn LaneStandHost>>,
}

// 要確認（audit 2026-07-18、先行実装の可能性）: PR-δ/PR-ε Stand-host registry の skeleton API
// （count/get/get_typed/remove は未活性、new/insert は LaneCapabilities::new で活性）。
#[allow(dead_code)]
impl LaneStandRegistry {
    /// 空の Registry を構築。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登録された Stand 数。
    pub fn count(&self) -> usize {
        self.stands.len()
    }

    /// Stand を登録 (idempotent: 同じ `stand_kind` で 2 回 insert すると **後勝ち**)。
    ///
    /// `Arc<T>` を受け取り、 unsized coercion で `Arc<dyn LaneStandHost>` に変換して
    /// HashMap に格納。 caller は `Arc<T>` を呼び出し側に保持しても良い (= shared
    /// reference として複数 path で活用可能、 PR-ε で creo memory feed inject 等)。
    ///
    /// ## type bound 注意
    ///
    /// `T: LaneStandHost + ?Sized` ではなく `T: LaneStandHost` (sized) のみを許容。
    /// Sized 制約は `Arc<T> -> Arc<dyn LaneStandHost>` の unsized coercion を成立させるため必須
    /// (Rust の `CoerceUnsized<Arc<U>> for Arc<T>` は `T: Unsize<U>` 要求、 これは Sized T で満たされる)。
    pub fn insert<T: LaneStandHost>(&mut self, stand: Arc<T>) {
        let kind = stand.stand_kind();
        let dyn_stand: Arc<dyn LaneStandHost> = stand;
        self.stands.insert(kind, dyn_stand);
    }

    /// `kind` ID で Arc reference を取得。
    ///
    /// `&Arc<...>` を返すため、 caller は `.clone()` して shared ownership を得るか、
    /// `&*arc` で `&dyn LaneStandHost` deref する。
    pub fn get(&self, kind: &str) -> Option<&Arc<dyn LaneStandHost>> {
        self.stands.get(kind)
    }

    /// `kind` ID で specific Stand 型に downcast 取得。
    ///
    /// `Arc<dyn LaneStandHost>` → `&dyn Any` → `&T` の 2 段 cast。 失敗時 (`kind` 不在 / 型不一致)
    /// は `None`。 PR-δ-2 で PP caller (例: `routes/canvas.rs`) が
    /// `registry.get_typed::<PaisleyParkStand>("paisley_park")` で typed access する。
    pub fn get_typed<T: LaneStandHost>(&self, kind: &str) -> Option<&T> {
        self.stands.get(kind)?.as_any().downcast_ref::<T>()
    }

    /// `kind` ID の Stand を削除。 削除成功で `Some(Arc<dyn LaneStandHost>)`、 不在で `None`。
    pub fn remove(&mut self, kind: &str) -> Option<Arc<dyn LaneStandHost>> {
        self.stands.remove(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test fixture A — `stand_kind = "fixture_a"`、 数値 state を持つ minimal Stand impl
    struct FixtureA {
        value: u32,
    }

    impl LaneStandHost for FixtureA {
        fn stand_kind(&self) -> &'static str {
            "fixture_a"
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// test fixture B — `stand_kind = "fixture_b"`、 文字列 state を持つ別 Stand impl
    /// (異なる kind の 2 fixture 共存検証用、 PR-δ-3 mock Stand B の雛形でもある)
    struct FixtureB {
        label: String,
    }

    impl LaneStandHost for FixtureB {
        fn stand_kind(&self) -> &'static str {
            "fixture_b"
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn registry_new_is_empty() {
        let reg = LaneStandRegistry::new();
        assert_eq!(reg.count(), 0);
        assert!(reg.get("fixture_a").is_none());
    }

    #[test]
    fn registry_insert_arc_coercion_works() {
        // Arc<T> から Arc<dyn LaneStandHost> への unsized coercion が成立すること
        let mut reg = LaneStandRegistry::new();
        let fix = Arc::new(FixtureA { value: 42 });
        reg.insert(fix);

        assert_eq!(reg.count(), 1);
        assert!(reg.get("fixture_a").is_some());
    }

    #[test]
    fn registry_get_returns_arc_dyn() {
        // get(kind) は Arc<dyn LaneStandHost> reference を返す、 stand_kind() で id 確認
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 7 }));

        let stand = reg.get("fixture_a").expect("fixture_a 不在");
        assert_eq!(stand.stand_kind(), "fixture_a");
    }

    #[test]
    fn registry_get_typed_succeeds_for_correct_type() {
        // get_typed::<T>(kind) で正しい型に downcast できる
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 99 }));

        let typed = reg
            .get_typed::<FixtureA>("fixture_a")
            .expect("FixtureA downcast 失敗");
        assert_eq!(typed.value, 99);
    }

    #[test]
    fn registry_get_typed_fails_for_wrong_type() {
        // 正しい kind だが型が違う場合は None (downcast 失敗)
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 1 }));

        let wrong = reg.get_typed::<FixtureB>("fixture_a");
        assert!(
            wrong.is_none(),
            "kind は fixture_a だが型 FixtureB で downcast 試行 → None 期待"
        );
    }

    #[test]
    fn registry_get_typed_returns_none_for_missing_kind() {
        // 存在しない kind は型に関わらず None
        let reg = LaneStandRegistry::new();
        assert!(reg.get_typed::<FixtureA>("nonexistent").is_none());
    }

    #[test]
    fn registry_insert_same_kind_overwrites() {
        // 同じ stand_kind で 2 回 insert → 後勝ち (idempotent: respawn / restart 経路で安全)
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 1 }));
        reg.insert(Arc::new(FixtureA { value: 2 }));

        assert_eq!(
            reg.count(),
            1,
            "同 kind の 2 回 insert で count は 1 のまま"
        );
        let typed = reg.get_typed::<FixtureA>("fixture_a").expect("fixture_a");
        assert_eq!(typed.value, 2, "後勝ちで value = 2");
    }

    #[test]
    fn registry_remove_returns_some_then_none() {
        // remove は 1 度目 Some、 2 度目 None
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 0 }));

        let removed = reg.remove("fixture_a");
        assert!(removed.is_some(), "1 度目の remove で Some");
        assert_eq!(reg.count(), 0);

        let second = reg.remove("fixture_a");
        assert!(second.is_none(), "2 度目の remove で None");
    }

    #[test]
    fn registry_hosts_n_distinct_stands() {
        // 異なる kind の 2 fixture が同じ Registry に共存 (= 「N Stand host」 invariant の雛形)。
        // PR-δ-3 で mock Stand B + 真の invariant test に拡張予定。
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 10 }));
        reg.insert(Arc::new(FixtureB {
            label: "hello".to_string(),
        }));

        assert_eq!(reg.count(), 2, "異なる kind の 2 Stand 共存");

        let a = reg.get_typed::<FixtureA>("fixture_a").expect("FixtureA");
        let b = reg.get_typed::<FixtureB>("fixture_b").expect("FixtureB");
        assert_eq!(a.value, 10);
        assert_eq!(b.label, "hello");
    }

    #[test]
    fn registry_remove_does_not_affect_other_stands() {
        // 1 つ remove しても他 Stand は残る (HashMap 当たり前だが Lane 移管後の lifecycle で重要)
        let mut reg = LaneStandRegistry::new();
        reg.insert(Arc::new(FixtureA { value: 1 }));
        reg.insert(Arc::new(FixtureB {
            label: "b".to_string(),
        }));

        reg.remove("fixture_a");

        assert_eq!(reg.count(), 1);
        assert!(reg.get("fixture_a").is_none());
        assert!(reg.get("fixture_b").is_some(), "fixture_b は残存");
    }
}
