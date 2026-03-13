//! アプリケーション状態モジュール
//!
//! Process サーバーの共有状態と関連型を定義する。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::capabilities::ProcessCapabilities;
use super::hub::Hub;
use super::process_runner::ProcessRegistry;
use super::pty::PtyManager;
use super::session::SessionManager;
use super::tmux_actor::TmuxHandle;
use super::topic_router::TopicRouter;
use crate::agent::InteractiveClaudeAgent;
use crate::agui::AgUiEvent;
use crate::capability::{ProcessManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::mcp::PermissionResponse;
use crate::process::topic::TopicPattern;
use crate::protocol::{Content, DebugMode, ProcessMessage};

/// ペインの最新コンテンツ（Canvas 再接続時の状態復元用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PaneState {
    pub content: Content,
    pub title: Option<String>,
}

/// Pending permission request entry
pub(crate) struct PendingPermission {
    /// Original input from the permission request (needed for "allow" response)
    pub original_input: serde_json::Value,
    /// Response once user has responded (None = still waiting)
    pub response: Option<PermissionResponse>,
}

/// Pending user prompt request entry (REQ-PROMPT-001 to REQ-PROMPT-005)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PendingPrompt {
    /// The prompt request data
    pub request: PendingPromptRequest,
    /// Response once user has responded (None = still waiting)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<UserPromptResponseData>,
}

/// User prompt request data stored in pending prompts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingPromptRequest {
    pub request_id: String,
    pub prompt_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<PromptOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    pub timeout_seconds: u32,
    pub created_at: u64,
}

/// Prompt option for select/multi_select
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PromptOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// User prompt response data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UserPromptResponseData {
    /// Response outcome: approved, rejected, cancelled, timeout
    pub outcome: String,
    /// Text response (for input type or optional comment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Selected option IDs (for select/multi_select)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_options: Option<Vec<String>>,
}

/// スクリーンショットキャプチャの応答データ
pub(crate) struct ScreenshotData {
    pub data: String,
    pub width: u32,
    pub height: u32,
}

/// Application state
pub(crate) struct AppState {
    pub hub: Hub,
    /// Session manager for multiple Claude sessions
    pub sessions: Arc<RwLock<SessionManager>>,
    /// Cancellation token for current chat request
    pub cancel_token: Arc<RwLock<CancellationToken>>,
    /// Debug display mode
    pub debug_mode: DebugMode,
    /// Shutdown signal token
    pub shutdown_token: CancellationToken,
    /// Project directory for Claude agent
    pub project_dir: String,
    /// Pending permission requests: request_id -> response channel
    pub pending_permissions: Arc<RwLock<HashMap<String, PendingPermission>>>,
    /// Pending user prompts: request_id -> response (REQ-PROMPT-001)
    pub pending_prompts: Arc<RwLock<HashMap<String, PendingPrompt>>>,
    /// Capability system (Agent, MIDI, Protocol)
    pub capabilities: Arc<ProcessCapabilities>,
    /// World capability for managing multiple processes (optional, only for world mode)
    pub world: Option<Arc<RwLock<ProcessManagerCapability>>>,
    /// Update capability for version checking (optional, only for world mode)
    pub update: Option<Arc<RwLock<UpdateCapability>>>,
    /// Interactive Claude agent (stream-json mode for structured communication)
    pub interactive_agent: Arc<RwLock<Option<InteractiveClaudeAgent>>>,
    /// PTYセッションマネージャー（ターミナル機能）- レガシー、tmux未対応環境用
    pub pty_manager: Arc<tokio::sync::Mutex<PtyManager>>,
    /// Canvasウィンドウのプロセス管理（PID）
    pub canvas_pid: Arc<tokio::sync::Mutex<Option<u32>>>,
    /// Processの待ち受けポート番号（Canvas起動時に使用）
    pub port: u16,
    /// ファイル監視マネージャー
    pub file_watchers: Arc<tokio::sync::Mutex<FileWatcherManager>>,
    /// Terminal チャネル認証トークン
    pub terminal_token: String,
    /// tmux ペイン管理 Actor（tmux 環境下でのみ有効）
    pub tmux: Option<TmuxHandle>,
    /// スクリーンショット応答待ち: request_id → oneshot sender
    /// プロセスレジストリ（ProcessRunner）
    pub process_registry: Arc<tokio::sync::Mutex<ProcessRegistry>>,
    pub screenshot_waiters:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<ScreenshotData>>>>,
    /// Topic ベースのメッセージルーター（Hub → Topic 振り分け）
    pub topic_router: Arc<TopicRouter>,
    /// Canvas WS クライアントへの送信チャネル（HTTP API → lanes WS handler）
    pub canvas_senders: Arc<tokio::sync::Mutex<Vec<tokio::sync::mpsc::Sender<serde_json::Value>>>>,
    /// プロセス起動時刻（ISO 8601）
    pub started_at: String,
}

impl AppState {
    /// Send debug info to connected clients
    pub fn send_debug(&self, category: &str, message: &str, data: Option<serde_json::Value>) {
        if self.debug_mode == DebugMode::None {
            return;
        }

        // For simple mode, skip detail-level messages
        if self.debug_mode == DebugMode::Simple && data.is_some() {
            // Still send but without detailed data
            self.hub.broadcast(ProcessMessage::DebugInfo {
                level: DebugMode::Simple,
                category: category.to_string(),
                message: message.to_string(),
                data: None,
                tags: vec![],
            });
        } else {
            self.hub.broadcast(ProcessMessage::DebugInfo {
                level: self.debug_mode,
                category: category.to_string(),
                message: message.to_string(),
                data,
                tags: vec![],
            });
        }
    }

    /// Send debug info only in detail mode
    pub fn send_debug_detail(&self, category: &str, message: &str, data: serde_json::Value) {
        if self.debug_mode == DebugMode::Detail {
            self.hub.broadcast(ProcessMessage::DebugInfo {
                level: DebugMode::Detail,
                category: category.to_string(),
                message: message.to_string(),
                data: Some(data),
                tags: vec![],
            });
        }
    }

    /// Send AG-UI event to connected clients (REQ-AGUI-040)
    pub fn send_agui_event(&self, event: AgUiEvent) {
        self.hub.broadcast(ProcessMessage::AgUi { event });
    }

    // =========================================================================
    // Canvas 状態永続化
    // =========================================================================

    /// ペイン状態のファイルパス
    fn pane_state_path(&self) -> std::path::PathBuf {
        crate::config::config_dir()
            .join("state")
            .join(format!("{}-panes.json", self.port))
    }

    /// Canvas レイアウト状態のファイルパス
    fn canvas_layout_path(&self) -> std::path::PathBuf {
        crate::config::config_dir()
            .join("state")
            .join(format!("{}-canvas-layout.json", self.port))
    }

    /// RetainedStore から Paisley Park のペイン状態を取得してディスクに保存
    pub async fn persist_pane_contents(&self) {
        let pattern = TopicPattern::parse("process/paisley-park/command/show/#");
        let retained = self.topic_router.retained();
        let store = retained.read().await;
        let matching = store.get_matching(&pattern);

        if matching.is_empty() {
            return;
        }

        // RetainedStore の ProcessMessage::Show → PaneState に変換
        let mut panes = HashMap::new();
        for (_topic, msg) in &matching {
            if let ProcessMessage::Show {
                pane_id,
                content,
                title,
                ..
            } = msg
            {
                panes.insert(
                    pane_id.clone(),
                    PaneState {
                        content: content.clone(),
                        title: title.clone(),
                    },
                );
            }
        }

        if panes.is_empty() {
            return;
        }

        let path = self.pane_state_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string(&panes) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("ペイン状態の保存に失敗: {}", e);
                }
            }
            Err(e) => tracing::warn!("ペイン状態のシリアライズに失敗: {}", e),
        }
    }

    /// ディスクからペイン状態を復元し、RetainedStore にセットする
    pub async fn restore_pane_contents(&self) {
        let path = self.pane_state_path();
        if !path.exists() {
            return;
        }

        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<HashMap<String, PaneState>>(&json) {
                Ok(restored) => {
                    let count = restored.len();
                    let retained = self.topic_router.retained();
                    let mut store = retained.write().await;
                    for (pane_id, pane_state) in restored {
                        let topic = format!("process/paisley-park/command/show/{}", pane_id);
                        store.set(
                            &topic,
                            ProcessMessage::Show {
                                pane_id,
                                content: pane_state.content,
                                append: false,
                                title: pane_state.title,
                            },
                        );
                    }
                    tracing::info!("ペイン状態を復元: {} ペイン (port={})", count, self.port);
                }
                Err(e) => tracing::warn!("ペイン状態のデシリアライズに失敗: {}", e),
            },
            Err(e) => tracing::warn!("ペイン状態ファイルの読み込みに失敗: {}", e),
        }
    }

    /// Canvas レイアウト状態を保存（フロントエンドからの JSON をそのまま保存）
    pub fn save_canvas_layout(&self, layout: &serde_json::Value) {
        let path = self.canvas_layout_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(layout) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Canvas レイアウト保存に失敗: {}", e);
                }
            }
            Err(e) => tracing::warn!("Canvas レイアウトのシリアライズに失敗: {}", e),
        }
    }

    /// Canvas レイアウト状態を復元
    pub fn load_canvas_layout(&self) -> Option<serde_json::Value> {
        let path = self.canvas_layout_path();
        let json = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
    }
}
