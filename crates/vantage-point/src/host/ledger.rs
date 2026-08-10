//! 帳簿 — Repo Host が repo について持つ現在値と履歴（doc 44 D3 / §8）。
//!
//! 第一の住人は **開発起点ポインタ**（D4）。「この repo の開発の起点はどの lane か」を
//! Host が 1 本だけ持つ。
//!
//! ## なぜ lane 自身に持たせないのか
//!
//! P2（フラット化）で `LaneKind` を撤去し、lane は全て対等になった。
//! 「起点である」は lane の属性ではなく **repo 側の指定** なので、帳簿が持つ。
//! こうすると起点の移動が「ポインタの書き換え 1 回」になり、lane は何も動かない（D5）。
//!
//! ## key が名前ではなく id な理由
//!
//! ポインタが指すのは lane **そのもの**であって「今その名前で呼ばれているもの」ではない。
//! 名前は表示のための自然キーで、将来 rename できるようにすると動く。だから帳簿は
//! [`crate::repo::lanes_state::LaneId`]（UUID v7、doc 24 §7 の I1）を key にする。
//!
//! 名前 ↔ id の解決は **境界で 1 回だけ**行う: 人が打つのは名前、帳簿に入るのは id
//! （[`resolve_origin_name`] が読み側、unison `lane_origin_set` が書き側）。
//!
//! ## フォールバックの設計
//!
//! ポインタが無い / 指す lane が実在しない場合は予約名 `main` に落ちる。
//! これは「推測」ではなく D4 が定めた既定値（main は残る）なので、Host の
//! 「推測しない」原則には反しない。ただし **dangling は隠さず事実として返す**
//! （[`OriginSource::Dangling`]）— 黙って既定に戻ると、指定したはずの起点が
//! いつの間にか動いていたことに気付けない。
//!
//! ## 第二の住人 — 見送りの記録（§7.5 / §8.5）
//!
//! [`FarewellEntry`] が「いつ何を見送ったか」と「`AskHuman` がどれだけ滞留しているか」を持つ。
//! こちらも key は `LaneId` で、**記録時点の lane 名をスナップショット**で並べて持つ
//! （履歴は rename で動いてはいけない / 同名 lane の再作成と混ざってはいけない）。

use crate::host::farewell::FarewellVerdict;
use crate::repo::lanes_state::ROOT_LANE_NAME;

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
// data + calculations — 見送りの記録（doc 44 §7.5「帳簿の永続化」）
// =============================================================================

/// 帳簿に載る見送りの記録の**種類**。
///
/// 2 つしか無いのは、帳簿に書くのが「計算で復元できない事実」だけだから（§8.5 の規律）:
///
/// - `Reclaimed` = lane を消した。**消した後は survey で復元できない**
/// - `Pending` = 人の判断待ちが**いつから何回続いているか**。今この瞬間が `AskHuman` かは
///   `survey_repo` が都度計算できるが、「いつから」「何回目」は観測の履歴なので計算できない
///
/// `Keep` と「判定だけの `Reclaim`」を書かないのは同じ理由 — どちらも次の survey で同じ答えが
/// 出る（= 復元できる）。書けば行が増えるだけで新しい事実は増えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FarewellKind {
    /// 人の判断待ち（`AskHuman` の滞留）。連続する限り 1 行に畳んで回数を数える
    Pending,
    /// 実際に見送った（lane は消えている）
    Reclaimed,
}

impl FarewellKind {
    /// DB / wire に載る文字列。
    pub fn as_str(self) -> &'static str {
        match self {
            FarewellKind::Pending => "pending",
            FarewellKind::Reclaimed => "reclaimed",
        }
    }

    /// 文字列から戻す（未知の値は `None` — 帳簿の行が読めなくても他の行は読める）。
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(FarewellKind::Pending),
            "reclaimed" => Some(FarewellKind::Reclaimed),
            _ => None,
        }
    }
}

/// 帳簿の 1 行（見送りの履歴 / `AskHuman` の滞留）。
///
/// # なぜ名前を**スナップショット**で持つのか
///
/// key は [`lane_id`](Self::lane_id) なので、rename しても行は動かない（§8.2）。だが
/// 履歴の表示に「今の名前」を引くと、**過去の記録が rename で書き換わって見える**
/// （「old-feat を見送った」が「new-feat を見送った」になる）。しかも lane を見送った後は
/// 引く先が消えているので引けない。だから記録時点の名前をその場で凍結する。
///
/// 逆に `vp lane cleanup` の**現在の行**は survey が持つ生きた名前を表示する — 帳簿から
/// 取るのは回数と初回時刻だけ。こうすると凍結名が現在の表示に漏れない。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FarewellEntry {
    /// 帳簿の key（記録時点の lane の安定 id）
    pub lane_id: String,
    /// 記録時点の lane 名（表示専用のスナップショット、後から書き換えない）
    pub lane_name: String,
    pub kind: FarewellKind,
    /// 直近に観測した判定理由
    pub reason: String,
    /// 同じ判定が**連続した回数**（1 = 初回）。
    ///
    /// ⚠️ これは「日数」ではなく「survey を回した回数」。`vp lane cleanup` を連打すれば
    /// 増える。だから [`first_seen_at`](Self::first_seen_at) を必ず添えて表示する
    /// （回数だけを見せると滞留の長さを誤読させる）。
    pub streak: u32,
    /// 連続の**初回**を観測した時刻（RFC3339 / UTC）
    pub first_seen_at: String,
    /// 直近に観測した時刻（RFC3339 / UTC）
    pub last_seen_at: String,
    /// 滞留が継続中か（`Pending` だけが true になりうる。`Reclaimed` は常に終端）
    pub ongoing: bool,
}

/// 1 lane 分の見送り判定の観測（CLI → daemon の RPC payload でもある）。
///
/// `verdict` は [`FarewellVerdict`] をそのまま flatten するので、wire 上は
/// `{"lane_id":..,"lane_name":..,"verdict":"ask_human","reason":".."}` になる
/// （`FarewellReport` と同じ形）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FarewellObservation {
    /// 帳簿の key。**lane を消す前に**解決しておくこと（消すと id の state file も消える）
    pub lane_id: String,
    /// 記録時点の lane 名
    pub lane_name: String,
    #[serde(flatten)]
    pub verdict: FarewellVerdict,
}

/// 観測 1 件を帳簿にどう反映するか（[`fold_farewell_observation`] の出力）。
///
/// DB 操作を直接返さず「何をすべきか」だけを返すので、**時刻を注入して全分岐を
/// テストで固定**できる（Host の層 1 = calculations）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FarewellWrite {
    /// 何も書かない（記録対象でない判定 + 継続中の滞留も無い）。
    ///
    /// **これが多数派**であることが書き込み量の設計そのもの — 安定した lane
    /// （起点 / 稼働中 / 未 merge の作業中）は survey のたびに 0 write で通り過ぎる
    Nothing,
    /// 継続中の滞留を閉じる（判定が `AskHuman` から外れた = 人の判断が済んだ）
    Close,
    /// 滞留を新しく起こす（streak = 1、first_seen_at = now）
    Open { at: String },
    /// 継続中の滞留を伸ばす（streak を 1 増やし、last_seen_at = now）
    Extend { streak: u32, at: String },
}

/// 観測を帳簿にどう反映するかを決める **純関数**（Host の層 1）。
///
/// # 「同じ判定の連続は 1 行に畳む」理由
///
/// `vp lane cleanup` は何度でも走る。観測のたびに行を足すと、**放置された lane ほど
/// 帳簿を太らせる**（滞留を追いたいのに、滞留が帳簿を壊す）。連続を 1 行に畳めば
/// 行数は「判定が変わった回数」に比例し、走らせた回数には比例しない。
///
/// 畳んでも失う事実は無い: 連続の間は判定も理由もほぼ同じで、意味があるのは
/// **いつから続いているか（`first_seen_at`）と何回目か（`streak`）**だけ。
///
/// # 判定が変わったら閉じる
///
/// `AskHuman` 以外（`Keep` / `Reclaim`）が来たら滞留は解消したので閉じる。閉じずに
/// 放置すると、後でまた `AskHuman` になった時に**連続していない観測が 1 本の滞留に
/// 見える**（「3 週間放置」が実は「1 回 → 解決 → 2 週後にまた 1 回」だったことになる）。
pub fn fold_farewell_observation(
    open: Option<&FarewellEntry>,
    verdict: &FarewellVerdict,
    now: &str,
) -> FarewellWrite {
    let pending = matches!(verdict, FarewellVerdict::AskHuman { .. });
    match (open, pending) {
        (None, false) => FarewellWrite::Nothing,
        (None, true) => FarewellWrite::Open {
            at: now.to_string(),
        },
        (Some(_), false) => FarewellWrite::Close,
        (Some(entry), true) => FarewellWrite::Extend {
            streak: entry.streak.saturating_add(1),
            at: now.to_string(),
        },
    }
}

/// 滞留の表示文（`3 回連続、初回 2026-07-15`）。初回の観測（streak = 1）は `None`。
///
/// 1 回目に注記を付けないのは、**滞留していない行に注記が付くと信号が薄まる**ため。
/// `vp lane cleanup` の要判断行は毎回全部出るので、「積み残されているもの」だけが
/// 目に付く形にする。
///
/// 日付は保存された RFC3339 の日付部分をそのまま出す（UTC）。local に変換しないのは、
/// 変換すると表示が実行環境の TZ に依存してテストで固定できなくなるため。滞留の粒度は
/// 日単位で足りるので、日付境界の 1 日ずれは許容する。
pub fn stagnation_note(entry: &FarewellEntry) -> Option<String> {
    if entry.streak < 2 {
        return None;
    }
    let day = entry
        .first_seen_at
        .get(..10)
        .unwrap_or(&entry.first_seen_at);
    Some(format!("{} 回連続、初回 {}", entry.streak, day))
}

/// 帳簿 1 行の表示（`vp lane history` の 1 行）。
///
/// 純関数にしてあるのは、**読み手が実際に何を出すか**をテストで固定するため
/// （帳簿の書き込みだけをテストすると「読み手のない書き込み」に戻る）。
pub fn format_history_line(entry: &FarewellEntry) -> String {
    let day = entry.last_seen_at.get(..10).unwrap_or(&entry.last_seen_at);
    match entry.kind {
        FarewellKind::Reclaimed => {
            format!("{day}  🧹 見送り  {}  {}", entry.lane_name, entry.reason)
        }
        FarewellKind::Pending => {
            let state = if entry.ongoing {
                "⚠️ 判断待ち"
            } else {
                "✓ 解消済"
            };
            let note = stagnation_note(entry)
                .map(|n| format!("（{n}）"))
                .unwrap_or_default();
            format!(
                "{day}  {state}  {}{note}  {}",
                entry.lane_name, entry.reason
            )
        }
    }
}

// =============================================================================
// actions — 帳簿の永続（DB）。判定は上の純関数が済ませている
// =============================================================================

/// 帳簿の row key を作る（**repo path の正規化はここに畳む**）。
///
/// 呼び手によって渡ってくる path が違うため:
///
/// - `AppState.repo_dir` — `RepoRuntimes::start` が受け取った**生のパス**
/// - `path_key` — `normalize_path_key`（canonicalize 済、symlink 解決後）
///
/// `RepoRuntimes` は map key に正規化を使いつつ `CapabilityConfig` には生を渡すので、
/// 両者は一致するとは限らない（macOS の `/tmp` → `/private/tmp` 等）。ズレると
/// **書き手と読み手が別の行を触り、起点を指定しても snapshot に載らない**。
///
/// 慣習では call site 側で正規化するが、帳簿は経路が 4 本（publish 2 / get / set）あり、
/// 1 つ忘れると無症状で行が割れる。**書き手が 1 module しか無い新設 table なので、
/// 正規化をここに閉じて構造的に一致させる。**
fn row_key(repo_path: &str) -> String {
    crate::capability::normalize_path_key(std::path::Path::new(repo_path))
}

/// 帳簿から起点を読む。
///
/// DB が無い（`vpdb: None` の test fixture 等）/ 読めない場合は既定に落ちる —
/// 起点が読めないだけで repo が動かなくなる方が困るため（best-effort）。
pub async fn origin(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    lanes: &[LaneRef],
) -> Origin {
    let pointer = match vpdb {
        Some(db) => match db.get_host_origin(&row_key(repo_path)).await {
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
/// `LanesSnapshot` を publish する経路が 2 本ある（repo runtime の live push と、
/// daemon が vp-app 接続時に配る retained snapshot）ので、両方が同じ解決を通るように
/// ここに畳む。**片方だけ解決すると受け手が起点の有無で flicker する。**
pub async fn origin_name_for_lanes(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    lanes: &[crate::repo::lanes_state::LaneInfo],
) -> String {
    let refs: Vec<LaneRef> = lanes
        .iter()
        .map(|l| LaneRef::new(l.id.to_string(), l.address.name.clone()))
        .collect();
    origin(vpdb, repo_path, &refs).await.name
}

/// 帳簿の並び順を lane 列に適用する（snapshot publisher / `lanes_list` 用）。
///
/// DB が無い / 読めない場合は**並べ替えない**（既定順のまま）。並び順が読めないだけで
/// lane 一覧が出なくなる方が困る。
pub async fn sort_lanes_by_ledger(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    lanes: &mut [crate::repo::lanes_state::LaneInfo],
) {
    let Some(db) = vpdb else { return };
    let order = match db.list_lane_order(&row_key(repo_path)).await {
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
    repo_path: &str,
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
    db.replace_lane_order(&row_key(repo_path), &ids)
        .await
        .map_err(|e| format!("帳簿: 並び順の永続に失敗: {e}"))
}

/// 起点を設定する（名前で受けて id で書く）。
///
/// 名前解決に失敗したら **書かない** — 存在しない lane を指すポインタを自ら作らない。
pub async fn set_origin(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    lane_name: &str,
    lanes: &[LaneRef],
) -> Result<(), String> {
    let db = vpdb.ok_or_else(|| "帳簿: DB 未接続のため起点を設定できません".to_string())?;
    let id = lane_id_of(lane_name, lanes).ok_or_else(|| {
        format!("帳簿: lane '{lane_name}' が見つからない（または安定 id を持たない）")
    })?;
    db.upsert_host_origin(&row_key(repo_path), id)
        .await
        .map_err(|e| format!("帳簿: 起点の永続に失敗: {e}"))
}

/// 見送り判定を帳簿に反映し、**反映後の滞留一覧**を返す（doc 44 §7.5）。
///
/// `now` を引数で受けるのは記録時刻をテストで固定するため（本番は呼び出し側が実時刻を渡す）。
///
/// # 書かない条件
///
/// - `vpdb` が無い（test fixture / 未接続）→ 何もしない。帳簿が無いだけで見送りを止めない
/// - `lane_id` が空 → その lane は**飛ばす**。key を持たない行を作ると、後から
///   「どの lane の履歴か」を復元できない（空 id は `lane_id_of` でも弾いている）
///
/// なお **「稼働状況が不明」で保留した時に書かない**ことは、呼び出し側の順序で保証する
/// （保留は survey に進まない = 観測が 1 件も無い）。事実が無い状態を履歴に残さない。
pub async fn record_farewell_observations(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    observations: &[FarewellObservation],
    now: &str,
) -> Vec<FarewellEntry> {
    let Some(db) = vpdb else { return Vec::new() };
    let key = row_key(repo_path);
    for obs in observations {
        if obs.lane_id.is_empty() {
            tracing::warn!(
                "帳簿: lane '{}' は安定 id を持たないので見送りの記録を飛ばす",
                obs.lane_name
            );
            continue;
        }
        let open = match db.get_open_farewell(&key, &obs.lane_id).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("帳簿: 滞留の読み出しに失敗（記録を飛ばす）: {}", e);
                continue;
            }
        };
        let write = fold_farewell_observation(open.as_ref(), &obs.verdict, now);
        let result = match write {
            FarewellWrite::Nothing => Ok(()),
            FarewellWrite::Close => db.close_open_farewell(&key, &obs.lane_id).await,
            FarewellWrite::Open { at } => {
                db.create_farewell_entry(
                    &key,
                    &FarewellEntry {
                        lane_id: obs.lane_id.clone(),
                        lane_name: obs.lane_name.clone(),
                        kind: FarewellKind::Pending,
                        reason: obs.verdict.reason().to_string(),
                        streak: 1,
                        first_seen_at: at.clone(),
                        last_seen_at: at,
                        ongoing: true,
                    },
                )
                .await
            }
            FarewellWrite::Extend { streak, at } => {
                db.extend_open_farewell(&key, &obs.lane_id, streak, obs.verdict.reason(), &at)
                    .await
            }
        };
        if let Err(e) = result {
            tracing::warn!("帳簿: 見送り判定の記録に失敗（続行）: {}", e);
        }
    }
    match db.list_open_farewells(&key).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("帳簿: 滞留一覧の読み出しに失敗: {}", e);
            Vec::new()
        }
    }
}

/// 実際に見送った lane を帳簿に記録する（終端 event、doc 44 §7.5）。
///
/// 判定（`Reclaim`）ではなく**実行**を書くのがここ。判定は次の survey で同じ答えが出る =
/// 復元できるが、消した lane は復元できない。戻り値は記録できた件数。
pub async fn record_farewell_reclaimed(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    entries: &[FarewellObservation],
    now: &str,
) -> usize {
    let Some(db) = vpdb else { return 0 };
    let key = row_key(repo_path);
    let mut written = 0usize;
    for obs in entries {
        if obs.lane_id.is_empty() {
            tracing::warn!(
                "帳簿: lane '{}' は安定 id を持たないので見送りの記録を飛ばす",
                obs.lane_name
            );
            continue;
        }
        // 見送った lane に滞留が残っていたら閉じる（消えた lane が判断待ちのまま残らない）。
        if let Err(e) = db.close_open_farewell(&key, &obs.lane_id).await {
            tracing::warn!("帳簿: 滞留の終端に失敗（続行）: {}", e);
        }
        let entry = FarewellEntry {
            lane_id: obs.lane_id.clone(),
            lane_name: obs.lane_name.clone(),
            kind: FarewellKind::Reclaimed,
            reason: obs.verdict.reason().to_string(),
            streak: 1,
            first_seen_at: now.to_string(),
            last_seen_at: now.to_string(),
            ongoing: false,
        };
        match db.create_farewell_entry(&key, &entry).await {
            Ok(()) => written += 1,
            Err(e) => tracing::warn!("帳簿: 見送りの記録に失敗（続行）: {}", e),
        }
    }
    written
}

/// 帳簿の見送り記録を新しい順に読む（`vp lane history` の供給元）。
pub async fn farewell_history(
    vpdb: Option<&crate::db::SharedVpDb>,
    repo_path: &str,
    limit: usize,
) -> Vec<FarewellEntry> {
    let Some(db) = vpdb else { return Vec::new() };
    match db.list_farewell_entries(&row_key(repo_path), limit).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("帳簿: 見送り履歴の読み出しに失敗: {}", e);
            Vec::new()
        }
    }
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
    /// これが D4 の本体 — 起点は lane の役割ではなく repo の指定。
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
    /// publish 経路は 2 本（repo runtime の live push と daemon の retained snapshot）で、
    /// **両方が同じ解決を通らないと受け手が起点の有無で flicker する**。だから解決を
    /// [`origin_name_for_lanes`] 1 本に畳んでいる。ここではその 1 本を固定する。
    #[tokio::test]
    async fn origin_name_for_lanes_resolves_through_db() {
        use crate::repo::lanes_state::LanePool;

        // ⚠️ `with_root` は **実 PTY を spawn** し、その replay を `vp_state_dir()` に書く。
        // 隔離しないと user の実 state（`~/.local/state/vp/terminal_replay/proj__root__1`）を
        // 汚し、かつテストが hermetic でなくなる（実 state 依存で実行回数によって挙動が変わる
        // — 過去に同型で CI だけ落ちた）。doc 50 §4.6 A6 の作業中に発見（2026-07-25）。
        let _state = crate::test_env::state_dir_async().await;
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();

        // main + sub の 2 本。sub を起点にできることが D4 の本体なので、
        // **答えが分岐する形**で組む（1 本だけだと全ケース同じ答えになり判別力ゼロ）。
        let mut lanes = LanePool::with_root("proj", "/tmp/proj").list();
        let mut sub = lanes[0].clone();
        sub.address = crate::repo::lanes_state::LaneAddress::new("proj", "feat-x");
        sub.id = crate::repo::lanes_state::LaneId::generate();
        let sub_id = sub.id.to_string();
        lanes.push(sub);

        // 未設定 = 予約名（既定）
        assert_eq!(
            origin_name_for_lanes(Some(&db), "/tmp/proj", &lanes).await,
            ROOT_LANE_NAME
        );

        // 帳簿が sub を指せば起点が動く（= 予約名ではなくなる）
        db.upsert_host_origin("/tmp/proj", &sub_id).await.unwrap();
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
    /// `AppState.repo_dir`（`RepoRuntimes::start` が受け取った生のパス）と
    /// `path_key`（`normalize_path_key` = canonicalize 済）。`RepoRuntimes` は map key に
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
            "未指定 repo の並びは既定のまま"
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

    /// 名前の重複が無い前提（1 repo 内で lane 名は一意）で、id は往復する。
    #[test]
    fn name_and_id_round_trip() {
        let lanes = lanes();
        let id = lane_id_of("foo", &lanes).expect("id");
        let origin = resolve_origin_name(Some(id), &lanes);
        assert_eq!(origin.name, "foo");
    }

    // =========================================================================
    // 見送りの記録（doc 44 §7.5）
    // =========================================================================

    fn ask(reason: &str) -> FarewellVerdict {
        FarewellVerdict::AskHuman {
            reason: reason.to_string(),
        }
    }

    fn observation(id: &str, name: &str, verdict: FarewellVerdict) -> FarewellObservation {
        FarewellObservation {
            lane_id: id.to_string(),
            lane_name: name.to_string(),
            verdict,
        }
    }

    fn pending_entry(streak: u32, first: &str) -> FarewellEntry {
        FarewellEntry {
            lane_id: "id-1".to_string(),
            lane_name: "foo".to_string(),
            kind: FarewellKind::Pending,
            reason: "未コミットの変更".to_string(),
            streak,
            first_seen_at: first.to_string(),
            last_seen_at: first.to_string(),
            ongoing: true,
        }
    }

    async fn mem_db() -> crate::db::SharedVpDb {
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        db
    }

    /// 記録対象でない判定は**何も書かない**。ここが書き込み量の設計そのもの。
    ///
    /// 安定した lane（起点 / 稼働中 / 作業中）は survey のたびに 0 write で通り過ぎる。
    /// ここが `Open` を返すようになると、`vp lane cleanup` を回すたびに全 lane 分の行が
    /// 増えて帳簿が実行回数に比例して太る。
    #[test]
    fn stable_verdicts_write_nothing() {
        for verdict in [
            FarewellVerdict::Keep {
                reason: "稼働中".to_string(),
            },
            FarewellVerdict::Reclaim {
                reason: "merge 済み".to_string(),
            },
        ] {
            assert_eq!(
                fold_farewell_observation(None, &verdict, "2026-07-15T00:00:00+00:00"),
                FarewellWrite::Nothing,
                "滞留していない lane は 1 度も書かない: {verdict:?}"
            );
        }
    }

    /// `AskHuman` の連続は**行を増やさず回数を増やす**（1 行に畳む）。
    #[test]
    fn consecutive_ask_human_folds_into_streak() {
        let first = fold_farewell_observation(None, &ask("dirty"), "2026-07-15T00:00:00+00:00");
        assert_eq!(
            first,
            FarewellWrite::Open {
                at: "2026-07-15T00:00:00+00:00".to_string()
            }
        );

        let open = pending_entry(1, "2026-07-15T00:00:00+00:00");
        assert_eq!(
            fold_farewell_observation(Some(&open), &ask("dirty"), "2026-07-16T00:00:00+00:00"),
            FarewellWrite::Extend {
                streak: 2,
                at: "2026-07-16T00:00:00+00:00".to_string()
            },
            "2 回目は新しい行ではなく既存の滞留を伸ばす"
        );
    }

    /// 判定が滞留から外れたら**閉じる**。
    ///
    /// 閉じないと、後でまた `AskHuman` になった時に連続していない観測が 1 本の滞留に
    /// 見える（「3 週間放置」が実は「1 回 → 解決 → 2 週後にまた 1 回」だったことになる）。
    #[test]
    fn resolved_pending_is_closed_not_extended() {
        let open = pending_entry(3, "2026-07-15T00:00:00+00:00");
        assert_eq!(
            fold_farewell_observation(
                Some(&open),
                &FarewellVerdict::Keep {
                    reason: "未 merge の commit".to_string()
                },
                "2026-07-20T00:00:00+00:00"
            ),
            FarewellWrite::Close
        );
    }

    /// 滞留の注記は 2 回目から出る（1 回目は滞留ではない）。
    #[test]
    fn stagnation_note_starts_at_second_observation() {
        assert_eq!(
            stagnation_note(&pending_entry(1, "2026-07-15T00:00:00+00:00")),
            None,
            "初回に注記を付けると要判断行が全部注記だらけになって信号が薄まる"
        );
        assert_eq!(
            stagnation_note(&pending_entry(3, "2026-07-15T09:00:00+00:00")).as_deref(),
            Some("3 回連続、初回 2026-07-15"),
            "回数だけでなく初回時刻を必ず添える（回数は実行回数なので連打で膨らむ）"
        );
    }

    /// 帳簿の round-trip: 判定 → 記録 → 滞留として読み戻る。
    ///
    /// **時刻を注入**して固定値で検証する（記録時点の実時刻は本番の呼び出し側が渡す）。
    #[tokio::test]
    async fn observations_accumulate_stagnation() {
        let db = mem_db().await;
        let obs = vec![observation("id-1", "foo", ask("未コミットの変更が 1 件"))];

        let pending =
            record_farewell_observations(Some(&db), "/tmp/proj", &obs, "2026-07-15T00:00:00+00:00")
                .await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].streak, 1);
        assert_eq!(pending[0].first_seen_at, "2026-07-15T00:00:00+00:00");

        let pending =
            record_farewell_observations(Some(&db), "/tmp/proj", &obs, "2026-07-16T00:00:00+00:00")
                .await;
        assert_eq!(pending.len(), 1, "行は増えない（連続は 1 行に畳む）");
        assert_eq!(pending[0].streak, 2);
        assert_eq!(
            pending[0].first_seen_at, "2026-07-15T00:00:00+00:00",
            "初回時刻は動かない"
        );
        assert_eq!(pending[0].last_seen_at, "2026-07-16T00:00:00+00:00");

        // 判定が変われば滞留は解消 = 一覧から消える（行は履歴として残る）
        let keep = vec![observation(
            "id-1",
            "foo",
            FarewellVerdict::Keep {
                reason: "未 merge の commit が 2 件ある".to_string(),
            },
        )];
        let pending = record_farewell_observations(
            Some(&db),
            "/tmp/proj",
            &keep,
            "2026-07-17T00:00:00+00:00",
        )
        .await;
        assert!(pending.is_empty(), "解消した滞留は滞留一覧に出ない");
        let history = farewell_history(Some(&db), "/tmp/proj", 0).await;
        assert_eq!(history.len(), 1, "行は履歴として残る");
        assert!(!history[0].ongoing);
        assert_eq!(history[0].streak, 2);

        // 再発は**新しい滞留**として起票される（前の 2 回と繋がらない）
        let pending =
            record_farewell_observations(Some(&db), "/tmp/proj", &obs, "2026-07-20T00:00:00+00:00")
                .await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].streak, 1, "解消を挟んだら連続ではない");
        assert_eq!(pending[0].first_seen_at, "2026-07-20T00:00:00+00:00");
    }

    /// 回帰固定: **rename しても履歴が動かない**（key が id である意味）。
    ///
    /// 同じ id を別名で観測した時に:
    /// - 滞留は**繋がる**（key が名前なら別 lane 扱いで streak が 1 に戻る）
    /// - 既存行の `lane_name` は**記録時点のまま**（更新すると過去の記録が rename で
    ///   書き換わり、「old-feat を見送った」が「new-feat を見送った」になる）
    #[tokio::test]
    async fn rename_keeps_history_in_place() {
        let db = mem_db().await;
        record_farewell_observations(
            Some(&db),
            "/tmp/proj",
            &[observation("id-1", "old-name", ask("未コミットの変更"))],
            "2026-07-15T00:00:00+00:00",
        )
        .await;

        // rename 後の観測（id は同じ、名前だけ変わる）
        let pending = record_farewell_observations(
            Some(&db),
            "/tmp/proj",
            &[observation("id-1", "new-name", ask("未コミットの変更"))],
            "2026-07-16T00:00:00+00:00",
        )
        .await;

        assert_eq!(pending.len(), 1, "rename しても行は分裂しない");
        assert_eq!(pending[0].streak, 2, "滞留は id で繋がる");
        assert_eq!(
            pending[0].lane_name, "old-name",
            "名前は記録時点のスナップショット（履歴は rename で動かない）"
        );
    }

    /// 回帰固定: **同名 lane を作り直しても前の履歴と混ざらない**。
    ///
    /// lane を消すと `lane_ids` state file も消える（`clear_lane_state_in`）ので、同名で
    /// 作り直した lane は必ず別 id になる。帳簿が名前 key だと、前の lane の滞留 3 回を
    /// 新しい lane が引き継いでしまう（= 作ったばかりの lane が「3 回放置されている」）。
    #[tokio::test]
    async fn recreated_lane_does_not_inherit_history() {
        let db = mem_db().await;
        for now in [
            "2026-07-15T00:00:00+00:00",
            "2026-07-16T00:00:00+00:00",
            "2026-07-17T00:00:00+00:00",
        ] {
            record_farewell_observations(
                Some(&db),
                "/tmp/proj",
                &[observation("id-old", "foo", ask("未コミットの変更"))],
                now,
            )
            .await;
        }
        // 旧 lane を見送る（= 削除）。以後この id は二度と現れない。
        record_farewell_reclaimed(
            Some(&db),
            "/tmp/proj",
            &[observation(
                "id-old",
                "foo",
                FarewellVerdict::Reclaim {
                    reason: "merge 済みで作業残なし".to_string(),
                },
            )],
            "2026-07-18T00:00:00+00:00",
        )
        .await;

        // 同じ名前で作り直し（新しい安定 id）
        let pending = record_farewell_observations(
            Some(&db),
            "/tmp/proj",
            &[observation("id-new", "foo", ask("未コミットの変更"))],
            "2026-07-19T00:00:00+00:00",
        )
        .await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].lane_id, "id-new");
        assert_eq!(
            pending[0].streak, 1,
            "作り直した lane は前の滞留を引き継がない"
        );

        // 旧 lane の記録は履歴として残り、見送りが終端している
        let history = farewell_history(Some(&db), "/tmp/proj", 0).await;
        let reclaimed: Vec<_> = history
            .iter()
            .filter(|e| e.kind == FarewellKind::Reclaimed)
            .collect();
        assert_eq!(reclaimed.len(), 1, "見送りは 1 件記録されている");
        assert_eq!(reclaimed[0].lane_id, "id-old");
        assert!(
            history
                .iter()
                .all(|e| e.kind != FarewellKind::Pending || !e.ongoing || e.lane_id == "id-new"),
            "旧 lane の滞留は見送りで閉じている: {history:?}"
        );
    }

    /// 安定 id を持たない lane（空 id）は記録しない。
    ///
    /// key の無い行を作ると「どの lane の履歴か」を後から復元できない
    /// （起点ポインタが空 id を弾くのと同じ規律）。
    #[tokio::test]
    async fn lane_without_stable_id_is_not_recorded() {
        let db = mem_db().await;
        let pending = record_farewell_observations(
            Some(&db),
            "/tmp/proj",
            &[observation("", "legacy", ask("未コミットの変更"))],
            "2026-07-15T00:00:00+00:00",
        )
        .await;
        assert!(pending.is_empty());
        assert!(farewell_history(Some(&db), "/tmp/proj", 0).await.is_empty());
    }

    /// 帳簿の行も `row_key` に乗る（書き手と読み手の path の形が違っても同じ repo）。
    ///
    /// 起点 / 並び順で固定したのと同じ回帰。ズレると `vp lane cleanup` が書いた滞留を
    /// `vp lane history` が読めない（症状は「記録が消えた」だけで error は出ない）。
    #[tokio::test]
    async fn farewell_rows_share_the_normalized_row_key() {
        let db = mem_db().await;
        let dir = std::env::temp_dir().join(format!("vp-ledger-farewell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.to_string_lossy().to_string();
        let quirky = format!("{canonical}/.");

        record_farewell_observations(
            Some(&db),
            &quirky,
            &[observation("id-1", "foo", ask("未コミットの変更"))],
            "2026-07-15T00:00:00+00:00",
        )
        .await;
        let history = farewell_history(Some(&db), &canonical, 0).await;
        assert_eq!(history.len(), 1, "path の形が違っても同じ行を読む");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DB 不在（test fixture / 未接続）でも落ちない — 帳簿が無いだけで見送りは止めない。
    #[tokio::test]
    async fn missing_db_is_a_noop() {
        let obs = [observation("id-1", "foo", ask("dirty"))];
        assert!(
            record_farewell_observations(None, "/tmp/proj", &obs, "2026-07-15")
                .await
                .is_empty()
        );
        assert_eq!(
            record_farewell_reclaimed(None, "/tmp/proj", &obs, "2026-07-15").await,
            0
        );
        assert!(farewell_history(None, "/tmp/proj", 0).await.is_empty());
    }

    /// 履歴の表示（`vp lane history` の 1 行）。読み手が何を出すかを固定する。
    #[test]
    fn history_line_shows_kind_and_snapshot_name() {
        let reclaimed = FarewellEntry {
            lane_id: "id-1".to_string(),
            lane_name: "old-feat".to_string(),
            kind: FarewellKind::Reclaimed,
            reason: "merge 済みで作業残なし".to_string(),
            streak: 1,
            first_seen_at: "2026-07-18T03:00:00+00:00".to_string(),
            last_seen_at: "2026-07-18T03:00:00+00:00".to_string(),
            ongoing: false,
        };
        let line = format_history_line(&reclaimed);
        assert!(line.starts_with("2026-07-18"), "日付が先頭: {line}");
        assert!(line.contains("見送り"), "種別が出る: {line}");
        assert!(
            line.contains("old-feat"),
            "記録時点の名前が出る（今の名前ではない）: {line}"
        );

        let mut pending = pending_entry(3, "2026-07-15T00:00:00+00:00");
        pending.last_seen_at = "2026-07-21T00:00:00+00:00".to_string();
        let line = format_history_line(&pending);
        assert!(line.contains("判断待ち"), "{line}");
        assert!(line.contains("3 回連続、初回 2026-07-15"), "{line}");
    }

    /// `FarewellObservation` は wire に載るので serde 形を固定する。
    ///
    /// `verdict` は flatten なので `{"verdict":"ask_human","reason":".."}` の形。
    /// ここがズレると CLI の観測が daemon で `serde_json::from_value` に落ちて、
    /// **記録だけが黙って止まる**（cleanup 自体は動くので気付けない）。
    #[test]
    fn observation_serde_shape() {
        let obs = observation("id-1", "foo", ask("未コミットの変更が 1 件"));
        let json = serde_json::to_value(&obs).unwrap();
        assert_eq!(json["lane_id"], "id-1");
        assert_eq!(json["lane_name"], "foo");
        assert_eq!(json["verdict"], "ask_human");
        assert_eq!(json["reason"], "未コミットの変更が 1 件");
        assert_eq!(
            serde_json::from_value::<FarewellObservation>(json).unwrap(),
            obs,
            "往復する（daemon 側で観測を読み戻せる）"
        );
    }
}
