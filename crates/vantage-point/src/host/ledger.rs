//! 帳簿 — Project Host が project について持つ現在値と履歴（doc 44 D3 / §8）。
//!
//! 第一の住人は **開発起点ポインタ**（D4）。「この project の開発の起点はどの lane か」を
//! Host が 1 本だけ持つ。
//!
//! ## なぜ lane 自身に持たせないのか
//!
//! P2（フラット化）で `LaneKind` を撤去し、lane は全て対等になった。
//! 「起点である」は lane の属性ではなく **project 側の指定** なので、帳簿が持つ。
//! こうすると起点の移動が「ポインタの書き換え 1 回」になり、lane は何も動かない（D5）。
//!
//! ## key が名前ではなく id な理由
//!
//! ポインタが指すのは lane **そのもの**であって「今その名前で呼ばれているもの」ではない。
//! 名前は表示のための自然キーで、将来 rename できるようにすると動く。だから帳簿は
//! [`crate::process::lanes_state::LaneId`]（UUID v7、doc 24 §7 の I1）を key にする。
//!
//! 名前 ↔ id の解決は **境界で 1 回だけ**行う: 人が打つのは名前、帳簿に入るのは id
//! （[`resolve_origin_name`] が読み側、unison `lane_origin_set` が書き側）。
//!
//! ## フォールバックの設計
//!
//! ポインタが無い / 指す lane が実在しない場合は予約名 `conductor` に落ちる。
//! これは「推測」ではなく D4 が定めた既定値（conductor は残る）なので、Host の
//! 「推測しない」原則には反しない。ただし **dangling は隠さず事実として返す**
//! （[`OriginSource::Dangling`]）— 黙って既定に戻ると、指定したはずの起点が
//! いつの間にか動いていたことに気付けない。

use crate::process::lanes_state::ROOT_LANE_NAME;

/// 起点の解決に要る lane の最小情報（id と表示名の対）。
///
/// `LaneInfo` 全体を渡さないのは、帳簿が lane の中身に依存しないため
/// （`host::farewell::LaneFacts` が git も DB も知らないのと同じ切り方）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneRef {
    pub id: String,
    pub name: String,
}

impl LaneRef {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

/// 起点がどう決まったか。表示にそのまま出せる粒度で持つ。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginSource {
    /// ポインタ未設定 — 予約名が起点（P2 までの挙動そのもの）
    Default,
    /// ポインタが実在 lane を指している
    Pinned,
    /// ポインタはあるが指す lane が実在しない（削除された等）— 予約名に戻る。
    /// 事実として残すのは、指定が失われたことを人に見せるため
    Dangling { lane_id: String },
}

/// 起点の解決結果。unison `lane_origin_get` の応答形でもある。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Origin {
    /// 起点 lane の表示名
    pub name: String,
    pub source: OriginSource,
}

impl Origin {
    /// 予約名に落ちた既定の起点。
    fn default_origin(source: OriginSource) -> Self {
        Self {
            name: ROOT_LANE_NAME.to_string(),
            source,
        }
    }
}

/// 帳簿のポインタと実在 lane から「今の起点」を決める **純関数**（Host の層 1）。
///
/// I/O ゼロなので全分岐をテストで固定できる。判定順序:
///
/// 1. ポインタが無い → 予約名（既定）
/// 2. ポインタが実在 lane を指す → その lane
/// 3. ポインタが指す lane が居ない → 予約名に戻すが、**dangling だったことは残す**
pub fn resolve_origin_name(pointer: Option<&str>, lanes: &[LaneRef]) -> Origin {
    let Some(id) = pointer.filter(|s| !s.is_empty()) else {
        return Origin::default_origin(OriginSource::Default);
    };
    match lanes.iter().find(|l| l.id == id) {
        Some(lane) => Origin {
            name: lane.name.clone(),
            source: OriginSource::Pinned,
        },
        None => Origin::default_origin(OriginSource::Dangling {
            lane_id: id.to_string(),
        }),
    }
}

/// 名前から lane の id を引く（書き側の境界変換、純関数）。
///
/// 人が打つのは名前なので、帳簿に入れる前にここで id にする。
/// 見つからなければ `None` — Host は存在しない lane を起点にしない。
pub fn lane_id_of<'a>(name: &str, lanes: &'a [LaneRef]) -> Option<&'a str> {
    lanes
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.id.as_str())
        .filter(|id| !id.is_empty())
}

/// 帳簿の並び順を lane 列に適用する **純関数**（doc 44 §12）。
///
/// `order` は `lane_id` → `ord`。**指定のある lane が先**（ord 昇順）、指定の無い lane は
/// その後ろに**元の並びのまま**続く。
///
/// 「未指定は末尾」にするのは、新しく作られた lane が既存の並びに割り込まないため。
/// 割り込むと「並べ替えたのに勝手に崩れた」に見える。
///
/// 元の並び（`LanePool::list()` の 開発起点が先頭 → created_at）を保つのは、
/// **帳簿が何も言っていない範囲では既定の意味論を壊さない**ため。
pub fn apply_lane_order<T>(
    lanes: &mut [T],
    order: &std::collections::HashMap<String, i64>,
    id_of: impl Fn(&T) -> String,
) {
    if order.is_empty() {
        return;
    }
    // 安定 sort なので、同じ rank（= どちらも未指定）は元の並びが保たれる。
    lanes.sort_by_key(|l| order.get(&id_of(l)).copied().unwrap_or(i64::MAX));
}

// =============================================================================
// actions — 帳簿の永続（DB）。判定は上の純関数が済ませている
// =============================================================================

/// 帳簿の row key を作る（**project path の正規化はここに畳む**）。
///
/// 呼び手によって渡ってくる path が違うため:
///
/// - `AppState.project_dir` — `ProjectRuntimes::start` が受け取った**生のパス**
/// - `path_key` — `normalize_path_key`（canonicalize 済、symlink 解決後）
///
/// `ProjectRuntimes` は map key に正規化を使いつつ `CapabilityConfig` には生を渡すので、
/// 両者は一致するとは限らない（macOS の `/tmp` → `/private/tmp` 等）。ズレると
/// **書き手と読み手が別の行を触り、起点を指定しても snapshot に載らない**。
///
/// 慣習では call site 側で正規化するが、帳簿は経路が 4 本（publish 2 / get / set）あり、
/// 1 つ忘れると無症状で行が割れる。**書き手が 1 module しか無い新設 table なので、
/// 正規化をここに閉じて構造的に一致させる。**
fn row_key(project_path: &str) -> String {
    crate::capability::normalize_path_key(std::path::Path::new(project_path))
}

/// 帳簿から起点を読む。
///
/// DB が無い（`vpdb: None` の test fixture 等）/ 読めない場合は既定に落ちる —
/// 起点が読めないだけで project が動かなくなる方が困るため（best-effort）。
pub async fn origin(
    vpdb: Option<&crate::db::SharedVpDb>,
    project_path: &str,
    lanes: &[LaneRef],
) -> Origin {
    let pointer = match vpdb {
        Some(db) => match db.get_host_origin(&row_key(project_path)).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("帳簿: 起点ポインタの読み出しに失敗（既定に落とす）: {}", e);
                None
            }
        },
        None => None,
    };
    resolve_origin_name(pointer.as_deref(), lanes)
}

/// `LaneInfo` の並びから起点を解決する（snapshot publisher 用の薄い adapter）。
///
/// `LanesSnapshot` を publish する経路が 2 本ある（project runtime の live push と、
/// World が vp-app 接続時に配る retained snapshot）ので、両方が同じ解決を通るように
/// ここに畳む。**片方だけ解決すると受け手が起点の有無で flicker する。**
pub async fn origin_name_for_lanes(
    vpdb: Option<&crate::db::SharedVpDb>,
    project_path: &str,
    lanes: &[crate::process::lanes_state::LaneInfo],
) -> String {
    let refs: Vec<LaneRef> = lanes
        .iter()
        .map(|l| LaneRef::new(l.id.to_string(), l.address.name.clone()))
        .collect();
    origin(vpdb, project_path, &refs).await.name
}

/// 帳簿の並び順を lane 列に適用する（snapshot publisher / `lanes_list` 用）。
///
/// DB が無い / 読めない場合は**並べ替えない**（既定順のまま）。並び順が読めないだけで
/// lane 一覧が出なくなる方が困る。
pub async fn sort_lanes_by_ledger(
    vpdb: Option<&crate::db::SharedVpDb>,
    project_path: &str,
    lanes: &mut [crate::process::lanes_state::LaneInfo],
) {
    let Some(db) = vpdb else { return };
    let order = match db.list_lane_order(&row_key(project_path)).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("帳簿: lane 並び順の読み出しに失敗（既定順で継続）: {}", e);
            return;
        }
    };
    apply_lane_order(lanes, &order, |l| l.id.to_string());
}

/// lane の並び順を設定する（名前列で受けて id 列で書く）。
///
/// 起点と同じく境界で 1 回だけ名前 → id に変換する。実在しない名前は**黙って落とす**
/// のではなく、解決できた分だけを書く — 一覧と帳簿の間には常に時間差があり
/// （送信中に lane が消える等）、1 つの不一致で並べ替え全体を失敗させる意味が無い。
/// ただし 1 つも解決できなければ Err（呼び手の指定が丸ごと間違っている）。
pub async fn set_lane_order(
    vpdb: Option<&crate::db::SharedVpDb>,
    project_path: &str,
    lane_names: &[String],
    lanes: &[LaneRef],
) -> Result<(), String> {
    let db = vpdb.ok_or_else(|| "帳簿: DB 未接続のため並び順を保存できません".to_string())?;
    let ids: Vec<String> = lane_names
        .iter()
        .filter_map(|n| lane_id_of(n, lanes).map(|id| id.to_string()))
        .collect();
    if ids.is_empty() {
        return Err("帳簿: 並び順に解決できる lane がありません".to_string());
    }
    db.replace_lane_order(&row_key(project_path), &ids)
        .await
        .map_err(|e| format!("帳簿: 並び順の永続に失敗: {e}"))
}

/// 起点を設定する（名前で受けて id で書く）。
///
/// 名前解決に失敗したら **書かない** — 存在しない lane を指すポインタを自ら作らない。
pub async fn set_origin(
    vpdb: Option<&crate::db::SharedVpDb>,
    project_path: &str,
    lane_name: &str,
    lanes: &[LaneRef],
) -> Result<(), String> {
    let db = vpdb.ok_or_else(|| "帳簿: DB 未接続のため起点を設定できません".to_string())?;
    let id = lane_id_of(lane_name, lanes).ok_or_else(|| {
        format!("帳簿: lane '{lane_name}' が見つからない（または安定 id を持たない）")
    })?;
    db.upsert_host_origin(&row_key(project_path), id)
        .await
        .map_err(|e| format!("帳簿: 起点の永続に失敗: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lanes() -> Vec<LaneRef> {
        vec![
            LaneRef::new("id-root", ROOT_LANE_NAME),
            LaneRef::new("id-foo", "foo"),
        ]
    }

    /// ポインタ未設定は予約名（= P2 までの挙動）。移行時に何も壊れないことの固定。
    #[test]
    fn no_pointer_falls_back_to_reserved_name() {
        let origin = resolve_origin_name(None, &lanes());
        assert_eq!(origin.name, ROOT_LANE_NAME);
        assert_eq!(origin.source, OriginSource::Default);
    }

    /// 空文字のポインタは「未設定」と同じ扱い（空 `LaneId` が混入した時の保険）。
    #[test]
    fn empty_pointer_is_treated_as_unset() {
        let origin = resolve_origin_name(Some(""), &lanes());
        assert_eq!(origin.source, OriginSource::Default);
    }

    /// ポインタが実在 lane を指せば、予約名でなくてもそれが起点になる。
    /// これが D4 の本体 — 起点は lane の役割ではなく project の指定。
    #[test]
    fn pointer_moves_origin_to_any_lane() {
        let origin = resolve_origin_name(Some("id-foo"), &lanes());
        assert_eq!(origin.name, "foo");
        assert_eq!(origin.source, OriginSource::Pinned);
    }

    /// 指す先が消えていたら予約名に戻すが、**dangling だった事実は返す**。
    ///
    /// 黙って既定に戻すと「起点を指定したはずなのにいつの間にか動いていた」に
    /// 気付けない。Host は人の判断材料を作る立場なので、事実は落とさない。
    #[test]
    fn dangling_pointer_falls_back_but_is_reported() {
        let origin = resolve_origin_name(Some("id-gone"), &lanes());
        assert_eq!(origin.name, ROOT_LANE_NAME);
        assert_eq!(
            origin.source,
            OriginSource::Dangling {
                lane_id: "id-gone".to_string()
            }
        );
    }

    /// 名前 → id は書き側の境界変換。人は名前を打ち、帳簿には id が入る。
    #[test]
    fn lane_id_of_resolves_name_to_surrogate_key() {
        assert_eq!(lane_id_of("foo", &lanes()), Some("id-foo"));
        assert_eq!(lane_id_of("missing", &lanes()), None);
    }

    /// 安定 id を持たない lane（空 `LaneId`）は起点にできない。
    ///
    /// `LaneInfo.id` は `#[serde(default)]` で空になりうる（I1 以前の wire payload）。
    /// 空 id を書くと `resolve_origin_name` 側で「未設定」と区別できなくなるため、
    /// 書く前に弾く。
    #[test]
    fn lane_without_stable_id_cannot_become_origin() {
        let lanes = vec![LaneRef::new("", "legacy")];
        assert_eq!(lane_id_of("legacy", &lanes), None);
    }

    /// snapshot publisher が使う adapter が、DB 越しでも純関数と同じ答えを出すこと。
    ///
    /// publish 経路は 2 本（project runtime の live push と World の retained snapshot）で、
    /// **両方が同じ解決を通らないと受け手が起点の有無で flicker する**。だから解決を
    /// [`origin_name_for_lanes`] 1 本に畳んでいる。ここではその 1 本を固定する。
    #[tokio::test]
    async fn origin_name_for_lanes_resolves_through_db() {
        use crate::process::lanes_state::LanePool;

        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();

        // conductor + performer の 2 本。performer を起点にできることが D4 の本体なので、
        // **答えが分岐する形**で組む（1 本だけだと全ケース同じ答えになり判別力ゼロ）。
        let mut lanes = LanePool::with_root("proj", "/tmp/proj").list();
        let mut performer = lanes[0].clone();
        performer.address = crate::process::lanes_state::LaneAddress::new("proj", "feat-x");
        performer.id = crate::process::lanes_state::LaneId::generate();
        let performer_id = performer.id.to_string();
        lanes.push(performer);

        // 未設定 = 予約名（既定）
        assert_eq!(
            origin_name_for_lanes(Some(&db), "/tmp/proj", &lanes).await,
            ROOT_LANE_NAME
        );

        // 帳簿が performer を指せば起点が動く（= 予約名ではなくなる）
        db.upsert_host_origin("/tmp/proj", &performer_id)
            .await
            .unwrap();
        assert_eq!(
            origin_name_for_lanes(Some(&db), "/tmp/proj", &lanes).await,
            "feat-x",
            "帳簿のポインタが publish される起点を決める"
        );

        // 指す先が居なければ既定に戻る（publish は止めない = 起点不明で snapshot を欠かさない）
        db.upsert_host_origin("/tmp/proj", "id-gone").await.unwrap();
        assert_eq!(
            origin_name_for_lanes(Some(&db), "/tmp/proj", &lanes).await,
            ROOT_LANE_NAME
        );

        // DB 不在（test fixture / 未接続）でも既定を返して publish を止めない
        assert_eq!(
            origin_name_for_lanes(None, "/tmp/proj", &lanes).await,
            ROOT_LANE_NAME
        );
    }

    /// 回帰固定: **書き手と読み手が別の形の path を渡しても同じ行を触る**。
    ///
    /// 帳簿に触る経路は 4 本あり、渡ってくる path の形が揃っていない:
    /// `AppState.project_dir`（`ProjectRuntimes::start` が受け取った生のパス）と
    /// `path_key`（`normalize_path_key` = canonicalize 済）。`ProjectRuntimes` は map key に
    /// 正規化を使いつつ `CapabilityConfig` には生を渡すので、両者は一致するとは限らない。
    ///
    /// ズレると **起点を指定しても snapshot に載らない**（書いた行と読む行が違う）。
    /// 症状は「設定が効かない」だけで error も log も出ないため、テストで固定する。
    #[tokio::test]
    async fn write_and_read_agree_across_path_shapes() {
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();

        // 実在 dir を作る（canonicalize は未実在 path では入力を素通しするため）。
        let dir = std::env::temp_dir().join(format!("vp-ledger-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.to_string_lossy().to_string();
        // 同じ dir を指すが文字列としては別物（末尾 `/.`）。
        let quirky = format!("{canonical}/.");
        assert_ne!(canonical, quirky, "文字列としては異なる前提");

        let lanes = vec![LaneRef::new("id-foo", "foo")];

        // 生のパスで書き、正規化済みのパスで読む（= 実際の write / read 経路の組み合わせ）。
        set_origin(Some(&db), &quirky, "foo", &lanes)
            .await
            .expect("起点の設定");
        let origin = origin(Some(&db), &canonical, &lanes).await;
        assert_eq!(
            origin.name, "foo",
            "path の形が違っても同じ行を読む: {origin:?}"
        );
        assert_eq!(origin.source, OriginSource::Pinned);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 並び順の適用（doc 44 §12）— 指定のある lane が先、無い lane は**元の並びのまま後ろ**。
    #[test]
    fn apply_lane_order_puts_unspecified_last_in_original_order() {
        let mut lanes = vec!["a", "b", "c", "d"];
        let order: std::collections::HashMap<String, i64> =
            [("c".to_string(), 0), ("a".to_string(), 1)]
                .into_iter()
                .collect();

        apply_lane_order(&mut lanes, &order, |l| l.to_string());

        assert_eq!(
            lanes,
            vec!["c", "a", "b", "d"],
            "指定順が先、未指定は元の相対順（b→d）のまま後ろ"
        );
    }

    /// 帳簿が空なら**何もしない**（既定順 = 開発起点が先頭 → created_at を壊さない）。
    #[test]
    fn apply_lane_order_is_noop_when_ledger_is_empty() {
        let mut lanes = vec!["root", "b", "a"];
        apply_lane_order(&mut lanes, &Default::default(), |l| l.to_string());
        assert_eq!(
            lanes,
            vec!["root", "b", "a"],
            "未指定 project の並びは既定のまま"
        );
    }

    /// 新しく作られた lane が既存の並びに**割り込まない**。
    ///
    /// 割り込むと「並べ替えたのに勝手に崩れた」に見える。未指定 = 末尾が要件。
    #[test]
    fn new_lane_does_not_cut_into_existing_order() {
        let mut lanes = vec!["fresh", "a", "b"];
        let order: std::collections::HashMap<String, i64> =
            [("a".to_string(), 0), ("b".to_string(), 1)]
                .into_iter()
                .collect();

        apply_lane_order(&mut lanes, &order, |l| l.to_string());

        assert_eq!(lanes, vec!["a", "b", "fresh"], "新 lane は末尾に付く");
    }

    /// 並び順の round-trip（write → read）。**path の形が違っても同じ行**を触る
    /// （起点と同じ `row_key` に乗っているかの固定）。
    #[tokio::test]
    async fn lane_order_round_trip_across_path_shapes() {
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();

        let dir = std::env::temp_dir().join(format!("vp-ledger-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.to_string_lossy().to_string();
        let quirky = format!("{canonical}/.");

        let lanes = vec![
            LaneRef::new("id-a", "a"),
            LaneRef::new("id-b", "b"),
            LaneRef::new("id-c", "c"),
        ];

        set_lane_order(
            Some(&db),
            &quirky,
            &["c".to_string(), "a".to_string()],
            &lanes,
        )
        .await
        .expect("並び順の保存");

        let mut names = vec!["a", "b", "c"];
        let order = db.list_lane_order(&row_key(&canonical)).await.unwrap();
        assert_eq!(order.len(), 2, "解決できた 2 件だけが入る: {order:?}");
        apply_lane_order(&mut names, &order, |n| format!("id-{n}"));
        assert_eq!(names, vec!["c", "a", "b"], "保存した順で並ぶ");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 実在しない名前は落として続行するが、**1 つも解決できなければ Err**。
    ///
    /// 一覧と帳簿の間には常に時間差がある（送信中に lane が消える等）ので、
    /// 1 件の不一致で並べ替え全体を失敗させる意味は無い。逆に全滅なら
    /// 呼び手の指定が丸ごと間違っているので黙って空を書かない。
    #[tokio::test]
    async fn set_lane_order_rejects_when_nothing_resolves() {
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        let lanes = vec![LaneRef::new("id-a", "a")];

        assert!(
            set_lane_order(Some(&db), "/tmp/x", &["ghost".to_string()], &lanes)
                .await
                .is_err(),
            "全滅なら Err"
        );
        assert!(
            set_lane_order(
                Some(&db),
                "/tmp/x",
                &["ghost".to_string(), "a".to_string()],
                &lanes
            )
            .await
            .is_ok(),
            "一部でも解決すれば保存する"
        );
    }

    /// `Origin` は unison 応答としてそのまま wire に載るので serde 形を固定する。
    #[test]
    fn origin_serde_shape() {
        let pinned = resolve_origin_name(Some("id-foo"), &lanes());
        let json = serde_json::to_value(&pinned).unwrap();
        assert_eq!(json["name"], "foo");
        assert_eq!(json["source"]["kind"], "pinned");

        let dangling = resolve_origin_name(Some("id-gone"), &lanes());
        let json = serde_json::to_value(&dangling).unwrap();
        assert_eq!(json["name"], ROOT_LANE_NAME);
        assert_eq!(json["source"]["kind"], "dangling");
        assert_eq!(json["source"]["lane_id"], "id-gone");
    }

    /// 名前の重複が無い前提（1 project 内で lane 名は一意）で、id は往復する。
    #[test]
    fn name_and_id_round_trip() {
        let lanes = lanes();
        let id = lane_id_of("foo", &lanes).expect("id");
        let origin = resolve_origin_name(Some(id), &lanes);
        assert_eq!(origin.name, "foo");
    }
}
