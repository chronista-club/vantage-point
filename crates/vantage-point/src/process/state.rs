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
use crate::capability::mailbox::MailboxHandle;
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
    /// Processの待ち受けポート番号
    pub port: u16,
    /// ファイル監視マネージャー
    pub file_watchers: Arc<tokio::sync::Mutex<FileWatcherManager>>,
    /// Terminal チャネル認証トークン
    pub terminal_token: String,
    /// tmux ペイン管理 Actor（遅延初期化: 初回アクセス時にセッションを検索して起動）
    pub tmux: Arc<tokio::sync::Mutex<Option<TmuxHandle>>>,
    /// tmux セッション名（遅延初期化で使用）
    pub tmux_session_name: String,
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
    /// MCP 用 Mailbox ハンドル（VP-24: MCP → Capability への Mailbox 配信）
    pub mcp_mailbox: Option<MailboxHandle>,
    /// SurrealDB クライアント（VP-21: 状態管理の DB 統一）
    pub vpdb: Option<crate::db::SharedVpDb>,
    /// Whitesnake 🐍 — 汎用永続化レイヤー
    pub whitesnake: crate::capability::Whitesnake,
}

impl AppState {
    /// tmux ハンドルを取得（遅延初期化: 未接続なら tmux セッションを検索して起動）
    pub async fn ensure_tmux(&self) -> Option<TmuxHandle> {
        let mut guard = self.tmux.lock().await;
        if let Some(ref handle) = *guard {
            return Some(handle.clone());
        }

        // tmux セッションが存在すれば起動
        if crate::tmux::is_tmux_available()
            && crate::tmux::session_exists(&self.tmux_session_name)
            && let Some(handle) = super::tmux_actor::spawn_for_session(&self.tmux_session_name)
        {
            *guard = Some(handle.clone());
            tracing::info!("TmuxActor 遅延初期化: session={}", self.tmux_session_name);
            return Some(handle);
        }

        None
    }

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
    // ペイン状態永続化（Whitesnake 🐍 経由）
    // =========================================================================

    /// RetainedStore から Paisley Park のペイン状態を Whitesnake に保存
    ///
    /// Whitesnake が DISC として永続化（FileBackend）。
    /// 旧: SurrealDB + JSON ファイルの二重管理 → Whitesnake に統一。
    pub async fn persist_pane_contents(&self) {
        let pattern = TopicPattern::parse("process/paisley-park/command/show/#");
        let retained = self.topic_router.retained();
        let store = retained.read().await;
        let matching = store.get_matching(&pattern);

        if matching.is_empty() {
            return;
        }

        // RetainedStore の ProcessMessage::Show → PaneState に変換して DISC に焼く
        let mut count = 0;
        for (_topic, msg) in &matching {
            if let ProcessMessage::Show {
                pane_id,
                content,
                title,
                ..
            } = msg
            {
                let pane_state = PaneState {
                    content: content.clone(),
                    title: title.clone(),
                };
                let key = format!("pane/{}", pane_id);
                if let Err(e) = self
                    .whitesnake
                    .extract("paisley-park", &key, &pane_state)
                    .await
                {
                    tracing::warn!("Whitesnake DISC 保存失敗 ({}): {}", pane_id, e);
                } else {
                    count += 1;
                }
            }
        }

        if count > 0 {
            tracing::info!("{} ペイン状態を DISC に保存 (port={})", count, self.port);
        }
    }

    /// Whitesnake から DISC を読み出し、RetainedStore に復元する
    pub async fn restore_pane_contents(&self) {
        // Whitesnake から paisley-park/pane/* を復元
        match self
            .whitesnake
            .list_by_prefix("paisley-park", "pane/")
            .await
        {
            Ok(discs) if !discs.is_empty() => {
                let retained = self.topic_router.retained();
                let mut store = retained.write().await;
                let mut count = 0;
                for disc in &discs {
                    // key = "pane/{pane_id}" → pane_id を抽出
                    let pane_id = disc.key.strip_prefix("pane/").unwrap_or(&disc.key);
                    if let Ok(pane_state) = disc.extract::<PaneState>() {
                        let topic = format!("process/paisley-park/command/show/{}", pane_id);
                        store.set(
                            &topic,
                            ProcessMessage::Show {
                                pane_id: pane_id.to_string(),
                                content: pane_state.content,
                                append: false,
                                title: pane_state.title,
                            },
                        );
                        count += 1;
                    }
                }
                if count > 0 {
                    tracing::info!(
                        "ペイン状態を Whitesnake DISC から復元: {} ペイン (port={})",
                        count,
                        self.port
                    );
                }
            }
            Ok(_) => {
                // DISC が空 — 旧形式からのマイグレーション不要（初回起動）
            }
            Err(e) => {
                tracing::warn!("Whitesnake DISC 読み出し失敗: {}", e);
            }
        }
    }

    /// Canvas レイアウト状態を Whitesnake に保存
    pub async fn save_canvas_layout(&self, layout: &serde_json::Value) {
        if let Err(e) = self
            .whitesnake
            .extract("paisley-park", "layout", layout)
            .await
        {
            tracing::warn!("Canvas レイアウト DISC 保存に失敗: {}", e);
        }
    }

    /// Canvas レイアウト状態を Whitesnake から復元
    pub async fn load_canvas_layout(&self) -> Option<serde_json::Value> {
        match self.whitesnake.insert("paisley-park", "layout").await {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!("Canvas レイアウト DISC 読み出しに失敗: {}", e);
                None
            }
        }
    }
}
