//! Vantage DB - SurrealDB ローカル永続化
//!
//! The World のデータ永続化を担当。
//! 設計: docs/spec/07-the-world.md REQ-WORLD-003
//!
//! ## 保存対象
//! - プロジェクト情報
//! - セッション履歴
//! - Agent 対話ログ
//! - ユーザー設定

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Vantage DB 設定
#[derive(Debug, Clone)]
pub struct VantageDbConfig {
    /// データディレクトリ
    pub data_dir: PathBuf,
    /// デバッグモード
    pub debug: bool,
}

impl Default for VantageDbConfig {
    fn default() -> Self {
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("vantage-point")
            .join("db");

        Self {
            data_dir,
            debug: false,
        }
    }
}

/// プロジェクト情報（永続化用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    /// プロジェクト ID
    pub id: String,
    /// プロジェクト名
    pub name: String,
    /// プロジェクトパス
    pub path: String,
    /// 最終アクセス日時
    pub last_accessed: String,
    /// お気に入りフラグ
    pub favorite: bool,
}

/// セッション履歴（永続化用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// セッション ID
    pub id: String,
    /// プロジェクト ID
    pub project_id: String,
    /// 開始日時
    pub started_at: String,
    /// 終了日時
    pub ended_at: Option<String>,
    /// ステータス
    pub status: String,
}

/// Agent 対話ログ（永続化用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    /// レコード ID
    pub id: String,
    /// セッション ID
    pub session_id: String,
    /// ロール (user / assistant)
    pub role: String,
    /// 内容
    pub content: String,
    /// タイムスタンプ
    pub timestamp: String,
}

/// Vantage DB インスタンス
///
/// SurrealDB を使用したローカル永続化
pub struct VantageDb {
    /// 設定
    config: VantageDbConfig,
    /// 接続済みフラグ
    connected: bool,
}

impl VantageDb {
    /// 新しい VantageDb を作成
    pub fn new(config: VantageDbConfig) -> Self {
        Self {
            config,
            connected: false,
        }
    }

    /// デフォルト設定で作成
    pub fn default_instance() -> Self {
        Self::new(VantageDbConfig::default())
    }

    /// データベースに接続
    pub async fn connect(&mut self) -> Result<()> {
        // データディレクトリを作成
        std::fs::create_dir_all(&self.config.data_dir)?;

        tracing::info!("Vantage DB 接続: {:?}", self.config.data_dir);

        // TODO: SurrealDB 接続を実装
        // 現時点ではファイルベースのストレージとして動作
        self.connected = true;

        Ok(())
    }

    /// 接続を閉じる
    pub async fn disconnect(&mut self) -> Result<()> {
        self.connected = false;
        tracing::info!("Vantage DB 切断");
        Ok(())
    }

    /// 接続状態を確認
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    // =========================================================================
    // プロジェクト操作
    // =========================================================================

    /// プロジェクトを保存
    pub async fn save_project(&self, project: &ProjectRecord) -> Result<()> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let path = self.config.data_dir.join("projects").join(&project.id);
        std::fs::create_dir_all(path.parent().unwrap())?;

        let json = serde_json::to_string_pretty(project)?;
        std::fs::write(path.with_extension("json"), json)?;

        tracing::debug!("プロジェクト保存: {}", project.id);
        Ok(())
    }

    /// プロジェクトを取得
    pub async fn get_project(&self, project_id: &str) -> Result<Option<ProjectRecord>> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let path = self
            .config
            .data_dir
            .join("projects")
            .join(project_id)
            .with_extension("json");

        if !path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(&path)?;
        let project: ProjectRecord = serde_json::from_str(&json)?;
        Ok(Some(project))
    }

    /// 全プロジェクトを取得
    pub async fn list_projects(&self) -> Result<Vec<ProjectRecord>> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let projects_dir = self.config.data_dir.join("projects");
        if !projects_dir.exists() {
            return Ok(vec![]);
        }

        let mut projects = Vec::new();
        for entry in std::fs::read_dir(&projects_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                let json = std::fs::read_to_string(&path)?;
                if let Ok(project) = serde_json::from_str::<ProjectRecord>(&json) {
                    projects.push(project);
                }
            }
        }

        Ok(projects)
    }

    // =========================================================================
    // セッション操作
    // =========================================================================

    /// セッションを保存
    pub async fn save_session(&self, session: &SessionRecord) -> Result<()> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let path = self
            .config
            .data_dir
            .join("sessions")
            .join(&session.project_id)
            .join(&session.id);
        std::fs::create_dir_all(path.parent().unwrap())?;

        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(path.with_extension("json"), json)?;

        tracing::debug!("セッション保存: {}", session.id);
        Ok(())
    }

    /// セッションを取得
    pub async fn get_session(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let path = self
            .config
            .data_dir
            .join("sessions")
            .join(project_id)
            .join(session_id)
            .with_extension("json");

        if !path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(&path)?;
        let session: SessionRecord = serde_json::from_str(&json)?;
        Ok(Some(session))
    }

    /// プロジェクトのセッション一覧を取得
    pub async fn list_sessions(&self, project_id: &str) -> Result<Vec<SessionRecord>> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let sessions_dir = self.config.data_dir.join("sessions").join(project_id);
        if !sessions_dir.exists() {
            return Ok(vec![]);
        }

        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                let json = std::fs::read_to_string(&path)?;
                if let Ok(session) = serde_json::from_str::<SessionRecord>(&json) {
                    sessions.push(session);
                }
            }
        }

        Ok(sessions)
    }

    // =========================================================================
    // 対話ログ操作
    // =========================================================================

    /// 対話ログを追加
    pub async fn add_conversation(&self, record: &ConversationRecord) -> Result<()> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let path = self
            .config
            .data_dir
            .join("conversations")
            .join(&record.session_id);
        std::fs::create_dir_all(&path)?;

        let file_path = path.join(&record.id).with_extension("json");
        let json = serde_json::to_string_pretty(record)?;
        std::fs::write(file_path, json)?;

        tracing::debug!("対話ログ追加: {} ({})", record.id, record.role);
        Ok(())
    }

    /// セッションの対話ログを取得
    pub async fn get_conversations(&self, session_id: &str) -> Result<Vec<ConversationRecord>> {
        if !self.connected {
            return Err(anyhow::anyhow!("DB に接続されていません"));
        }

        let conv_dir = self.config.data_dir.join("conversations").join(session_id);
        if !conv_dir.exists() {
            return Ok(vec![]);
        }

        let mut conversations = Vec::new();
        for entry in std::fs::read_dir(&conv_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                let json = std::fs::read_to_string(&path)?;
                if let Ok(conv) = serde_json::from_str::<ConversationRecord>(&json) {
                    conversations.push(conv);
                }
            }
        }

        // タイムスタンプでソート
        conversations.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(conversations)
    }
}
