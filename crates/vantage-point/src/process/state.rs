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
use crate::protocol::{Content, DebugMode, ProcessMessage};

/// PP の overall canvas layout を pane_contents に畳む際の reserved pane_id。
/// 通常 pane ではないので restore / pane 一覧から除外する (Whitesnake 退役で導入)。
pub(crate) const CANVAS_LAYOUT_PANE_ID: &str = "__canvas_layout__";

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
    ///
    /// 束縛 session は固定値ではなく `primary_tmux_session()` で lane_pool から動的解決する
    /// (旧: `{project}-vp` 固定 → lane scheme `vp-{project}-{lane}-{stand}` と不一致で全 op が
    /// 「tmux 未使用環境です」になる bug の根治、 fix-tmux-session-naming)。
    pub tmux: Arc<tokio::sync::Mutex<Option<TmuxHandle>>>,
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
    /// R2-b: wire delivery loop の即時 wake (command 着信時に notify)。
    /// World mode でのみ DeliveryActor が待ち受ける。 SP では未使用 (proxy が TheWorld に送るだけ)。
    pub delivery_notify: Arc<tokio::sync::Notify>,
    /// Lane Pool (Conductor/Performer registry) — Lane scope の Stand container
    /// 関連 memory: mem_1CaSsN7xj69aVQtLPQFJxQ (SP-as-Project-Master 9 component #4)
    pub lane_pool: Arc<RwLock<super::lanes_state::LanePool>>,
    /// Phase 2 (Step E): SP の system 系 lifecycle event を 1 つの broadcast bus で配信。
    /// caller (lane_spawn_actor / routes/lanes / restart_lane / lifecycle monitor) が
    /// `state.system_event_tx.send(SystemEvent::Lane(LaneDiff::*))` 等で publish、
    /// `spawn_world_uplink` subscriber が QUIC registry channel で TheWorld に push する経路。
    /// 将来 Pane / Stand / Process 等の lifecycle event も同 bus に variant 追加で乗せる。
    pub system_event_tx: tokio::sync::broadcast::Sender<super::lanes_state::SystemEvent>,
    /// Project scope の Stand pool (PP / GE / HP) — Phase A4-2b minimum、skeleton
    /// 関連 memory: 「多 scope architecture」rule (2026-04-27、 PR-pre2/PR-β-2 で supersede 予定)
    pub project_stands: Arc<RwLock<super::project_stands_state::ProjectStandsPool>>,
    /// World 階層 Stand container (LSCM、 PR-α series / VP-109)。
    ///
    /// World mode (`run_world`) でのみ Some、 SP mode (`run`) では None。
    /// PR-α 完了後も既存 World 階層 field (world / update)
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
    /// S2 (doc 27 §4.1): demand-driven terminal pump の lane → JoinHandle map。
    ///
    /// World の demand hook が control reverse-route で `terminal_demand_start {lane}` を撃つと、
    /// SP は当該 Lane の PtySlot output を購読する pump を spawn して本 map に保持する
    /// (`process/terminal/data/{lane}/out` topic に route)。 `terminal_demand_stop {lane}` で
    /// abort して除去する (= 購読者が居る間だけ pump を回す lazy production)。
    /// key は LaneAddress の Display 形 (`"<project>/conductor"` 等)。
    pub terminal_pumps: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl AppState {
    /// tmux ハンドルを取得（遅延初期化: 未接続なら tmux セッションを検索して起動）
    ///
    /// 束縛 session は lane_pool から動的解決する（`primary_tmux_session()`）。 SP は固定の
    /// 自前 session を持たず、 conductor lane の実 session を primary 舞台として借りる。
    /// split / list / capture_all 等の session-scoped op はこの primary session を対象にする。
    /// send-keys / capture(単一) / close は global な pane/session target で動くので、
    /// 束縛 session が何であっても正しく届く（gate を通すためだけに 1 つ存在すれば良い）。
    pub async fn ensure_tmux(&self) -> Option<TmuxHandle> {
        // 既存ハンドルがあれば即返す（lock は最小区間で解放してから次に進む）。
        {
            let guard = self.tmux.lock().await;
            if let Some(ref handle) = *guard {
                return Some(handle.clone());
            }
        }

        if !crate::tmux::is_tmux_available() {
            return None;
        }

        // primary session を lane_pool から解決 (= VP 管理 session が 1 つでもあれば actor 起動)。
        // tmux Mutex を保持したまま lane_pool RwLock を取らない（lock 取得順を直交させ、
        // 将来の逆順取得によるデッドロック余地を残さない）。
        let session = self.primary_tmux_session().await?;

        let mut guard = self.tmux.lock().await;
        // double-check: lock を一度手放した間に別 task が初期化済みかもしれない。
        if let Some(ref handle) = *guard {
            return Some(handle.clone());
        }
        if crate::tmux::session_exists(&session)
            && let Some(handle) = super::tmux_actor::spawn_for_session(&session)
        {
            *guard = Some(handle.clone());
            tracing::info!("TmuxActor 遅延初期化: session={}", session);
            return Some(handle);
        }

        None
    }

    /// SP が借りる primary tmux session を lane_pool から解決する。
    ///
    /// conductor lane の実 session（`LaneInfo.tmux[].session`、 spawn 時 populate）を優先し、
    /// 無ければ任意の tmux entry を持つ lane の session に fallback する。
    /// 該当 lane が無ければ None（= tmux 不使用扱い、 World mode 含む）。
    ///
    /// Running な lane のみ対象にする（`resolve_lane_session` と一貫）。 Dead lane は
    /// `LaneInfo.tmux` を clear しない経路があり（`kill_by_pid` 等）、 filter が無いと Dead
    /// conductor の session 名が Running performer より優先される意図しない順序が生じうる。
    pub async fn primary_tmux_session(&self) -> Option<String> {
        use super::lanes_state::{LaneKind, LaneState};
        let pool = self.lane_pool.read().await;
        let lanes = pool.list();
        // Running な conductor 優先 → 無ければ Running な任意 lane の tmux entry
        lanes
            .iter()
            .find(|l| l.kind == LaneKind::Conductor && l.state == LaneState::Running)
            .and_then(|l| l.tmux.first())
            .or_else(|| {
                lanes.iter().find_map(|l| {
                    (l.state == LaneState::Running)
                        .then(|| l.tmux.first())
                        .flatten()
                })
            })
            .map(|t| t.session.clone())
    }

    /// lane address 文字列（`<project>/conductor` / `<project>/performer/<name>`）を
    /// その lane の実 tmux session 名に解決する。
    ///
    /// nudge / resolve-pane の宛先解決に使う。 lane_pool に Running な lane があれば
    /// `tmux[0].session`（spawn 時の実 session）を返す。 lane address として parse できない、
    /// もしくは該当 lane が無い / tmux 不在なら None。
    ///
    /// `tmux send-keys -t <session>` は session の active pane に届くため、 pane_id や
    /// agent_metadata、 単一 TmuxActor の束縛 session を一切介さずに任意 lane へ届く。
    pub async fn resolve_lane_session(&self, query: &str) -> Option<String> {
        let addr = super::lanes_state::LanePool::parse_address(query)?;
        let pool = self.lane_pool.read().await;
        let info = pool.get(&addr)?;
        if !matches!(info.state, super::lanes_state::LaneState::Running) {
            return None;
        }
        info.tmux.first().map(|t| t.session.clone())
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
    // ペイン状態永続化（pane_contents / SurrealDB 経由、 旧 Whitesnake 退役）
    // =========================================================================

    /// pane_contents (SurrealDB) から PP pane 状態を RetainedStore に boot 復元する。
    ///
    /// 旧 Whitesnake DISC 退役 → canonical な pane_contents を直接読む。 webview 自身は
    /// `/api/pp/state` GET で state を読むが、 retained `show` topic を購読する経路 (MCP show 等)
    /// のため boot で RetainedStore も埋める (旧挙動保存)。 旧 Whitesnake restore と同じく
    /// **conductor scope のみ**復元する (performer は webview が lane 切替時に /api/pp/state で読む)。
    /// reserved な canvas-layout row は pane ではないので除外。
    pub async fn restore_pane_contents(&self) {
        let Some(vpdb) = self.vpdb.as_ref() else {
            return;
        };
        let rows = match vpdb.list_pane_contents(&self.project_dir).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("pane_contents 読み出し失敗: {}", e);
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let retained = self.topic_router.retained();
        let mut store = retained.write().await;
        let mut count = 0;
        for row in &rows {
            let pane_id = row.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
            // lane_name '' sentinel = conductor。 conductor のみ復元 (旧挙動)。
            let lane_name = row.get("lane_name").and_then(|v| v.as_str()).unwrap_or("");
            if pane_id.is_empty() || pane_id == CANVAS_LAYOUT_PANE_ID || !lane_name.is_empty() {
                continue;
            }
            // pane_contents は content_type(str)+content(str) で持つ → Content enum に組み直す。
            // image_base64 は data/mime を content 1 列に畳めず PP Canvas でも現状未使用
            // (mcp.rs: image_base64 は content 空文字保存) なので markdown fallback で可
            // (旧 Whitesnake も実経路では同等の dead path)。
            let content_str = row
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = match row.get("content_type").and_then(|v| v.as_str()) {
                Some("html") => Content::Html(content_str),
                Some("log") => Content::Log(content_str),
                Some("url") => Content::Url(content_str),
                _ => Content::Markdown(content_str),
            };
            let title = row
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let topic = format!("process/paisley-park/command/show/conductor/{}", pane_id);
            store.set(
                &topic,
                ProcessMessage::Show {
                    pane_id: pane_id.to_string(),
                    content,
                    append: false,
                    title,
                    lane: None,
                },
            );
            count += 1;
        }
        if count > 0 {
            tracing::info!(
                "ペイン状態を pane_contents から復元: {} ペイン (port={})",
                count,
                self.port
            );
        }
    }

    /// Canvas レイアウト状態を pane_contents の reserved row に保存する。
    ///
    /// 旧 Whitesnake 退役 → SurrealDB 一本化。 layout は lane 非依存の単一 row
    /// (lane=conductor, pane_id=[`CANVAS_LAYOUT_PANE_ID`]、 pane 一覧には現れない reserved key)。
    pub async fn save_canvas_layout(&self, layout: &serde_json::Value) {
        let Some(vpdb) = self.vpdb.as_ref() else {
            return;
        };
        let content = serde_json::to_string(layout).unwrap_or_else(|_| "{}".to_string());
        if let Err(e) = vpdb
            .upsert_pp_state(
                &self.project_dir,
                None,
                CANVAS_LAYOUT_PANE_ID,
                "canvas-layout",
                &content,
                None,
                None,
                None,
            )
            .await
        {
            tracing::warn!("canvas layout 保存に失敗: {}", e);
        }
    }

    /// Canvas レイアウト状態を pane_contents の reserved row から復元する。
    pub async fn load_canvas_layout(&self) -> Option<serde_json::Value> {
        let vpdb = self.vpdb.as_ref()?;
        match vpdb
            .load_pp_state(&self.project_dir, None, CANVAS_LAYOUT_PANE_ID)
            .await
        {
            Ok(Some(row)) => row
                .get("content")
                .and_then(|c| c.as_str())
                .and_then(|s| serde_json::from_str(s).ok()),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("canvas layout 読み出しに失敗: {}", e);
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
/// は None で軽量化。
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
    use crate::capability::WireNotifier;
    use crate::protocol::DebugMode;

    let capabilities = Arc::new(
        ProcessCapabilities::new(CapabilityConfig {
            project_dir: String::new(),
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
        process_registry: Arc::new(tokio::sync::Mutex::new(ProcessRegistry::new())),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router: Arc::new(TopicRouter::new()),
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb: None,
        wiremsg_store: None,
        wire_notifier: WireNotifier::new(),
        delivery_notify: Arc::new(tokio::sync::Notify::new()),
        lane_pool: Arc::new(RwLock::new(LanePool::new())),
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        project_stands: Arc::new(RwLock::new(ProjectStandsPool::new())),
        world_capabilities: None,
        lane_capabilities: Some(Arc::new(RwLock::new(LaneCapabilitiesPool::new()))),
        terminal_pumps: Arc::new(RwLock::new(HashMap::new())),
    })
}

#[cfg(test)]
mod tmux_session_resolve_tests {
    use super::build_test_app_state;
    use crate::process::lanes_state::{
        LaneAddress, LaneInfo, LaneState, TmuxLaneAddress, TmuxMode,
    };

    /// 指定 lane の Running + tmux session 入り LaneInfo を作る test helper
    fn running_lane(addr: LaneAddress, stand: &str) -> LaneInfo {
        let session = addr.tmux_session_name(stand);
        LaneInfo {
            id: Default::default(),
            address: addr.clone(),
            kind: addr.kind,
            name: addr.name.clone(),
            state: LaneState::Running,
            stand: stand.to_string(),
            created_at: "2026-06-16T00:00:00Z".to_string(),
            pid: Some(1234),
            cwd: "/tmp/work".to_string(),
            performer_status: None,
            cc_session_id: None,
            tmux: vec![TmuxLaneAddress {
                stand: stand.to_string(),
                session,
                mode: TmuxMode::Tmux,
            }],
        }
    }

    /// lane address が実 session（lane scheme）に解決される。 legacy `{project}-vp` ではない。
    #[tokio::test]
    async fn resolve_lane_session_returns_real_lane_scheme_session() {
        let state = build_test_app_state(None).await;
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(running_lane(
                LaneAddress::conductor("vantage-point"),
                "echoes",
            ));
            pool.insert(running_lane(
                LaneAddress::performer("vantage-point", "hub-unison-client"),
                "echoes",
            ));
        }

        // conductor
        assert_eq!(
            state.resolve_lane_session("vantage-point/conductor").await,
            Some("vp-vantage-point-conductor-echoes".to_string())
        );
        // performer（受け入れ条件の dogfood 対象）
        assert_eq!(
            state
                .resolve_lane_session("vantage-point/performer/hub-unison-client")
                .await,
            Some("vp-vantage-point-hub-unison-client-echoes".to_string())
        );
    }

    /// lane address として parse できない query は None（従来 pane_id / label 経路に fallback）
    #[tokio::test]
    async fn resolve_lane_session_none_for_non_lane_query() {
        let state = build_test_app_state(None).await;
        assert_eq!(state.resolve_lane_session("%3").await, None);
        assert_eq!(state.resolve_lane_session("some-label").await, None);
    }

    /// Dead lane は nudge 不可なので None
    #[tokio::test]
    async fn resolve_lane_session_none_for_dead_lane() {
        let state = build_test_app_state(None).await;
        {
            let mut pool = state.lane_pool.write().await;
            let mut info = running_lane(LaneAddress::conductor("vp"), "echoes");
            info.state = LaneState::Dead;
            info.tmux.clear();
            pool.insert(info);
        }
        assert_eq!(state.resolve_lane_session("vp/conductor").await, None);
    }

    /// primary session は conductor lane を優先する
    #[tokio::test]
    async fn primary_tmux_session_prefers_conductor() {
        let state = build_test_app_state(None).await;
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(running_lane(
                LaneAddress::performer("vp", "perf-a"),
                "echoes",
            ));
            pool.insert(running_lane(LaneAddress::conductor("vp"), "echoes"));
        }
        assert_eq!(
            state.primary_tmux_session().await,
            Some("vp-vp-conductor-echoes".to_string())
        );
    }

    /// Dead conductor（tmux entry は stale で残置）より Running performer を優先する（Issue #1）
    #[tokio::test]
    async fn primary_tmux_session_skips_dead_conductor() {
        let state = build_test_app_state(None).await;
        {
            let mut pool = state.lane_pool.write().await;
            // Dead conductor だが tmux entry は clear されず残っている状況を再現
            let mut dead_conductor = running_lane(LaneAddress::conductor("vp"), "echoes");
            dead_conductor.state = LaneState::Dead;
            pool.insert(dead_conductor);
            pool.insert(running_lane(
                LaneAddress::performer("vp", "perf-a"),
                "echoes",
            ));
        }
        // Dead conductor を飛ばして Running performer の session が返る
        assert_eq!(
            state.primary_tmux_session().await,
            Some("vp-vp-perf-a-echoes".to_string())
        );
    }

    /// conductor が無ければ任意の tmux lane に fallback、 lane 皆無なら None
    #[tokio::test]
    async fn primary_tmux_session_fallback_and_empty() {
        let state = build_test_app_state(None).await;
        assert_eq!(state.primary_tmux_session().await, None);
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(running_lane(
                LaneAddress::performer("vp", "perf-a"),
                "echoes",
            ));
        }
        assert_eq!(
            state.primary_tmux_session().await,
            Some("vp-vp-perf-a-echoes".to_string())
        );
    }
}
