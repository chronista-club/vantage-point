//! DB Capability - ローカル永続化
//!
//! SQLite（sqlx）によるローカルデータベース。
//\! Process 履歴・設定・汎用 KV ストアの3機能を提供する。
//!
//! DB ファイル: `~/.config/vp/vantage.db`

use crate::capability::core::{
    Capability, CapabilityContext, CapabilityError, CapabilityInfo, CapabilityResult,
    CapabilityState,
};
use crate::capability::params::{CapabilityParams, Rank};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::any::Any;
use std::path::PathBuf;
use std::str::FromStr;

// =============================================================================
// 型定義
// =============================================================================

/// Process イベントレコード
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct StandEvent {
    pub id: i64,
    pub port: i64,
    pub event_type: String,
    pub project: Option<String>,
    pub details: Option<String>,
    pub created_at: String,
}

/// Process 履歴クエリフィルター
#[derive(Debug, Default)]
pub struct StandEventFilter {
    /// ポート番号で絞り込み
    pub port: Option<i64>,
    /// イベントタイプで絞り込み
    pub event_type: Option<String>,
    /// 最大件数（デフォルト: 100）
    pub limit: Option<i64>,
}

// =============================================================================
// DbCapability
// =============================================================================

/// DB Capability - ローカル永続化
pub struct DbCapability {
    state: CapabilityState,
    pool: Option<SqlitePool>,
    db_path: PathBuf,
}

impl DbCapability {
    /// 新しい DbCapability を作成
    ///
    /// DB パスは `~/.config/vp/vantage.db` を使用
    pub fn new() -> Self {
        let db_path = crate::config::config_dir().join("vantage.db");

        Self {
            state: CapabilityState::Uninitialized,
            pool: None,
            db_path,
        }
    }

    /// DB パスを指定して作成（テスト用）
    #[cfg(test)]
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            pool: None,
            db_path: path,
        }
    }

    /// SqlitePool への参照を取得
    fn pool(&self) -> CapabilityResult<&SqlitePool> {
        self.pool.as_ref().ok_or(CapabilityError::NotInitialized)
    }

    // =========================================================================
    // マイグレーション
    // =========================================================================

    /// SQL ファイルからスキーマを適用
    async fn run_migrations(&self) -> CapabilityResult<()> {
        let pool = self.pool()?;
        let schema = include_str!("../../migrations/20260220_init.sql");

        sqlx::raw_sql(schema)
            .execute(pool)
            .await
            .map_err(|e| CapabilityError::InitializationFailed(format!("migration failed: {e}")))?;

        tracing::info!("DB migration completed");
        Ok(())
    }

    // =========================================================================
    // Events API
    // =========================================================================

    /// Process イベントを記録
    pub async fn record_stand_event(
        &self,
        port: i64,
        event_type: &str,
        project: Option<&str>,
        details: Option<&str>,
    ) -> CapabilityResult<i64> {
        let pool = self.pool()?;

        let result = sqlx::query(
            "INSERT INTO stand_events (port, event_type, project, details) VALUES (?, ?, ?, ?)",
        )
        .bind(port)
        .bind(event_type)
        .bind(project)
        .bind(details)
        .execute(pool)
        .await
        .map_err(|e| CapabilityError::ResourceError(format!("record event: {e}")))?;

        Ok(result.last_insert_rowid())
    }

    /// Process 履歴をクエリ
    pub async fn query_stand_history(
        &self,
        filter: StandEventFilter,
    ) -> CapabilityResult<Vec<StandEvent>> {
        let pool = self.pool()?;
        let limit = filter.limit.unwrap_or(100);

        // フィルター条件を動的に構築（ランタイムクエリ）
        let mut sql = String::from(
            "SELECT id, port, event_type, project, details, created_at FROM stand_events",
        );
        let mut conditions = Vec::new();

        if filter.port.is_some() {
            conditions.push("port = ?");
        }
        if filter.event_type.is_some() {
            conditions.push("event_type = ?");
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");

        let mut query = sqlx::query_as::<_, StandEvent>(&sql);
        if let Some(port) = filter.port {
            query = query.bind(port);
        }
        if let Some(ref event_type) = filter.event_type {
            query = query.bind(event_type.clone());
        }
        query = query.bind(limit);

        let rows = query
            .fetch_all(pool)
            .await
            .map_err(|e| CapabilityError::ResourceError(format!("query history: {e}")))?;

        Ok(rows)
    }

    // =========================================================================
    // Settings API
    // =========================================================================

    /// グローバル設定を取得
    pub async fn get_setting(&self, key: &str) -> CapabilityResult<Option<String>> {
        self.get_project_setting("", key).await
    }

    /// グローバル設定を保存（UPSERT）
    pub async fn set_setting(&self, key: &str, value: &str) -> CapabilityResult<()> {
        self.set_project_setting("", key, value).await
    }

    /// プロジェクト固有設定を取得
    pub async fn get_project_setting(
        &self,
        project: &str,
        key: &str,
    ) -> CapabilityResult<Option<String>> {
        let pool = self.pool()?;

        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM settings WHERE project = ? AND key = ?")
                .bind(project)
                .bind(key)
                .fetch_optional(pool)
                .await
                .map_err(|e| CapabilityError::ResourceError(format!("get setting: {e}")))?;

        Ok(row.map(|r| r.0))
    }

    /// プロジェクト固有設定を保存（UPSERT）
    pub async fn set_project_setting(
        &self,
        project: &str,
        key: &str,
        value: &str,
    ) -> CapabilityResult<()> {
        let pool = self.pool()?;

        sqlx::query(
            "INSERT INTO settings (project, key, value) VALUES (?, ?, ?)
             ON CONFLICT(project, key) DO UPDATE SET value = excluded.value,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(project)
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .map_err(|e| CapabilityError::ResourceError(format!("set setting: {e}")))?;

        Ok(())
    }

    // =========================================================================
    // KV Store API
    // =========================================================================

    /// KV ストアから値を取得
    pub async fn kv_get(&self, namespace: &str, key: &str) -> CapabilityResult<Option<String>> {
        let pool = self.pool()?;

        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM kv_store WHERE namespace = ? AND key = ?")
                .bind(namespace)
                .bind(key)
                .fetch_optional(pool)
                .await
                .map_err(|e| CapabilityError::ResourceError(format!("kv get: {e}")))?;

        Ok(row.map(|r| r.0))
    }

    /// KV ストアに値を保存（UPSERT）
    pub async fn kv_set(&self, namespace: &str, key: &str, value: &str) -> CapabilityResult<()> {
        let pool = self.pool()?;

        sqlx::query(
            "INSERT INTO kv_store (namespace, key, value) VALUES (?, ?, ?)
             ON CONFLICT(namespace, key) DO UPDATE SET value = excluded.value,
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(namespace)
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .map_err(|e| CapabilityError::ResourceError(format!("kv set: {e}")))?;

        Ok(())
    }

    /// KV ストアから値を削除
    pub async fn kv_delete(&self, namespace: &str, key: &str) -> CapabilityResult<bool> {
        let pool = self.pool()?;

        let result = sqlx::query("DELETE FROM kv_store WHERE namespace = ? AND key = ?")
            .bind(namespace)
            .bind(key)
            .execute(pool)
            .await
            .map_err(|e| CapabilityError::ResourceError(format!("kv delete: {e}")))?;

        Ok(result.rows_affected() > 0)
    }

    /// KV ストアの namespace 内のキー一覧を取得
    pub async fn kv_list_keys(&self, namespace: &str) -> CapabilityResult<Vec<String>> {
        let pool = self.pool()?;

        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT key FROM kv_store WHERE namespace = ? ORDER BY key")
                .bind(namespace)
                .fetch_all(pool)
                .await
                .map_err(|e| CapabilityError::ResourceError(format!("kv list: {e}")))?;

        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

// =============================================================================
// Capability トレイト実装
// =============================================================================

#[async_trait]
impl Capability for DbCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new("db", "0.1.0", "SQLite ローカル永続化")
            .with_type("infrastructure")
            .with_params(CapabilityParams {
                power: Rank::C,
                speed: Rank::B,
                range: Rank::E,   // ローカルのみ
                stamina: Rank::A, // 永続化
                precision: Rank::B,
                potential: Rank::A,
            })
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
        if self.state != CapabilityState::Uninitialized {
            return Err(CapabilityError::AlreadyInitialized);
        }

        self.state = CapabilityState::Initializing;

        // DB ディレクトリを確保
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CapabilityError::InitializationFailed(format!("create db dir: {e}"))
            })?;
        }

        // SQLite 接続プールを作成
        let db_url = format!("sqlite://{}?mode=rwc", self.db_path.display());
        let options = SqliteConnectOptions::from_str(&db_url)
            .map_err(|e| CapabilityError::InitializationFailed(format!("parse db url: {e}")))?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| CapabilityError::InitializationFailed(format!("connect db: {e}")))?;

        self.pool = Some(pool);

        // マイグレーション実行
        self.run_migrations().await?;

        self.state = CapabilityState::Idle;
        tracing::info!("DbCapability initialized: {}", self.db_path.display());
        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        self.state = CapabilityState::ShuttingDown;

        if let Some(pool) = self.pool.take() {
            pool.close().await;
        }

        self.state = CapabilityState::Stopped;
        tracing::info!("DbCapability shut down");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の一時 DB を作成
    async fn setup_test_db() -> DbCapability {
        let tmp = std::env::temp_dir().join(format!("vp-test-{}.db", uuid::Uuid::new_v4()));
        let mut db = DbCapability::with_path(tmp);
        let ctx = CapabilityContext::new();
        db.initialize(&ctx).await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_lifecycle() {
        let mut db = setup_test_db().await;
        assert_eq!(db.state(), CapabilityState::Idle);

        db.shutdown().await.unwrap();
        assert_eq!(db.state(), CapabilityState::Stopped);

        // テスト DB を削除
        let _ = std::fs::remove_file(&db.db_path);
    }

    #[tokio::test]
    async fn test_process_events() {
        let mut db = setup_test_db().await;

        // イベント記録
        let id = db
            .record_stand_event(33000, "start", Some("my-project"), Some("debug=simple"))
            .await
            .unwrap();
        assert!(id > 0);

        db.record_stand_event(33000, "stop", Some("my-project"), None)
            .await
            .unwrap();

        // 全件クエリ
        let events = db
            .query_stand_history(StandEventFilter::default())
            .await
            .unwrap();
        assert_eq!(events.len(), 2);

        // ポートフィルター
        let events = db
            .query_stand_history(StandEventFilter {
                port: Some(33000),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 2);

        // タイプフィルター
        let events = db
            .query_stand_history(StandEventFilter {
                event_type: Some("start".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 1);

        db.shutdown().await.unwrap();
        let _ = std::fs::remove_file(&db.db_path);
    }

    #[tokio::test]
    async fn test_settings() {
        let mut db = setup_test_db().await;

        // グローバル設定
        assert!(db.get_setting("theme").await.unwrap().is_none());
        db.set_setting("theme", "dark").await.unwrap();
        assert_eq!(
            db.get_setting("theme").await.unwrap(),
            Some("dark".to_string())
        );

        // UPSERT: 上書き
        db.set_setting("theme", "light").await.unwrap();
        assert_eq!(
            db.get_setting("theme").await.unwrap(),
            Some("light".to_string())
        );

        // プロジェクト固有設定
        db.set_project_setting("vp", "debug_mode", "detail")
            .await
            .unwrap();
        assert_eq!(
            db.get_project_setting("vp", "debug_mode").await.unwrap(),
            Some("detail".to_string())
        );

        // グローバルとプロジェクトは独立
        assert!(db.get_setting("debug_mode").await.unwrap().is_none());

        db.shutdown().await.unwrap();
        let _ = std::fs::remove_file(&db.db_path);
    }

    #[tokio::test]
    async fn test_kv_store() {
        let mut db = setup_test_db().await;

        // Set & Get
        db.kv_set("agent", "session_id", "abc-123").await.unwrap();
        assert_eq!(
            db.kv_get("agent", "session_id").await.unwrap(),
            Some("abc-123".to_string())
        );

        // 別 namespace は独立
        assert!(db.kv_get("midi", "session_id").await.unwrap().is_none());

        // UPSERT
        db.kv_set("agent", "session_id", "def-456").await.unwrap();
        assert_eq!(
            db.kv_get("agent", "session_id").await.unwrap(),
            Some("def-456".to_string())
        );

        // List keys
        db.kv_set("agent", "model", "claude-4").await.unwrap();
        let keys = db.kv_list_keys("agent").await.unwrap();
        assert_eq!(keys, vec!["model", "session_id"]);

        // Delete
        let deleted = db.kv_delete("agent", "session_id").await.unwrap();
        assert!(deleted);
        assert!(db.kv_get("agent", "session_id").await.unwrap().is_none());

        // 存在しないキーの削除
        let deleted = db.kv_delete("agent", "nonexistent").await.unwrap();
        assert!(!deleted);

        db.shutdown().await.unwrap();
        let _ = std::fs::remove_file(&db.db_path);
    }
}
