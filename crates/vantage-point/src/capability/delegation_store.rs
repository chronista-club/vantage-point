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

/// 委譲の World 中央 store。
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
}
