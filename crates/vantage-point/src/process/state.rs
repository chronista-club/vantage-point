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
use crate::capability::{ActorRegistry, ProcessManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::process::topic::TopicPattern;
use crate::protocol::{Content, DebugMode, ProcessMessage};

/// ペインの最新コンテンツ（Canvas 再接続時の状態復元用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PaneState {
    pub content: Content,
    pub title: Option<String>,
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
    /// 解決済 project 名 (config の `projects[].name`、 未登録ならディレクトリ名)
    ///
    /// R3 (wire cross-process delivery): `wire_send` の宛先分類で「自 SP の project か否か」を
    /// 判定するのに使う。 `agent@<project>` の `<project>` が本 field と異なれば remote SP。
    /// World mode では空文字列 (= cross-process forward は SP mode 専用)。
    pub project_name: String,
    /// Pending user prompts: request_id -> response (REQ-PROMPT-001)
    pub pending_prompts: Arc<RwLock<HashMap<String, PendingPrompt>>>,
    /// Capability system (Agent, MIDI, Protocol)
    pub capabilities: Arc<ProcessCapabilities>,
    /// VP-159 PR-4b: Stand / Service actor の supervisor 受け皿。
    ///
    /// SP mode で notify / lane-spawn を `spawn_service` 経由で起動・register、 JoinHandle を保持。
    /// World mode では空で構築 (= World scope actor の register は後続 PR、 MidiCapability の
    /// metadata register は dynamic routing vision 確定後、 cf. design-spark `mem_1CavFi5D1aMSpEkas89SvQ`)。
    /// PR-5 supervisor 統一で JoinHandle 経由の abort / await を activate する foundation。
    pub actor_registry: Arc<RwLock<ActorRegistry>>,
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
    /// SurrealDB クライアント（VP-21: 状態管理の DB 統一）
    pub vpdb: Option<crate::db::SharedVpDb>,
    /// Phase A ①: wiremsg threaded inbox store (= `wire_send` / `wire_recv` の実体)
    ///
    /// `vpdb` が `Some` の時に同 DB 接続から build する。 TopicRouter は介さず、
    /// `wire_recv` がこの store を直接 long-poll する。
    /// 設計 memory: `mem_1CbD9H1KGQykBaFG8XXVsn`。
    ///
    /// wiremsg R5-3: 旧 `msgbox_store` (= `WhitesnakeStore`、 msgs table) は撤去済。
    /// msg messaging はこの wiremsg store に一本化。
    pub wiremsg_store: Option<crate::capability::WiremsgStore>,
    /// Phase A ①: wiremsg long-poll の SP 内 in-process 起床機構
    ///
    /// `wire_send` が宛先 agent を notify、 待機中の `wire_recv` を起こす。
    pub wire_notifier: crate::capability::WireNotifier,
    /// Whitesnake 🐍 — 汎用永続化レイヤー
    pub whitesnake: crate::capability::Whitesnake,
    /// Lane Pool (Lead/Wing registry) — Lane scope の Stand container
    /// 関連 memory: mem_1CaSsN7xj69aVQtLPQFJxQ (SP-as-Project-Master 9 component #4)
    pub lane_pool: Arc<RwLock<super::lanes_state::LanePool>>,
    /// Phase 2 (Step E): SP の system 系 lifecycle event を 1 つの broadcast bus で配信。
    /// caller (lane_spawn_actor / routes/lanes / restart_lane / lifecycle monitor) が
    /// `state.system_event_tx.send(SystemEvent::Lane(LaneDiff::*))` 等で publish、
    /// `spawn_registry_keepalive` subscriber が QUIC registry channel で TheWorld に push する経路。
    /// 将来 Pane / Stand / Process 等の lifecycle event も同 bus に variant 追加で乗せる。
    pub system_event_tx: tokio::sync::broadcast::Sender<super::lanes_state::SystemEvent>,
    /// Project scope の Stand pool (PP / GE / HP) — Phase A4-2b minimum、skeleton
    /// 関連 memory: 「多 scope architecture」rule (2026-04-27、 PR-pre2/PR-β-2 で supersede 予定)
    pub project_stands: Arc<RwLock<super::project_stands_state::ProjectStandsPool>>,
    /// World 階層 Stand container (LSCM、 PR-α series / VP-109)。
    ///
    /// World mode (`run_world`) でのみ Some、 SP mode (`run`) では None。
    /// PR-α 完了後も既存 World 階層 field (world / update / whitesnake)
    /// と重複保持 (意図的 HACK、 LSCM A6 share-nothing 整合は β 以降の cleanup PR で整理予定)。
    /// 関連: doc 12 §3 / §9、 Linear VP-109 (epic) / VP-111/112/113/114/115 ✅
    pub world_capabilities: Option<Arc<crate::daemon::world_capabilities::WorldCapabilities>>,
    /// Lane 階層 Stand container pool (LSCM、 PR-δ-2 / VP-136 で PP を `LaneStandRegistry` 経由 host に統一)。
    ///
    /// SP mode (`run`) でのみ Some、 World mode (`run_world`) では None。
    /// PR-β-1 (VP-119) で空 HashMap 受け皿として新設、 PR-β-2 (VP-120) で PP を
    /// `project_stands.paisley_park` から本 pool の各 Lane entry に物理移管。 PR-δ-2 (VP-136) で
    /// `LaneStand` trait + `LaneStandRegistry` 経由 host に進化、 hardcoded field を
    /// trait-based generic interface に置換。 cardinality 1 → N invariant は保持。
    /// 既存 `lane_pool` / `project_stands` とは並立 (gradual migration、 PR-γ で GE も移管予定)。
    /// 関連: doc 12 §9 catalog、 doc 13 §3 / §9 / §10 Q-7、 Linear VP-109 (epic) / VP-119 / VP-120 / VP-135 / VP-136
    pub lane_capabilities: Option<Arc<RwLock<super::lane_capabilities::LaneCapabilitiesPool>>>,
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

// --- VP-13 sub-scope E: Medium 層 route test 用 fixture ---

/// Test 用の minimal AppState builder。 各 field は default / None / in-memory mock で構築、
/// `world` のみ caller が optional に指定 (= 503 path / 200 path 切り替え)。
///
/// 用途: `crates/vantage-point/src/process/routes/` の各 handler を Axum oneshot で
/// smoke test する際の shared fixture。 重い field (vpdb / wiremsg_store / lane_capabilities)
/// は None、 `whitesnake` は `Whitesnake::in_memory()` で軽量化。
///
/// Note: `pub(crate)` のため `crates/vantage-point/src/` 内 inline `#[cfg(test)]` mod
/// からのみ使用可。 integration test (`crates/vantage-point/tests/`) は別 crate なので
/// 不可 (= 必要なら pub 化検討、 PR 4b cleanup philosophy に従い API surface 拡大は控えめに)。
#[cfg(test)]
pub(crate) async fn build_test_app_state(
    world: Option<Arc<RwLock<ProcessManagerCapability>>>,
) -> Arc<AppState> {
    use super::capabilities::CapabilityConfig;
    use super::lane_capabilities::LaneCapabilitiesPool;
    use super::lanes_state::LanePool;
    use super::project_stands_state::ProjectStandsPool;
    use crate::capability::{Whitesnake, WireNotifier};
    use crate::protocol::DebugMode;

    let capabilities = Arc::new(
        ProcessCapabilities::new(CapabilityConfig {
            project_dir: String::new(),
            whitesnake: None,
        })
        .await,
    );

    Arc::new(AppState {
        hub: Hub::new(),
        sessions: Arc::new(RwLock::new(SessionManager::new())),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode: DebugMode::None,
        shutdown_token: CancellationToken::new(),
        project_dir: String::new(),
        project_name: String::new(),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities,
        actor_registry: Arc::new(RwLock::new(ActorRegistry::new())),
        world,
        update: None,
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port: 0,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "test".to_string(),
        tmux: Arc::new(tokio::sync::Mutex::new(None)),
        tmux_session_name: String::new(),
        process_registry: Arc::new(tokio::sync::Mutex::new(ProcessRegistry::new())),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router: Arc::new(TopicRouter::new()),
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb: None,
        wiremsg_store: None,
        wire_notifier: WireNotifier::new(),
        whitesnake: Whitesnake::in_memory(),
        lane_pool: Arc::new(RwLock::new(LanePool::new())),
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        project_stands: Arc::new(RwLock::new(ProjectStandsPool::new())),
        world_capabilities: None,
        lane_capabilities: Some(Arc::new(RwLock::new(LaneCapabilitiesPool::new()))),
    })
}
