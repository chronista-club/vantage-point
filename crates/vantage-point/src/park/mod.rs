//! Paisley Park - プロジェクト単位の AI Agent
//!
//! JoJo's Bizarre Adventure Part 8 の広瀬康穂のスタンド "ペイズリー・パーク" にちなんだ命名。
//! 情報を収集・整理し、ユーザーを目的地へ導く能力のように、
//! プロジェクト単位で AI Agent を管理し、開発を支援する。
//!
//! ## 責務
//! - プロジェクト単位の Claude CLI 統合
//! - The World への登録・ハートビート
//! - Terminal 管理
//! - View 操作
//!
//! ## アーキテクチャ
//! ```text
//!  ┌──────────────────────────────────────────────┐
//!  │                 Paisley Park                  │
//!  │               [::1]:dynamic                   │
//!  ├──────────────────────────────────────────────┤
//!  │  ┌────────────────┐  ┌────────────────┐      │
//!  │  │   Claude CLI   │  │   Terminal     │      │
//!  │  │   Integration  │  │    Manager     │      │
//!  │  └────────────────┘  └────────────────┘      │
//!  │  ┌────────────────┐  ┌────────────────┐      │
//!  │  │   View Ops     │  │   Heartbeat    │      │
//!  │  │   (to World)   │  │    Service     │      │
//!  │  └────────────────┘  └────────────────┘      │
//!  └──────────────────────────────────────────────┘
//!            ▲                    │
//!            │ HTTP               │ Heartbeat
//!            ▼                    ▼
//!       ┌─────────────────────────────────┐
//!       │            The World            │
//!       │          [::1]:33000            │
//!       └─────────────────────────────────┘
//! ```

pub mod agent;
pub mod client;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use client::WorldClient;

/// Paisley Park 設定
#[derive(Debug, Clone)]
pub struct ParkConfig {
    /// プロジェクト ID
    pub project_id: String,
    /// プロジェクトパス
    pub project_path: PathBuf,
    /// The World の URL
    pub world_url: String,
    /// ハートビート間隔
    pub heartbeat_interval: Duration,
}

impl Default for ParkConfig {
    fn default() -> Self {
        Self {
            project_id: "default".to_string(),
            project_path: PathBuf::from("."),
            world_url: "http://localhost:33000".to_string(),
            heartbeat_interval: Duration::from_secs(10),
        }
    }
}

/// Paisley Park のステータス
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkStatus {
    /// 未接続
    Disconnected,
    /// The World へ接続中
    Connecting,
    /// 登録済み・アイドル
    Idle,
    /// タスク実行中
    Busy,
    /// エラー状態
    Error(String),
}

/// Paisley Park インスタンス
///
/// プロジェクト単位の AI Agent として動作
pub struct PaisleyPark {
    /// 設定
    config: ParkConfig,
    /// Park ID (The World から割り当て)
    park_id: Arc<RwLock<Option<String>>>,
    /// セッショントークン
    session_token: Arc<RwLock<Option<String>>>,
    /// 現在のステータス
    status: Arc<RwLock<ParkStatus>>,
    /// World クライアント
    world_client: WorldClient,
    /// キャンセルトークン
    cancel: CancellationToken,
}

impl PaisleyPark {
    /// 新しい Paisley Park を作成
    pub fn new(config: ParkConfig) -> Self {
        let world_client = WorldClient::new(&config.world_url);
        Self {
            config,
            park_id: Arc::new(RwLock::new(None)),
            session_token: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(ParkStatus::Disconnected)),
            world_client,
            cancel: CancellationToken::new(),
        }
    }

    /// The World に登録して起動
    pub async fn start(&self, port: u16) -> Result<()> {
        tracing::info!("Paisley Park 起動中... 「情報を収集します」");

        // ステータス更新
        *self.status.write().await = ParkStatus::Connecting;

        // The World に登録
        let response = self
            .world_client
            .register(
                &self.config.project_id,
                self.config.project_path.to_string_lossy().as_ref(),
                port,
            )
            .await?;

        // 登録情報を保存
        *self.park_id.write().await = Some(response.park_id.clone());
        *self.session_token.write().await = Some(response.session_token);
        *self.status.write().await = ParkStatus::Idle;

        tracing::info!("The World に登録完了: {}", response.park_id);

        // ハートビートタスク開始
        self.start_heartbeat().await;

        Ok(())
    }

    /// ハートビートタスクを開始
    async fn start_heartbeat(&self) {
        let park_id = Arc::clone(&self.park_id);
        let status = Arc::clone(&self.status);
        let client = self.world_client.clone();
        let interval = self.config.heartbeat_interval;
        let cancel = self.cancel.clone();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("ハートビート停止");
                        break;
                    }
                    _ = ticker.tick() => {
                        let id = park_id.read().await;
                        if let Some(ref park_id) = *id {
                            let current_status = status.read().await.clone();
                            let status_str = match current_status {
                                ParkStatus::Idle => "idle",
                                ParkStatus::Busy => "busy",
                                ParkStatus::Error(_) => "error",
                                _ => "idle",
                            };

                            if let Err(e) = client.heartbeat(park_id, status_str).await {
                                tracing::warn!("ハートビート失敗: {}", e);
                            }
                        }
                    }
                }
            }
        });
    }

    /// 停止
    pub async fn stop(&self) -> Result<()> {
        tracing::info!("Paisley Park 停止中...");

        // ハートビート停止
        self.cancel.cancel();

        // The World から解除
        let park_id = self.park_id.read().await;
        if let Some(ref id) = *park_id {
            let _ = self.world_client.unregister(id, Some("shutdown")).await;
        }

        *self.status.write().await = ParkStatus::Disconnected;
        tracing::info!("Paisley Park 停止完了");
        Ok(())
    }

    /// 現在のステータスを取得
    pub async fn status(&self) -> ParkStatus {
        self.status.read().await.clone()
    }

    /// Park ID を取得
    pub async fn park_id(&self) -> Option<String> {
        self.park_id.read().await.clone()
    }
}
