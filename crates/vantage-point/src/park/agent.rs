//! Paisley Park Agent 連携
//!
//! Claude CLI を使用した AI Agent 機能を提供。
//! 設計: docs/spec/08-paisley-park.md REQ-PAISLEY-004

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, RwLock, mpsc};

use super::client::WorldClient;
use crate::agent::{AgentConfig, AgentEvent, AgentMode, InteractiveClaudeAgent};

/// Agent セッション状態
#[derive(Debug, Clone, Default)]
pub struct AgentSession {
    /// セッション ID (Claude CLI から取得)
    pub session_id: Option<String>,
    /// 使用中のモデル
    pub model: Option<String>,
    /// 利用可能なツール
    pub tools: Vec<String>,
    /// 接続中の MCP サーバー
    pub mcp_servers: Vec<String>,
    /// 実行中かどうか
    pub is_running: bool,
}

/// Paisley Park Agent
///
/// Claude CLI を使用してプロジェクト単位の AI 機能を提供
pub struct ParkAgent {
    /// プロジェクトパス (working directory)
    project_path: PathBuf,
    /// World Client (View 操作用)
    world_client: WorldClient,
    /// セッション状態
    session: Arc<RwLock<AgentSession>>,
    /// Interactive Agent (持続プロセス)
    agent: Option<InteractiveClaudeAgent>,
}

impl ParkAgent {
    /// 新しい ParkAgent を作成
    pub fn new(project_path: PathBuf, world_client: WorldClient) -> Self {
        Self {
            project_path,
            world_client,
            session: Arc::new(RwLock::new(AgentSession::default())),
            agent: None,
        }
    }

    /// Agent を起動（Interactive モード）
    pub async fn start(&mut self) -> Result<()> {
        let config = AgentConfig {
            mode: AgentMode::Interactive,
            working_dir: Some(self.project_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let agent = InteractiveClaudeAgent::new(config);
        agent.start().await?;

        self.agent = Some(agent);
        self.session.write().await.is_running = true;

        tracing::info!("ParkAgent 起動: {}", self.project_path.display());
        Ok(())
    }

    /// プロンプトを送信
    pub async fn prompt(&self, message: &str) -> Result<()> {
        let agent = self
            .agent
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent が起動していません"))?;

        agent.send(message).await?;
        Ok(())
    }

    /// イベントレシーバーを取得
    ///
    /// Agent からのイベント（応答テキスト、ツール呼び出し、完了等）を受信
    pub fn events(&self) -> Option<Arc<Mutex<mpsc::Receiver<AgentEvent>>>> {
        self.agent.as_ref().map(|a| a.events())
    }

    /// セッション状態を取得
    pub async fn session(&self) -> AgentSession {
        self.session.read().await.clone()
    }

    /// セッション状態を更新（イベントから）
    pub async fn update_session(&self, event: &AgentEvent) {
        let mut session = self.session.write().await;
        match event {
            AgentEvent::SessionInit {
                session_id,
                model,
                tools,
                mcp_servers,
            } => {
                session.session_id = Some(session_id.clone());
                session.model = model.clone();
                session.tools = tools.clone();
                session.mcp_servers = mcp_servers.clone();
            }
            AgentEvent::Done { .. } => {
                // タスク完了
            }
            _ => {}
        }
    }

    /// Agent を停止
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(agent) = self.agent.take() {
            // Agent の停止処理
            drop(agent);
        }
        self.session.write().await.is_running = false;
        tracing::info!("ParkAgent 停止");
        Ok(())
    }

    /// View にコンテンツを表示
    pub async fn show(&self, pane_id: &str, content: &str) -> Result<()> {
        self.world_client
            .show(pane_id, "markdown", content, false)
            .await?;
        Ok(())
    }
}
