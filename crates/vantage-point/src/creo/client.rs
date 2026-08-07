//! creo-memories（creo-app-server）の REST client — ACTIONS の読み（doc 57 Phase 3）。
//!
//! ## なぜ daemon が fetch するのか
//!
//! webview から外部 HTTP を叩く前例は VP にゼロで、CORS は相手が決めるし、token を JS に
//! 渡すことにもなる。**daemon が fetch して `/api/health` で流す**のが唯一の筋 —
//! hub federation / in-app update と同じ雛形（`hub_client.rs` → `/api/health` →
//! `spawn_activity_poller` → `SidebarState`）で、写せる完成形がある。
//!
//! ## 表示ゲートが tag なのは creo の list API が metadata で絞れないから
//!
//! doc 57 §3 は当初ゲートを `metadata.vp.board == "actions"` に置いていたが、creo の
//! `GET /api/memories` が **server-side で絞れるのは category / tags / conceptIds / atlasId /
//! status / keyword / 日付だけ**で、metadata を見る条件は 1 つも無い（一次資料:
//! `creo-memories` の `packages/creo-memories/src/services/memory-list.ts` の WHERE builder）。
//! 実測 2726 件 / `limit` 上限 100 なので client 側で絞ると **30s ごとに 28 往復**になる。
//!
//! そこで **tag [`ACTIONS_TAG`] を唯一のゲート**にした（mako 裁定 2026-08-04）。
//! metadata.vp は区画（`bucket`）と並び（`order`）だけを持つ。
//!
//! ⚠️ **ゲートを 2 本持たない**（tag と `metadata.vp.board` を併記しない）。同じ 1 つの事実を
//! 指す signal が 2 本あると必ず片方だけ書かれる日が来て、「creo には在るのに VP に出ない」が
//! 無言で起きる。
//!
//! 副次的な利点として、tag は creo の UI から人が付けられる = **既存 memory を手で ACTIONS へ
//! 引き取れる**（`metadata.vp.board` は人の目に見えないので原理的にできなかった）。区画未設定の
//! 引き取りは webview の `normalizeActions` が TODOs 末尾へ丸める。

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;

/// creo-app-server の base URL の default。
///
/// env `VP_CREO_URL` で上書き可（staging / 別 tenant を試す用）。
pub const DEFAULT_CREO_URL: &str = "https://app.creo-memories.in";

/// ACTIONS の表示ゲート。**この tag が付いた memory だけ** sidebar に出る。
///
/// `:` を含めないのは list API の `tags` が**カンマ区切り**で渡る query param だから
/// （`,` 以外は通るが、素直な kebab に寄せて query 上で紛れないようにする）。
pub const ACTIONS_TAG: &str = "vp-actions";

/// 1 回の取得で引く上限（creo の `limit` は 100 が上限）。
const FETCH_LIMIT: u32 = 100;

/// 外部 HTTP なので短めに切る（daemon の他の仕事を待たせない）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// token の期限をこの秒数切っていたら先に巻き直す（`credentials_refreshed_if_needed` の skew）。
const REFRESH_SKEW_SECS: u64 = 60;

/// creo-app-server の base URL を解決する（env 上書き > default、末尾 `/` は落とす）。
pub fn creo_base_url() -> String {
    let raw = std::env::var("VP_CREO_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CREO_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// sidebar の ACTIONS 1 件（`/api/health` の `actions[]` 要素）。
///
/// vp-app 側 `crate::pane::ActionItem` と同形にしてあり、中間 mapping を持たない
/// （`HubNodeInfo` / `HubNode` と同じ流儀）。
///
/// ⚠️ ここでは creo の言い分を**そのまま運ぶ**（`bucket` / `order` が空でも埋めない）。
/// 正すのは webview の `normalizeActions` 1 箇所に閉じる — 両側で既定値を持つと
/// 「どちらが正か」が二重になる。
/// ⚠️ `Deserialize` も要る — 読み（creo → `/api/health`）だけでなく、書き
/// （sidebar → `daemon-control.actions/save` の [`ActionsWrite`]）でも同じ形を受けるため。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CreoAction {
    /// creo の memory id（`mem_xxx`）。Action の同一性はこれ 1 本。
    pub id: String,
    /// タイトル + 内容。1 行目がタイトル、2 行目以降が内容（doc 57 §3）。
    #[serde(default)]
    pub text: String,
    /// 完了（creo の `status == "done"`）。
    ///
    /// ⚠️ `default` が要る — webview の `ActionItem.done` は optional なので、まだ一度も
    /// トグルしていない行の JSON には**この key が無い**。無いと書き込みが丸ごと弾かれる。
    #[serde(default)]
    pub done: bool,
    /// 区画（`metadata.vp.bucket`）。未設定は空文字。
    #[serde(default)]
    pub bucket: String,
    /// 区画内の並び（`metadata.vp.order`）。未設定は空文字。
    #[serde(default)]
    pub order: String,
}

/// ACTIONS 一覧の snapshot（版つき）。
#[derive(Debug, Clone, Default)]
pub struct ActionsSnapshot {
    /// 版。**内容が変わった時だけ**上がる。`0` = 一度も取得していない。
    ///
    /// これがあるので webview は「同じ内容の再 push」を当てずに済む
    /// （5s ごとに撃ち返すと、編集中の行を書き戻して caret が飛ぶ）。
    pub rev: u32,
    pub items: Vec<CreoAction>,
}

/// ACTIONS の cache + creo との往復口（doc 57 Phase 3-4）。
///
/// `Option` にせず**常設**にするのは、poller を持たない repo / test mode が自然に「空」へ
/// degrade するため（`HubNodesCache` と同じ理屈）。`Option` だと読み手側で
/// 「不在 = 未取得 or 非対応」の 2 義が生まれる。
///
/// 外に見せる動詞は [`refresh`](Self::refresh)（読み）と [`save`](Self::save)（書き）の 2 つで、
/// **どちらも同じ門（`gate`）を通る**。creo との往復は network 時間かかるので、直列化しないと
/// 「poll が読んだ古い一覧」と「save が書いた新しい一覧」が交差して片方が消える。
#[derive(Clone, Default)]
pub struct CreoActionsCache {
    snapshot: Arc<std::sync::RwLock<ActionsSnapshot>>,
    /// creo との往復（poll / save）を直列化する門。
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl CreoActionsCache {
    /// 初期状態 = 空 + `rev: 0`（= 未取得）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 一覧を差し替える。**内容が変わった時だけ `rev` を上げ、`true` を返す**。
    ///
    /// 空 vec を渡せば clear（creo から logout した時に stale な Action を残さない）。
    pub fn set(&self, items: Vec<CreoAction>) -> bool {
        let mut g = self.snapshot.write().unwrap_or_else(|e| e.into_inner());
        if g.items == items {
            return false;
        }
        g.items = items;
        // ⚠️ `0` は「未取得」の意味を持たせてあるので、一周しても 0 に戻さない。
        g.rev = match g.rev.wrapping_add(1) {
            0 => 1,
            n => n,
        };
        true
    }

    /// 現在の snapshot（reader = `/api/health` handler）。
    pub fn get(&self) -> ActionsSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// creo から引き直して cache を温める（30s poller が呼ぶ）。
    ///
    /// 未ログインなら空にする。失敗なら**据え置き**（直前まで正しかった一覧を消さない）。
    pub async fn refresh(&self) -> Result<()> {
        let _gate = self.gate.lock().await;
        match fetch_actions().await {
            Ok(Some(fetched)) => {
                let merged = merge_fetched(fetched, &self.get().items);
                if self.set(merged) {
                    tracing::debug!(rev = self.get().rev, "ACTIONS 更新");
                }
                Ok(())
            }
            Ok(None) => {
                if self.set(Vec::new()) {
                    tracing::debug!("creo 未ログイン — ACTIONS を空にしました");
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// webview の編集を creo に書いて、書いた結果で cache を差し替える（write-through）。
    ///
    /// **cache を先に更新するのが要点** — 書いた直後の 5s push が古い内容を返すと、
    /// user の編集が一瞬戻って見える。未ログインなら何も書かない（`Ok(false)`）。
    pub async fn save(&self, write: &ActionsWrite) -> Result<bool> {
        let _gate = self.gate.lock().await;
        let prev = self.get().items;
        match save_actions(write, &prev).await? {
            Some(saved) => {
                self.set(merge_saved(saved, write, &prev));
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// `GET /api/memories` の応答（要る field だけ）。
#[derive(Debug, serde::Deserialize)]
struct ListResponse {
    #[serde(default)]
    memories: Vec<CreoMemory>,
    /// filter 後の総件数。`FETCH_LIMIT` を超えたら取りこぼしているので warn を出す。
    #[serde(default)]
    total: u32,
}

/// memory 1 件（ACTIONS に要る field だけ。creo は他にも多数返すが serde が捨てる）。
#[derive(Debug, serde::Deserialize)]
struct CreoMemory {
    #[serde(default)]
    id: String,
    #[serde(default)]
    content: String,
    /// `"active"` | `"done"`。**付いていない memory もある**（IDEAs / EVENTs は付けない設計）。
    #[serde(default)]
    status: Option<String>,
    /// creo の `metadata` は FLEXIBLE object なので、形を決めつけず `Value` で受ける。
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

/// `metadata.vp` — VP の名前空間（doc 57 §3）。他 client が触らないことを名前で示す。
#[derive(Debug, Default, serde::Deserialize)]
struct VpMeta {
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    order: String,
}

impl CreoMemory {
    /// memory → Action。id を持たないものは捨てる（同一性が無いと focus も更新も追えない）。
    fn into_action(self) -> Option<CreoAction> {
        if self.id.is_empty() {
            return None;
        }
        // `metadata.vp` が無い / 形が違う（手で付けた tag 等）は既定値へ倒す。
        // 表示を止めるより「TODOs の末尾に出る」方が data を失わない。
        let vp = self
            .metadata
            .as_ref()
            .and_then(|m| m.get("vp"))
            .cloned()
            .and_then(|v| serde_json::from_value::<VpMeta>(v).ok())
            .unwrap_or_default();
        Some(CreoAction {
            id: self.id,
            text: self.content,
            done: self.status.as_deref() == Some("done"),
            bucket: vp.bucket,
            order: vp.order,
        })
    }
}

/// 応答 JSON → Action 一覧（**network を持たない純粋変換**、test はここを撃つ）。
fn parse_actions(body: &str) -> Result<Vec<CreoAction>> {
    let resp: ListResponse =
        serde_json::from_str(body).context("creo /api/memories の JSON を parse できません")?;
    let dropped = resp.total as usize > resp.memories.len();
    let items: Vec<CreoAction> = resp
        .memories
        .into_iter()
        .filter_map(|m| m.into_action())
        .collect();
    // ⚠️ 黙って切らない。上限に当たっていることが log に出ないと「全部見えている」と読める。
    if dropped {
        tracing::warn!(
            total = resp.total,
            fetched = items.len(),
            "ACTIONS が取得上限（{FETCH_LIMIT}）を超えています — 超過分は sidebar に出ません"
        );
    }
    Ok(items)
}

/// creo から ACTIONS を引く。
///
/// 戻り値の 3 値には**別々の意味**があり、呼び手の反応も違う:
///
/// - `Ok(Some(items))` — 取れた（cache を差し替える）
/// - `Ok(None)` — **creo に未ログイン**（error ではない。cache は clear する —
///   logout した user の Action を残さない）
/// - `Err(_)` — 取得に失敗（network / creo 側）。**cache は据え置く** — 直前まで正しかった
///   一覧を消すより、少し古いものを見せる方が嘘が小さい
pub async fn fetch_actions() -> Result<Option<Vec<CreoAction>>> {
    let audience = crate::commands::auth::creo_audience();
    // 期限が近ければ先に巻き直す（hub 接続が接続直前に同じことをしているのと同型）。
    let Some(creds) =
        crate::commands::auth::credentials_refreshed_if_needed(&audience, REFRESH_SKEW_SECS)
            .await?
    else {
        return Ok(None);
    };

    let url = format!(
        "{}/api/memories?tags={ACTIONS_TAG}&limit={FETCH_LIMIT}",
        creo_base_url()
    );
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .context("HTTP client の構築に失敗")?;
    let resp = client
        .get(&url)
        .header("authorization", format!("Bearer {}", creds.access_token))
        .send()
        .await
        .with_context(|| format!("creo への接続に失敗: {url}"))?;

    let status = resp.status();
    // 401 = token が通らなかった。**未ログインと同じ扱いにはしない** — 「ログインしているのに
    // 弾かれている」は user が知るべき状態で、cache を黙って空にすると原因が消える。
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("creo /api/memories が {status} を返しました: {body}");
    }
    let body = resp.text().await.context("creo 応答の読み取りに失敗")?;
    Ok(Some(parse_actions(&body)?))
}

// =============================================================================
// 書き（doc 57 Phase 4）
// =============================================================================

/// webview から届く「今の一覧」1 回分。
///
/// ⚠️ **`items` に無い = 消す、ではない**。webview の一覧は起動直後や push 到着前に
/// 短く見えることがある（⌘b で 1 件だけ捕まえた直後など）ので、**不在から削除を推論すると
/// 一瞬で memory を全部消す**。消すのは user が明示的に消した [`removed`](Self::removed) だけ。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ActionsWrite {
    /// 現在の一覧（差分ではなく全件）。
    #[serde(default)]
    pub items: Vec<CreoAction>,
    /// user が明示的に消した id。**ここに無い id は決して消さない**。
    #[serde(default)]
    pub removed: Vec<String>,
}

/// VP が採番した local id か（webview の `newActionId` が `act-` を付ける）。
///
/// ⚠️ 判定を「`mem_` で始まるか」に**しない**のは、未知の形を踏んだときに倒れる方向が
/// 逆になるから。`act-` 以外を既存扱いにすれば最悪 PUT が 404 で終わる（log が出るだけ）が、
/// 逆にすると**同じ Action の memory が poll のたびに増える**。
fn is_local_id(id: &str) -> bool {
    id.starts_with("act-")
}

/// creo に書く時の `metadata.vp`。
///
/// ⚠️ creo の update は metadata を **top-level だけ shallow merge** する
/// （`services/memory.ts` の `mergedMetadata`）。`vp` の中身は**丸ごと置き換わる**ので、
/// Phase 5 で `lane` を足す時は**ここに必ず載せる**こと（載せ忘れると書くたびに消える）。
fn vp_metadata(item: &CreoAction) -> serde_json::Value {
    serde_json::json!({ "vp": { "bucket": item.bucket, "order": item.order } })
}

/// 区画 → creo の `status`（doc 57 §3 の線引き）。
///
/// `None` = status を触らない。IDEAs / EVENTs は task ではないので `active` を付けず、
/// mako の `list_todos` を思いつきで埋めない。
///
/// ⚠️ **一度付いた status を creo API から外す手段は無い**（PUT の `status` は
/// `active | done` の enum で、省略 = 変更なし）。NEXTs → IDEAs と移した Action は
/// `active` を持ったまま残る。新規の IDEAs は綺麗なので、汚れるのは「移した時」だけ。
fn status_for(item: &CreoAction) -> Option<&'static str> {
    if item.done {
        return Some("done");
    }
    match item.bucket.as_str() {
        "currents" | "nexts" | "waits" | "todos" => Some("active"),
        // IDEAs / EVENTs / 未知の区画は付けない
        _ => None,
    }
}

/// 1 件をどう書くか。
///
/// ⚠️ **どの枝も `out` に 1 件残す**（= 書き方の分類であって、残すかの分類ではない）。
/// 「creo に上げない」を `continue` で表していた頃、空の新規行が `out` から落ちて
/// **cache ごと消え、足したばかりの行が画面から消滅**した（`⌘ hard b` → 数字で行が出ない、
/// 2026-08-07）。分類を型にして `match` を網羅にすることで、push し忘れを構造的に塞ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePlan {
    /// 空の新規行 — creo には上げない（空の memory をゴミとして作らない）が local には残す。
    KeepLocal,
    /// 新規 — POST する。
    Create,
    /// 既存で変化なし — 撃たない（5s ごとの push で無駄な PUT を出さない）。
    Unchanged,
    /// 既存で変化あり — PUT する。
    Update,
}

/// [`WritePlan`] の判定（純関数 — network を触らないのでここだけ test できる）。
fn plan_write(item: &CreoAction, prev: Option<&CreoAction>) -> WritePlan {
    if is_local_id(&item.id) {
        return if item.text.trim().is_empty() {
            WritePlan::KeepLocal
        } else {
            WritePlan::Create
        };
    }
    if prev.is_some_and(|p| p == item) {
        WritePlan::Unchanged
    } else {
        WritePlan::Update
    }
}

/// creo に書く（create / update / delete）。**cache の持ち主だけが呼ぶ**。
///
/// 戻り値は**書いた後の一覧**（新規は creo の id に差し替わる）。`None` = 未ログイン。
///
/// 個々の失敗では全体を止めない（1 件の network 失敗で残りの編集を捨てない）。失敗した新規は
/// local id のまま戻るので、cache に残って次の tick / 次の編集で再試行される。
pub async fn save_actions(
    write: &ActionsWrite,
    prev: &[CreoAction],
) -> Result<Option<Vec<CreoAction>>> {
    let audience = crate::commands::auth::creo_audience();
    let Some(creds) =
        crate::commands::auth::credentials_refreshed_if_needed(&audience, REFRESH_SKEW_SECS)
            .await?
    else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .context("HTTP client の構築に失敗")?;
    let token = creds.access_token.as_str();
    let base = creo_base_url();

    let prev_by_id: std::collections::HashMap<&str, &CreoAction> =
        prev.iter().map(|a| (a.id.as_str(), a)).collect();

    let mut out = Vec::with_capacity(write.items.len());
    for item in &write.items {
        match plan_write(item, prev_by_id.get(item.id.as_str()).copied()) {
            // 上げない枝。**local のまま残す**ので、次に user が書いた時点で Create に移る。
            WritePlan::KeepLocal | WritePlan::Unchanged => out.push(item.clone()),
            WritePlan::Create => match create_action(&client, &base, token, item).await {
                Ok(created) => out.push(created),
                Err(e) => {
                    // local id のまま残す = cache に生き残り、次の機会に再試行される。
                    tracing::warn!("ACTIONS の作成に失敗（次の機会に再試行）: {e}");
                    out.push(item.clone());
                }
            },
            WritePlan::Update => {
                if let Err(e) = update_action(&client, &base, token, item).await {
                    tracing::warn!(id = %item.id, "ACTIONS の更新に失敗: {e}");
                }
                out.push(item.clone());
            }
        }
    }

    // 削除は**明示された id だけ**。local id は creo に無いので撃たない。
    for id in write.removed.iter().filter(|id| !is_local_id(id)) {
        if let Err(e) = delete_action(&client, &base, token, id).await {
            tracing::warn!(%id, "ACTIONS の削除に失敗: {e}");
        }
    }

    Ok(Some(out))
}

/// 新規 memory を作る。**POST は `status` を受け付けない**ので、要るなら PUT で立て直す 2 段。
async fn create_action(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    item: &CreoAction,
) -> Result<CreoAction> {
    let body = serde_json::json!({
        "content": item.text,
        // 表示のゲート。**これが無いと次の poll で消えたように見える**（読みは tag で絞る）。
        "tags": [ACTIONS_TAG],
        "metadata": vp_metadata(item),
    });
    let resp = client
        .post(format!("{base}/api/memories"))
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .context("creo への POST に失敗")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("creo POST /api/memories が {status} を返しました: {text}");
    }
    #[derive(serde::Deserialize)]
    struct CreateResponse {
        memory: CreoMemory,
    }
    let created: CreateResponse =
        serde_json::from_str(&text).context("creo POST の応答を parse できません")?;
    // **id 以外は手元の値が正**。creo の応答から metadata を取りこぼすと、以降の PUT が
    // 空の区画で本文を組んで「今書いた区画を消す + status を立てない」を同時にやる。
    // 採番された id を貰うだけ、と読めるよう 1 つの式に畳んである（下の PUT との順序を
    // 間違えようがない形にするため）。
    let action = adopt_local_intent(
        created
            .memory
            .into_action()
            .context("creo が id を返しませんでした")?,
        item,
    );
    // POST は `status` を受け付けないので、要るなら PUT で立て直す（doc 57 §4）。
    if let Some(want) = status_for(&action)
        && let Err(e) = update_action(client, base, token, &action).await
    {
        tracing::warn!(id = %action.id, "作成後の status 設定に失敗（{want} を諦める）: {e}");
    }
    Ok(action)
}

/// creo が採番した id に、**user が今持っている形**（本文 / 区画 / 並び / 完了）を載せる。
///
/// POST の応答が metadata を含むかは creo 側の都合なので、当てにしない。
fn adopt_local_intent(created: CreoAction, item: &CreoAction) -> CreoAction {
    CreoAction {
        id: created.id,
        text: item.text.clone(),
        done: item.done,
        bucket: item.bucket.clone(),
        order: item.order.clone(),
    }
}

/// 既存 memory を更新する。`PUT` 1 本で content / metadata / status が書ける。
///
/// ⚠️ `tags` は**送らない**。creo の update は tags を配列ごと置換するので、送ると user が
/// creo 側で付けた他の tag を消す（ゲートの付け外しは専用の atomic endpoint が別にある）。
async fn update_action(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    item: &CreoAction,
) -> Result<()> {
    let mut body = serde_json::json!({
        "content": item.text,
        "metadata": vp_metadata(item),
    });
    if let Some(status) = status_for(item) {
        body["status"] = serde_json::Value::String(status.to_string());
    }
    let resp = client
        .put(format!("{base}/api/memories/{}", item.id))
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .context("creo への PUT に失敗")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("creo PUT が {status} を返しました: {text}");
    }
    Ok(())
}

/// memory を消す（mako 裁定 2026-08-04: ✕ は **memory ごと消す**）。
///
/// ⚠️ **取り消せない**。呼ぶのは [`ActionsWrite::removed`] に明示された id だけで、
/// 一覧からの不在では決して呼ばない。404 は成功扱い（既に消えている = 望んだ状態）。
async fn delete_action(client: &reqwest::Client, base: &str, token: &str, id: &str) -> Result<()> {
    // ⚠️ **`?confirm=true` が必須**（creo の `deleteQuerySchema` が `z.literal('true')`）。
    // 無いと **400 が返るだけで 1 件も消えない** — 2026-08-04 に付けずに撃って実測した。
    // creo 自身が「削除は明示的な意図を要求する」設計なので、こちらもそれに従う。
    let resp = client
        .delete(format!("{base}/api/memories/{id}?confirm=true"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("creo への DELETE に失敗")?;
    let status = resp.status();
    if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!("creo DELETE が {status} を返しました: {text}");
}

/// poll 結果と cache を合わせる。
///
/// **creo にまだ上がっていない local id の Action は残す**。これが無いと、作成が失敗した
/// （あるいは書く前に poll が来た）捕捉が **30s 後の poll で無言で消える** — 差し込みを
/// 受け止めるための面が、受け止めたものを捨てることになる。
/// 書いた結果を cache に畳む。
///
/// ⚠️ **書いた結果で cache を置き換えてはいけない**。webview が送ってくる一覧は
/// 「その時点で webview が知っている全件」であって「creo にある全件」ではない。push が
/// 届く前（起動直後の最大 5s）に ⌘b で 1 件捕まえると、送られてくる `items` はその 1 件だけ —
/// 置き換えると既存の Action が次の poll まで sidebar から消える。
///
/// `removed`（消す意図）と同じ規律をここにも効かせる: **送られてこなかった id は触らない**。
fn merge_saved(
    saved: Vec<CreoAction>,
    write: &ActionsWrite,
    prev: &[CreoAction],
) -> Vec<CreoAction> {
    let sent: std::collections::HashSet<&str> = write.items.iter().map(|a| a.id.as_str()).collect();
    let removed: std::collections::HashSet<&str> =
        write.removed.iter().map(String::as_str).collect();
    let mut out = saved;
    // webview がまだ知らなかった（= 送ってこなかった）ものは cache に残す。
    // 消す意図が明示されたものだけは落とす。
    out.extend(
        prev.iter()
            .filter(|a| !sent.contains(a.id.as_str()) && !removed.contains(a.id.as_str()))
            .cloned(),
    );
    out
}

pub fn merge_fetched(fetched: Vec<CreoAction>, cached: &[CreoAction]) -> Vec<CreoAction> {
    let mut out = fetched;
    out.extend(cached.iter().filter(|a| is_local_id(&a.id)).cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(id: &str, bucket: &str) -> CreoAction {
        CreoAction {
            id: id.to_string(),
            text: "x".to_string(),
            done: false,
            bucket: bucket.to_string(),
            order: "a".to_string(),
        }
    }

    /// 契約の写像（doc 57 §3）を固定する — content / status / metadata.vp が
    /// そのまま Action の text / done / bucket / order に落ちること。
    #[test]
    fn maps_memory_to_action() {
        let body = r#"{
            "page": 1, "limit": 100, "total": 1,
            "memories": [{
                "id": "mem_abc",
                "content": "doc 56 設定画面\n\n- [ ] 永続先を決める",
                "status": "active",
                "tags": ["vp-actions"],
                "metadata": { "priority": "medium", "vp": { "bucket": "nexts", "order": "0|hzzzzz:" } }
            }]
        }"#;
        let items = parse_actions(body).expect("parse");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0],
            CreoAction {
                id: "mem_abc".to_string(),
                text: "doc 56 設定画面\n\n- [ ] 永続先を決める".to_string(),
                done: false,
                bucket: "nexts".to_string(),
                order: "0|hzzzzz:".to_string(),
            }
        );
    }

    /// `status: "done"` だけが done。**status 不在を done 扱いしない**
    /// （IDEAs / EVENTs は status を付けない設計なので、ここを取り違えると
    /// 思いつきが全部「完了」で出る）。
    #[test]
    fn done_is_only_status_done() {
        let body = r#"{"total":3,"memories":[
            {"id":"a","content":"x","status":"done"},
            {"id":"b","content":"y","status":"active"},
            {"id":"c","content":"z"}
        ]}"#;
        let items = parse_actions(body).expect("parse");
        assert_eq!(
            items.iter().map(|i| i.done).collect::<Vec<_>>(),
            vec![true, false, false]
        );
    }

    /// creo の UI から手で tag を付けた memory（`metadata.vp` が無い）も落とさずに通す。
    /// 区画は空のまま運び、webview の `normalizeActions` が TODOs 末尾へ丸める。
    #[test]
    fn hand_tagged_memory_survives_without_vp_metadata() {
        let body = r#"{"total":2,"memories":[
            {"id":"a","content":"手で引き取った","metadata":{"priority":"high"}},
            {"id":"b","content":"metadata ごと無い"}
        ]}"#;
        let items = parse_actions(body).expect("parse");
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|i| i.bucket.is_empty() && i.order.is_empty())
        );
    }

    /// `metadata.vp` の形が違う（文字列 / 型違い）でも表示ごと止めない。
    #[test]
    fn malformed_vp_metadata_falls_back_to_empty() {
        let body = r#"{"total":2,"memories":[
            {"id":"a","content":"x","metadata":{"vp":"actions"}},
            {"id":"b","content":"y","metadata":{"vp":{"bucket":42}}}
        ]}"#;
        let items = parse_actions(body).expect("parse");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.bucket.is_empty()));
    }

    /// id を持たない memory は捨てる（同一性が無いと更新も focus も追えない）。
    #[test]
    fn drops_memories_without_id() {
        let body = r#"{"total":2,"memories":[{"id":"","content":"x"},{"id":"ok","content":"y"}]}"#;
        let items = parse_actions(body).expect("parse");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "ok");
    }

    /// ⚠️ **内容が変わった時だけ rev が上がる**。ここが崩れると webview が 5s ごとに
    /// 同じ一覧を当て直し、編集中の行を書き戻して caret が飛ぶ。
    #[test]
    fn rev_advances_only_on_change() {
        let cache = CreoActionsCache::new();
        assert_eq!(cache.get().rev, 0, "未取得は 0");

        let one = vec![CreoAction {
            id: "a".into(),
            text: "x".into(),
            done: false,
            bucket: "nexts".into(),
            order: "a".into(),
        }];
        assert!(cache.set(one.clone()), "初回は変化");
        assert_eq!(cache.get().rev, 1);

        assert!(!cache.set(one.clone()), "同じ内容では動かない");
        assert_eq!(cache.get().rev, 1);

        let mut two = one.clone();
        two[0].text = "y".into();
        assert!(cache.set(two), "text の変化を拾う");
        assert_eq!(cache.get().rev, 2);

        // logout 相当 = 空へ。stale な Action を残さない。
        assert!(cache.set(Vec::new()));
        assert_eq!(cache.get().rev, 3);
        assert!(cache.get().items.is_empty());
    }

    /// 区画 → status の線引き（doc 57 §3）。**IDEAs / EVENTs には付けない** —
    /// 付けると mako の `list_todos` が思いつきで埋まる。
    #[test]
    fn status_mapping_follows_bucket() {
        assert_eq!(status_for(&act("m", "nexts")), Some("active"));
        assert_eq!(status_for(&act("m", "currents")), Some("active"));
        assert_eq!(status_for(&act("m", "waits")), Some("active"));
        assert_eq!(status_for(&act("m", "todos")), Some("active"));
        assert_eq!(status_for(&act("m", "ideas")), None);
        assert_eq!(status_for(&act("m", "events")), None);
        // 未知の区画も付けない（知らないものを task にしない）
        assert_eq!(status_for(&act("m", "")), None);
        // done は区画に関わらず done
        let mut d = act("m", "ideas");
        d.done = true;
        assert_eq!(status_for(&d), Some("done"));
    }

    /// ⚠️ **未知の id の形は「既存」に倒す**。逆に倒すと、同じ Action の memory が
    /// 書くたびに増える（PUT の 404 は log が出るだけで済む）。
    #[test]
    fn only_vp_minted_ids_count_as_local() {
        assert!(is_local_id("act-018f-abcd"));
        assert!(!is_local_id("mem_1CdcG8k8LM4Ye18meuQPw9"));
        assert!(!is_local_id("なにかの知らない形"));
        assert!(!is_local_id(""));
    }

    /// `metadata.vp` は creo 側で**丸ごと置き換わる**。Phase 5 で `lane` を足す時に
    /// ここへ載せ忘れると書くたびに消えるので、形を test で見えるようにしておく。
    #[test]
    fn vp_metadata_carries_bucket_and_order() {
        let meta = vp_metadata(&act("m", "waits"));
        assert_eq!(meta["vp"]["bucket"], "waits");
        assert_eq!(meta["vp"]["order"], "a");
        // top-level は creo 側が shallow merge するので、こちらは vp だけ送る。
        assert_eq!(meta.as_object().map(|o| o.len()), Some(1));
    }

    /// ⚠️ **creo にまだ上がっていない捕捉を poll が消さない**。これが崩れると、
    /// 作成に失敗した（あるいは書く前に poll が来た）Action が 30s 後に無言で消える。
    #[test]
    fn merge_keeps_unsaved_local_items() {
        let cached = vec![act("mem_1", "nexts"), act("act-new", "ideas")];
        let fetched = vec![act("mem_1", "nexts")];
        let merged = merge_fetched(fetched, &cached);
        assert_eq!(
            merged.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["mem_1", "act-new"]
        );
    }

    /// ⚠️ **「creo に上げない」は「消す」ではない**。
    ///
    /// `⌘ hard b` → 数字は空の行を 1 本足して focus する動線。この空行を `out` から
    /// 落としていたため cache ごと消え、その snapshot が webview に返って
    /// **行が画面から消滅**した（2026-08-07 の実害）。`KeepLocal` は「POST しない」だけで、
    /// 呼び手は必ず 1 件 `out` に残す。
    #[test]
    fn empty_new_row_is_kept_local_not_dropped() {
        let empty = CreoAction {
            id: "act-new".into(),
            text: "   ".into(), // 空白だけも「未記入」
            done: false,
            bucket: "nexts".into(),
            order: "0|h:".into(),
        };
        assert_eq!(plan_write(&empty, None), WritePlan::KeepLocal);

        // 書き始めたら creo に上がる。
        let typed = CreoAction {
            text: "捕まえた".into(),
            ..empty.clone()
        };
        assert_eq!(plan_write(&typed, None), WritePlan::Create);
    }

    /// 既存行は「変わっていなければ撃たない」。5s ごとの push で無駄な PUT を出さないため。
    #[test]
    fn unchanged_remote_item_is_not_written() {
        let item = CreoAction {
            id: "mem_1".into(),
            text: "既存".into(),
            done: false,
            bucket: "todos".into(),
            order: "0|h:".into(),
        };
        assert_eq!(plan_write(&item, Some(&item)), WritePlan::Unchanged);

        let edited = CreoAction {
            text: "書き換えた".into(),
            ..item.clone()
        };
        assert_eq!(plan_write(&edited, Some(&item)), WritePlan::Update);
        // 手元に prev が無い（daemon 再起動直後など）なら、撃って合わせに行く。
        assert_eq!(plan_write(&item, None), WritePlan::Update);
    }

    /// ⚠️ **creo の応答が metadata を落としても、区画 / 並びは手元の値が残る**。
    /// ここが崩れると、作成直後の status PUT が空の区画で本文を組み
    /// 「今書いた区画を消す + status を立てない」を同時にやる（2026-08-04 に実際に書いた形）。
    #[test]
    fn created_action_keeps_local_intent() {
        let item = CreoAction {
            id: "act-1".into(),
            text: "捕まえた".into(),
            done: false,
            bucket: "nexts".into(),
            order: "0|h:".into(),
        };
        // creo の応答は id だけ（metadata を含まない worst case）。
        let created = CreoAction {
            id: "mem_new".into(),
            text: String::new(),
            done: false,
            bucket: String::new(),
            order: String::new(),
        };
        let adopted = adopt_local_intent(created, &item);
        assert_eq!(adopted.id, "mem_new", "id だけ creo のものを採る");
        assert_eq!(adopted.bucket, "nexts");
        assert_eq!(adopted.order, "0|h:");
        assert_eq!(adopted.text, "捕まえた");
        // 区画が残っているので status も正しく立つ。
        assert_eq!(status_for(&adopted), Some("active"));
    }

    /// ⚠️ **起動直後の「短い一覧」で cache を潰さない**。push が届く前に ⌘b で 1 件捕まえると
    /// webview は 1 件だけを送ってくる。置き換えると既存の Action が次の poll まで消える。
    #[test]
    fn merge_saved_keeps_items_the_webview_did_not_know_about() {
        let prev = vec![act("mem_1", "nexts"), act("mem_2", "nexts")];
        let write = ActionsWrite {
            items: vec![act("act-fresh", "ideas")],
            removed: Vec::new(),
        };
        let merged = merge_saved(vec![act("mem_new", "ideas")], &write, &prev);
        assert_eq!(
            merged.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["mem_new", "mem_1", "mem_2"],
            "書いた分 + webview がまだ知らなかった分"
        );
    }

    /// 明示された削除だけは cache からも落ちる（消したのに残って見えない）。
    #[test]
    fn merge_saved_drops_only_explicit_removals() {
        let prev = vec![act("mem_1", "nexts"), act("mem_2", "nexts")];
        let write = ActionsWrite {
            items: vec![act("mem_1", "nexts")],
            removed: vec!["mem_2".to_string()],
        };
        let merged = merge_saved(vec![act("mem_1", "nexts")], &write, &prev);
        assert_eq!(
            merged.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["mem_1"]
        );
    }

    /// creo で消された memory は poll で消える（local id だけが生き残る特別扱い）。
    #[test]
    fn merge_drops_remote_items_that_disappeared() {
        let cached = vec![act("mem_1", "nexts"), act("mem_2", "nexts")];
        let merged = merge_fetched(vec![act("mem_1", "nexts")], &cached);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "mem_1");
    }

    /// base URL は env で上書きでき、末尾の `/` は落ちる（URL 組み立てで `//` を作らない）。
    #[test]
    fn base_url_env_override_trims_slash() {
        // ⚠️ env は process global。この test は他と共有 var を触らないので単独で足りる。
        unsafe {
            std::env::remove_var("VP_CREO_URL");
        }
        assert_eq!(creo_base_url(), DEFAULT_CREO_URL);
        unsafe {
            std::env::set_var("VP_CREO_URL", "http://127.0.0.1:8787/");
        }
        assert_eq!(creo_base_url(), "http://127.0.0.1:8787");
        unsafe {
            std::env::remove_var("VP_CREO_URL");
        }
    }
}
