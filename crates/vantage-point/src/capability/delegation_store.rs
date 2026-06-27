//! 委譲 (delegation) の World 中央 store — SurrealDB backing（doc 28 §4 / §6）。
//!
//! wire と同じく TheWorld の DB（`delegations` table）に delegation record を持つ。SP の
//! `handle_delegate` / `handle_complete` / `handle_respond` は `world_wire::call("/api/delegation/*")`
//! でここに proxy する（SP 再起動を跨いで生存 = durable、World reconcile loop の駆動源）。
//!
//! 状態遷移ロジック（どの Outcome がどの state か）は本 store が持つ（single source of truth）。
//! wake（誰をどの prompt で起こすか）は SP 側（`process/delegation.rs`）の責務 — store は
//! data + transition のみ、wake は actions として分離する。

use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::process::delegation::{Delegation, DelegationState, Outcome};

/// 現在時刻（ms）。created_at / updated_at に使う（reconcile の timeout 判定の基準）。
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// SurrealDB record id（`delegations:<local>` 形式 or object）から local 部分を抽出する。
/// `wiremsg_store::extract_record_local_id` と同型（`CONTENT { id: $id }` で bare uuid を
/// 固定しているが、SELECT * の返却 id は record id 形式になりうるため剥がす）。
fn extract_record_local_id(id_value: &serde_json::Value, table: &str) -> String {
    if let Some(s) = id_value.as_str() {
        let prefix = format!("{table}:");
        return s
            .strip_prefix(&prefix)
            .unwrap_or(s)
            .trim_matches('`')
            .to_string();
    }
    id_value["id"]
        .as_str()
        .or_else(|| id_value["String"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// 委譲の World 中央 store（`Arc<Surreal>` 1 本なので Clone は cheap = reconcile loop に渡せる）。
#[derive(Clone)]
pub(crate) struct DelegationStore {
    db: Arc<Surreal<Any>>,
}

impl DelegationStore {
    /// 既存 `Surreal<Any>` connection から store を構築。
    pub(crate) fn new(db: Arc<Surreal<Any>>) -> Self {
        Self { db }
    }

    /// delegate: 新規 record を作成（state=active、delivered=false）。id は採番済を受ける。
    ///
    /// Pending は wake 直前の transient（SP がすぐ wake する）なので永続せず active で起こす。
    pub(crate) async fn create(
        &self,
        id: &str,
        requester: &str,
        doer: &str,
        task: &str,
    ) -> Result<Delegation> {
        let now = now_ms();
        self.db
            .query(
                r#"
                CREATE type::record('delegations', $id) CONTENT {
                    id: $id, requester: $requester, doer: $doer, task: $task,
                    state: 'active', outcome: NONE,
                    created_at: $now, updated_at: $now, delivered: false
                }
                "#,
            )
            .bind(("id", id.to_string()))
            .bind(("requester", requester.to_string()))
            .bind(("doer", doer.to_string()))
            .bind(("task", task.to_string()))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("delegation create failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("delegation create check failed: {e}"))?;
        self.get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("delegation create: not found after insert"))
    }

    /// id 指定で 1 件取得（未知 id は None）。
    pub(crate) async fn get(&self, id: &str) -> Result<Option<Delegation>> {
        let mut res = self
            .db
            .query("SELECT * FROM type::record('delegations', $id);")
            .bind(("id", id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("delegation get failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("delegation get take failed: {e}"))?;
        match rows.first() {
            Some(row) => Ok(Some(Self::row_to_delegation(row)?)),
            None => Ok(None),
        }
    }

    /// complete: Outcome に応じて state を done / failed / awaiting_response に遷移。
    /// 戻り値 = 更新後 record（未知 id は None = caller が Err にする）。
    pub(crate) async fn apply_complete(
        &self,
        id: &str,
        outcome: &Outcome,
    ) -> Result<Option<Delegation>> {
        let new_state = match outcome {
            Outcome::Done { .. } => "done",
            Outcome::Failed { .. } => "failed",
            // NeedsInput は非終端: doer が await に入り requester の respond を待つ。
            Outcome::NeedsInput { .. } => "awaiting_response",
        };
        let outcome_val = serde_json::to_value(outcome)
            .map_err(|e| anyhow::anyhow!("delegation outcome serialize: {e}"))?;
        // 存在チェック: SurrealDB の `UPDATE type::record(id)` は未知 id を upsert (create) する。
        // 委譲は「未知 id は Err」が契約なので、存在しなければ UPDATE せず None を返す。
        if self.get(id).await?.is_none() {
            return Ok(None);
        }
        self.db
            .query(
                r#"
                UPDATE type::record('delegations', $id) SET
                    state = $state, outcome = $outcome, updated_at = $now, delivered = false;
                "#,
            )
            .bind(("id", id.to_string()))
            .bind(("state", new_state.to_string()))
            .bind(("outcome", outcome_val))
            .bind(("now", now_ms()))
            .await
            .map_err(|e| anyhow::anyhow!("delegation apply_complete failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("delegation apply_complete check failed: {e}"))?;
        self.get(id).await
    }

    /// respond: NeedsInput を回答で消費し Active に戻す（answer は transient = SP の wake 専用）。
    /// 戻り値 = 更新後 record（未知 id は None）。
    pub(crate) async fn apply_respond(&self, id: &str) -> Result<Option<Delegation>> {
        // 存在チェック（UPDATE の upsert 回避、apply_complete と同じ理由）。
        if self.get(id).await?.is_none() {
            return Ok(None);
        }
        self.db
            .query(
                r#"
                UPDATE type::record('delegations', $id) SET
                    state = 'active', outcome = NONE, updated_at = $now, delivered = false;
                "#,
            )
            .bind(("id", id.to_string()))
            .bind(("now", now_ms()))
            .await
            .map_err(|e| anyhow::anyhow!("delegation apply_respond failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("delegation apply_respond check failed: {e}"))?;
        self.get(id).await
    }

    /// 直近 wake が target に届いたか（push の woke 結果）を記録する。
    /// follow-up B（reconcile）が delivered=false を再 nudge / C（pull-hook）が poll する基準。
    pub(crate) async fn mark_delivered(&self, id: &str, delivered: bool) -> Result<()> {
        self.db
            .query(
                "UPDATE type::record('delegations', $id) SET delivered = $delivered, updated_at = $now;",
            )
            .bind(("id", id.to_string()))
            .bind(("delivered", delivered))
            .bind(("now", now_ms()))
            .await
            .map_err(|e| anyhow::anyhow!("delegation mark_delivered failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("delegation mark_delivered check failed: {e}"))?;
        Ok(())
    }

    /// reconcile: 直近 wake が未配達（`delivered = false`）の record を全て引く。
    /// World reconcile loop が再 nudge する候補（push 取りこぼし / timeout 直後の Failed 等）。
    pub(crate) async fn list_undelivered(&self) -> Result<Vec<Delegation>> {
        let mut res = self
            .db
            .query("SELECT * FROM delegations WHERE delivered = false;")
            .await
            .map_err(|e| anyhow::anyhow!("delegation list_undelivered failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("delegation list_undelivered take failed: {e}"))?;
        rows.iter().map(Self::row_to_delegation).collect()
    }

    /// reconcile: `updated_at` が `cutoff_ms` より古い未終了（active / awaiting_response）record を引く。
    /// = doer 沈黙 / requester stall の timeout 候補（→ `fail_timeout` で Failed{timeout} に落とす）。
    pub(crate) async fn list_stale_open(&self, cutoff_ms: i64) -> Result<Vec<Delegation>> {
        let mut res = self
            .db
            .query(
                "SELECT * FROM delegations \
                 WHERE (state = 'active' OR state = 'awaiting_response') AND updated_at < $cutoff;",
            )
            .bind(("cutoff", cutoff_ms))
            .await
            .map_err(|e| anyhow::anyhow!("delegation list_stale_open failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("delegation list_stale_open take failed: {e}"))?;
        rows.iter().map(Self::row_to_delegation).collect()
    }

    /// reconcile: doer 沈黙 / 死を `Failed{timeout}` に落とす（delivered=false で requester wake 待ち）。
    /// 戻り値 = 更新後 record（未知 id は None）。
    pub(crate) async fn fail_timeout(&self, id: &str) -> Result<Option<Delegation>> {
        if self.get(id).await?.is_none() {
            return Ok(None);
        }
        // state ガード付き UPDATE: list_stale_open 判定〜ここまでの間に doer が complete して
        // done/failed に遷移していたら no-op（正常完了を timeout で上書きしない = 冪等）。
        self.db
            .query(
                r#"
                UPDATE type::record('delegations', $id) SET
                    state = 'failed',
                    outcome = { kind: 'failed', reason: 'timeout' },
                    updated_at = $now, delivered = false
                WHERE state = 'active' OR state = 'awaiting_response';
                "#,
            )
            .bind(("id", id.to_string()))
            .bind(("now", now_ms()))
            .await
            .map_err(|e| anyhow::anyhow!("delegation fail_timeout failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("delegation fail_timeout check failed: {e}"))?;
        self.get(id).await
    }

    /// pull-hook（C）: agent が関与（requester or doer）し、まだ配達されていない（delivered=false）
    /// 委譲を引く。ターン頭で「解決済だが push 取りこぼした委譲」を agent に surface する基準。
    /// requester 視点（done/failed/awaiting_response の Outcome）か doer 視点（active の task）かは
    /// caller（hook formatter）が role × state で判定する。
    pub(crate) async fn poll_for_agent(&self, agent: &str) -> Result<Vec<Delegation>> {
        let mut res = self
            .db
            .query(
                "SELECT * FROM delegations \
                 WHERE delivered = false AND (requester = $agent OR doer = $agent);",
            )
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("delegation poll_for_agent failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("delegation poll_for_agent take failed: {e}"))?;
        rows.iter().map(Self::row_to_delegation).collect()
    }

    /// `delegations` row JSON を [`Delegation`] に hydrate。
    fn row_to_delegation(row: &serde_json::Value) -> Result<Delegation> {
        let id = extract_record_local_id(&row["id"], "delegations");
        let state: DelegationState = serde_json::from_value(row["state"].clone())
            .map_err(|e| anyhow::anyhow!("delegation row state parse ({}): {e}", row["state"]))?;
        // outcome は NONE（null）or object。parse 失敗は None に倒す（防御的）。
        let outcome: Option<Outcome> = if row["outcome"].is_null() {
            None
        } else {
            serde_json::from_value(row["outcome"].clone()).ok()
        };
        Ok(Delegation {
            id,
            requester: row["requester"].as_str().unwrap_or_default().to_string(),
            doer: row["doer"].as_str().unwrap_or_default().to_string(),
            task: row["task"].as_str().unwrap_or_default().to_string(),
            state,
            outcome,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::VpDb;

    async fn make_store() -> DelegationStore {
        let db = VpDb::connect_mem().await.unwrap();
        // SCHEMAFULL の `delegations` table を定義（本番と同じ挙動 = 未知 id の SELECT は空）。
        db.define_schema().await.unwrap();
        DelegationStore::new(Arc::new(db.inner().clone()))
    }

    #[tokio::test]
    async fn create_then_get_roundtrip() {
        let store = make_store().await;
        let d = store
            .create("dlg-1", "agent@vp", "agent@vp/w1", "DB schema を書く")
            .await
            .unwrap();
        assert_eq!(d.id, "dlg-1");
        assert_eq!(d.requester, "agent@vp");
        assert_eq!(d.doer, "agent@vp/w1");
        assert_eq!(d.state, DelegationState::Active);
        assert!(d.outcome.is_none());

        let got = store.get("dlg-1").await.unwrap().unwrap();
        assert_eq!(got.task, "DB schema を書く");
        assert_eq!(got.state, DelegationState::Active);
    }

    #[tokio::test]
    async fn complete_done_and_failed_and_needsinput_transitions() {
        let store = make_store().await;
        store
            .create("dlg-d", "agent@vp", "agent@vp/w1", "t")
            .await
            .unwrap();
        let done = store
            .apply_complete(
                "dlg-d",
                &Outcome::Done {
                    result: "ok".into(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.state, DelegationState::Done);
        assert!(matches!(done.outcome, Some(Outcome::Done { result }) if result == "ok"));

        // NeedsInput → awaiting_response、 respond → active で outcome 消費。
        store
            .apply_complete(
                "dlg-d",
                &Outcome::NeedsInput {
                    question: "q?".into(),
                },
            )
            .await
            .unwrap();
        let ni = store.get("dlg-d").await.unwrap().unwrap();
        assert_eq!(ni.state, DelegationState::AwaitingResponse);
        let back = store.apply_respond("dlg-d").await.unwrap().unwrap();
        assert_eq!(back.state, DelegationState::Active);
        assert!(back.outcome.is_none());
    }

    #[tokio::test]
    async fn unknown_id_complete_returns_none() {
        let store = make_store().await;
        let res = store
            .apply_complete("dlg-none", &Outcome::Failed { reason: "x".into() })
            .await
            .unwrap();
        assert!(res.is_none(), "未知 id は None（UPDATE no-op）");
    }

    #[tokio::test]
    async fn reconcile_queries_and_fail_timeout() {
        let store = make_store().await;
        // 2 件作成（どちらも active = delivered false）。
        store
            .create("dlg-a", "agent@vp", "agent@vp/w", "ta")
            .await
            .unwrap();
        store
            .create("dlg-b", "agent@vp", "agent@vp/w", "tb")
            .await
            .unwrap();

        // create 直後は delivered=false → list_undelivered に両方。
        let undeliv = store.list_undelivered().await.unwrap();
        assert_eq!(undeliv.len(), 2);
        // mark_delivered で 1 件配達済 → list_undelivered は 1 件に。
        store.mark_delivered("dlg-a", true).await.unwrap();
        let undeliv = store.list_undelivered().await.unwrap();
        assert_eq!(undeliv.len(), 1);
        assert_eq!(undeliv[0].id, "dlg-b");

        // 未来 cutoff で list_stale_open → active な 2 件が stale 候補。
        let now = chrono::Utc::now().timestamp_millis();
        let stale = store.list_stale_open(now + 10_000).await.unwrap();
        assert_eq!(stale.len(), 2, "active な未終了が stale 候補");

        // fail_timeout → Failed{timeout}、 delivered=false に戻る。
        let failed = store.fail_timeout("dlg-a").await.unwrap().unwrap();
        assert_eq!(failed.state, DelegationState::Failed);
        assert!(matches!(failed.outcome, Some(Outcome::Failed { reason }) if reason == "timeout"));
        // timeout 後は再 wake 待ち（delivered=false）→ undelivered に再登場。
        let undeliv = store.list_undelivered().await.unwrap();
        assert!(undeliv.iter().any(|d| d.id == "dlg-a"));
        // 終端化した dlg-a は stale_open（active/awaiting_response）からは外れる。
        let stale = store.list_stale_open(now + 10_000).await.unwrap();
        assert!(stale.iter().all(|d| d.id != "dlg-a"));

        // 未知 id の fail_timeout は None。
        assert!(store.fail_timeout("dlg-none").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn poll_for_agent_returns_involved_undelivered() {
        let store = make_store().await;
        // requester=agent@vp / doer=agent@vp/w の委譲。
        store
            .create("dlg-1", "agent@vp", "agent@vp/w", "t")
            .await
            .unwrap();
        // 無関係 agent の委譲。
        store
            .create("dlg-2", "agent@other", "agent@x/w", "t")
            .await
            .unwrap();

        // requester として poll → dlg-1 が返る。
        let mine = store.poll_for_agent("agent@vp").await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, "dlg-1");
        // doer として poll → dlg-1 が返る。
        let as_doer = store.poll_for_agent("agent@vp/w").await.unwrap();
        assert_eq!(as_doer.len(), 1);
        // 配達済になると poll から外れる。
        store.mark_delivered("dlg-1", true).await.unwrap();
        assert!(store.poll_for_agent("agent@vp").await.unwrap().is_empty());
        // 無関係 agent は空。
        assert!(
            store
                .poll_for_agent("agent@nobody")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
