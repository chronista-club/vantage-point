//! service（各機能）の状態記録（`service_status` table）。
//!
//! `/api/health` が算出した `services` を DB にも書き残す先（VP-21）。repo 固有なので
//! `repo_path` 列で scope を切る。
//!
//! ⚠️ **`repo/http/health.rs` は読み手ではなく書き手**。`services` は live な in-memory state
//! から算出していて、この table は関与しない（算出結果を後から写しているだけ）。
//! [`VpDb::list_service_status`] は現状**呼び手が 1 つも無い**（doc 62 §6 の cut 候補）。
//!
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // =========================================================================
    // Agent Status CRUD
    // =========================================================================

    /// Agent ステータスを更新（UPSERT）
    pub async fn upsert_service_status(
        &self,
        repo_path: &str,
        agent_key: &str,
        status: &str,
        detail: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO service_status {
                    repo_path: $repo_path,
                    agent_key: $agent_key,
                    status: $status,
                    detail: $detail,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    status = $input.status,
                    detail = $input.detail,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("agent_key", agent_key.to_string()))
            .bind(("status", status.to_string()))
            .bind(("detail", detail.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("service_status upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("service_status upsert エラー: {}", e))?;
        Ok(())
    }

    /// repoの全 Agent ステータスを取得
    pub async fn list_service_status(&self, repo_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM service_status WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("service_status 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::make_test_db;

    // =========================================================================
    // Agent Status CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_stand_status_basic_crud() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["agent_key"], "heaven-door");
        assert_eq!(statuses[0]["status"], "running");
    }

    /// 同一 (repo_path, agent_key) で再度 upsert → status が更新される
    #[tokio::test]
    async fn test_stand_status_upsert_updates_status() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        db.upsert_service_status("/repos/vp", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["status"], "stopped");
    }

    /// detail が None → NULL で保存できる
    #[tokio::test]
    async fn test_stand_status_detail_null() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "board", "idle", None)
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(
            statuses[0]["detail"].is_null(),
            "detail が NULL でない: {:?}",
            statuses[0]["detail"]
        );
    }

    /// detail に JSON オブジェクト → 保存・復元できる
    #[tokio::test]
    async fn test_stand_status_detail_with_json() {
        let db = make_test_db().await;

        let detail = serde_json::json!({
            "canvas_open": true,
            "pane_count": 3
        });

        db.upsert_service_status("/repos/vp", "board", "running", Some(&detail))
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["detail"]["canvas_open"], true);
        assert_eq!(statuses[0]["detail"]["pane_count"], 3);
    }

    /// 異なる repo_path の service_status は分離される
    #[tokio::test]
    async fn test_stand_status_repo_isolation() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();
        db.upsert_service_status("/repos/creo", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let vp_statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(vp_statuses.len(), 1);
        assert_eq!(vp_statuses[0]["status"], "running");

        let creo_statuses = db.list_service_status("/repos/creo").await.unwrap();
        assert_eq!(creo_statuses.len(), 1);
        assert_eq!(creo_statuses[0]["status"], "stopped");
    }
}
