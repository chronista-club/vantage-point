//! repo process の永続層（`processes` table）と、その LIVE SELECT。
//!
//! daemon 内の running registry（`capability/repo_manager_capability.rs`）が単一の真実源で、
//! この table はその写し。書き手は `start_process` / `stop_process` の 2 箇所だけ。
//!
//! ⚠️ 「repo の QUIC 自己登録が書く」は **doc 44 P1 fold-in で失効した**帰属。repo が
//! プロセスでなくなり自己登録しに来る者が居なくなったので、書き手は daemon-canonical に
//! 戻っている（registry handler は #824 で撤去済み）。
//!
//! [`VpDb::clear_all_processes`] は daemon 再起動時のクリーンアップ**用**に在るが、
//! **現状の呼び手は test だけ**（doc 62 §6 の cut 候補）。
//!
//! [`VpDb::live_processes`] だけ性質が違って、SurrealDB の LIVE SELECT stream を返す。
//! 消費側（`repo/server.rs`）は `crate::db::Action` で差分の種類を見る。この re-export は
//! `db/mod.rs` に置いてあり、downstream が surrealdb に直接依存しなくて済むようにしている。
//!
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // =========================================================================
    // Processes CRUD
    // =========================================================================

    /// 稼働中プロセスを登録（UPSERT）
    pub async fn upsert_process(
        &self,
        repo_path: &str,
        repo_name: &str,
        port: u16,
        pid: u32,
        status: &str,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO processes {
                    repo_path: $repo_path,
                    repo_name: $repo_name,
                    port: $port,
                    pid: $pid,
                    status: $status,
                    started_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    repo_name = $input.repo_name,
                    port = $input.port,
                    pid = $input.pid,
                    status = $input.status",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("repo_name", repo_name.to_string()))
            .bind(("port", port as i64))
            .bind(("pid", pid as i64))
            .bind(("status", status.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("process upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process upsert エラー: {}", e))?;
        Ok(())
    }

    /// プロセスを登録解除（repo_path で特定）
    pub async fn delete_process(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM processes WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("process 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process 削除エラー: {}", e))?;
        Ok(())
    }

    /// 稼働中プロセス一覧を取得
    pub async fn list_processes(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM processes")
            .await
            .map_err(|e| anyhow::anyhow!("processes 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// 全プロセスを削除（daemon 再起動時のクリーンアップ用）
    pub async fn clear_all_processes(&self) -> Result<()> {
        self.db
            .query("DELETE FROM processes")
            .await
            .map_err(|e| anyhow::anyhow!("processes クリア失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("processes クリアエラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // LIVE SELECT（リアルタイム変更通知）
    // =========================================================================

    /// processes テーブルの LIVE SELECT を開始
    ///
    /// INSERT/UPDATE/DELETE のたびに `Notification<serde_json::Value>` を返すストリーム。
    /// daemon が購読して DistributedNotification に変換する。
    ///
    /// 返り値は `'static` ライフタイム（`Surreal<Any>` は内部 Arc なので clone が軽量）。
    pub async fn live_processes(
        &self,
    ) -> Result<surrealdb::method::Stream<Vec<serde_json::Value>>> {
        let stream = self
            .db
            .select("processes")
            .live()
            .await
            .map_err(|e| anyhow::anyhow!("LIVE SELECT processes 失敗: {}", e))?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::make_test_db;

    // =========================================================================
    // Processes CRUD テスト
    // =========================================================================

    #[tokio::test]
    async fn test_processes_crud() {
        let db = make_test_db().await;

        // 登録
        db.upsert_process("/repos/vp", "vp", 33000, 1234, "running")
            .await
            .unwrap();

        // 一覧
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["repo_name"], "vp");
        assert_eq!(procs[0]["port"], 33000);

        // 更新（同じ path で upsert）
        db.upsert_process("/repos/vp", "vp", 33001, 5678, "running")
            .await
            .unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["port"], 33001);

        // 削除
        db.delete_process("/repos/vp").await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    #[tokio::test]
    async fn test_processes_clear_all() {
        let db = make_test_db().await;

        db.upsert_process("/a", "a", 33000, 1, "running")
            .await
            .unwrap();
        db.upsert_process("/b", "b", 33001, 2, "running")
            .await
            .unwrap();

        db.clear_all_processes().await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    // =========================================================================
    // Processes エッジケーステスト
    // =========================================================================

    /// 存在しない repo_path を delete_process してもエラーにならない
    #[tokio::test]
    async fn test_processes_delete_nonexistent() {
        let db = make_test_db().await;

        // 何も INSERT せずに DELETE → エラーなし（空操作）
        db.delete_process("/repos/nonexistent")
            .await
            .expect("存在しないレコードの削除はエラーにならない");

        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    // =========================================================================
    // LIVE SELECT テスト
    // =========================================================================

    /// kv-mem で live_processes を開始してストリームが取得できる（接続確認）
    #[tokio::test]
    async fn test_live_processes_stream_connects() {
        let db = make_test_db().await;

        // ストリーム開始がエラーにならないことを確認
        let _stream = db
            .live_processes()
            .await
            .expect("live_processes ストリームの開始が失敗してはいけない");
    }
}
