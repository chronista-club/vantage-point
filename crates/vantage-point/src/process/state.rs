//! アプリケーション状態モジュール
//!
//! Process サーバーの共有状態と関連型を定義する。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::capabilities::ProcessCapabilities;
use super::hub::Hub;
use super::process_runner::ProcessRegistry;
use super::pty::PtyManager;
use super::session::SessionManager;
use super::topic_router::TopicRouter;
use crate::agent::InteractiveClaudeAgent;
use crate::agui::AgUiEvent;
use crate::capability::{ActorRegistry, ProcessManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::protocol::{Content, DebugMode, ProcessMessage};

/// PP の overall canvas layout を pane_contents に畳む際の reserved pane_id。
/// 通常 pane ではないので restore / pane 一覧から除外する (Whitesnake 退役で導入)。
pub(crate) const CANVAS_LAYOUT_PANE_ID: &str = "__canvas_layout__";

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
    /// chronista-hub federation の接続状態（World mode のみ更新、`/api/health` で vp-app に返す）。
    ///
    /// World mode では [`run_hub_federation`](crate::daemon::hub_client::run_hub_federation) が
    /// 遷移ごとに更新する。SP / test mode では `Disabled` のまま（federation は TheWorld のみ）。
    pub hub_status: crate::daemon::hub_client::HubFederationStatus,
    /// hub registry の available worlds cache（`/api/health` の `hub_worlds` field）。
    ///
    /// World mode では [`run_hub_federation`](crate::daemon::hub_client::run_hub_federation) が
    /// 接続直後 + 定期 discover で更新する（自 world 除外・handle dedup 済、切断で clear）。
    /// SP / test mode では常に空。
    pub hub_worlds: crate::daemon::hub_client::HubWorldsCache,
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
    /// プロセスレジストリ（ProcessRunner）
    pub process_registry: Arc<tokio::sync::Mutex<ProcessRegistry>>,
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
    /// Agent 委譲 (delegation) の World 中央 store (doc 28 §4 / §6)。
    ///
    /// **World mode (`run_world`) でのみ Some**、SP mode (`run`) では None。delegation record は
    /// wire と同じく TheWorld の SurrealDB に中央化 (durable、SP 再起動を跨いで生存、World
    /// reconcile loop の駆動源)。SP の `handle_delegate` 等は `world_wire::call("/api/delegation/*")`
    /// でここに proxy する (wake = SP-local nudge は保持、cf. `process/delegation.rs`)。
    pub delegation_store: Option<crate::capability::DelegationStore>,
}

impl AppState {
    // tmux decoupling PR2: `ensure_tmux` / `primary_tmux_session` / `resolve_lane_session`
    // (TmuxActor 遅延初期化 + LaneAddress ⇄ tmux session 名の翻訳層) は退役。
    // lane の解決は `resolve_lane_address`、 console I/O は PtySlot (deliver_nudge / lane_capture)。

    /// lane address 文字列（`<project>/conductor` / `<project>/performer/<name>`）を、
    /// Running な lane の [`LaneAddress`](super::lanes_state::LaneAddress) に解決する。
    ///
    /// nudge の宛先を `LaneAddress` で返し、
    /// [`deliver_nudge`](super::lanes_state::deliver_nudge) の入力にする。
    /// parse 不能 / lane 不在 / 非 Running なら None。
    pub async fn resolve_lane_address(
        &self,
        query: &str,
    ) -> Option<super::lanes_state::LaneAddress> {
        let addr = super::lanes_state::LanePool::parse_address(query)?;
        let pool = self.lane_pool.read().await;
        let info = pool.get(&addr)?;
        if !matches!(info.state, super::lanes_state::LaneState::Running) {
            return None;
        }
        Some(addr)
    }

    /// 論理 lane address（`agent@<project>[/<name>]` or bare `<project>/<lane>`）を実 tmux
    /// session に解決し、literal text + Enter を送る（= wake）。
    ///
    /// 委譲（`process/delegation.rs`）の `delegate` / `complete` が doer / requester を起こす
    /// 共通 helper。resolution は [`Self::resolve_lane_address`] を介す
    /// （federation 不変条件: `address → local lane` の翻訳層だけが swappable。後で
    /// `world-handle:` 接頭の remote 分岐を足すだけで federation 化できる）。
    ///
    /// tmux decoupling PR1: 旧 `tmux send-keys`（`send_keys_to_session`）を SP-local な
    /// [`deliver_nudge`](super::lanes_state::deliver_nudge) 直書きに置換。 delegation handler は
    /// SP プロセス内で走る（`&AppState` を持つ）ため、 PtySlot に in-process で直接届く
    /// （World-side re-nudge は `lane_nudge` proxy 経由、 同じ `deliver_nudge` sink に収束）。
    ///
    /// 解決できない（lane 不在 / 非 Running / PtySlot 不在）場合は `false` を返し graceful に握る
    /// （no-lane test や実機外で panic しない / wake 取りこぼしは reconcile・pull-hook が後で拾う）。
    pub async fn nudge_lane(&self, addr: &str, text: &str) -> bool {
        let query = super::delegation::lane_query_for(addr);
        let Some(lane_addr) = self.resolve_lane_address(&query).await else {
            return false;
        };
        match super::lanes_state::deliver_nudge(&self.lane_pool, &lane_addr, text).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    "nudge_lane: deliver_nudge 失敗 (addr={addr}, lane={lane_addr}): {e}"
                );
                false
            }
        }
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
                    scope: None,
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
        capabilities,
        actor_registry: Arc::new(RwLock::new(ActorRegistry::new())),
        world,
        update: None,
        hub_status: crate::daemon::hub_client::HubFederationStatus::new(),
        hub_worlds: crate::daemon::hub_client::HubWorldsCache::new(),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port: 0,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "test".to_string(),
        process_registry: Arc::new(tokio::sync::Mutex::new(ProcessRegistry::new())),
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
        // test fixture は SP 相当 (World store 無し)。delegation の store test は
        // capability::delegation_store の単体 test が担う。
        delegation_store: None,
    })
}

#[cfg(test)]
mod lane_resolve_tests {
    use super::build_test_app_state;
    use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};

    /// 指定 lane の Running な LaneInfo を作る test helper
    fn running_lane(addr: LaneAddress, stand: &str) -> LaneInfo {
        LaneInfo {
            console_mode: Default::default(),
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
            flow_state: None,
        }
    }

    /// Running な lane は LaneAddress に解決される（conductor / performer 両方）。
    #[tokio::test]
    async fn resolve_lane_address_returns_running_lane() {
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

        assert_eq!(
            state.resolve_lane_address("vantage-point/conductor").await,
            Some(LaneAddress::conductor("vantage-point"))
        );
        assert_eq!(
            state
                .resolve_lane_address("vantage-point/performer/hub-unison-client")
                .await,
            Some(LaneAddress::performer("vantage-point", "hub-unison-client"))
        );
    }

    /// lane address として parse できない query は None。
    #[tokio::test]
    async fn resolve_lane_address_none_for_non_lane_query() {
        let state = build_test_app_state(None).await;
        assert_eq!(state.resolve_lane_address("%3").await, None);
        assert_eq!(state.resolve_lane_address("some-label").await, None);
    }

    /// Dead lane は nudge 不可なので None。
    #[tokio::test]
    async fn resolve_lane_address_none_for_dead_lane() {
        let state = build_test_app_state(None).await;
        {
            let mut pool = state.lane_pool.write().await;
            let mut info = running_lane(LaneAddress::conductor("vp"), "echoes");
            info.state = LaneState::Dead;
            pool.insert(info);
        }
        assert_eq!(state.resolve_lane_address("vp/conductor").await, None);
    }
}
