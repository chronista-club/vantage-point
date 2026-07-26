//! アプリケーション状態モジュール
//!
//! Process サーバーの共有状態と関連型を定義する。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::capabilities::RepoCapabilities;
use super::hub::Hub;
use super::process_runner::ProcessRegistry;
use super::topic_router::TopicRouter;
use crate::agent::InteractiveClaudeAgent;
use crate::agui::AgUiEvent;
use crate::capability::{ActorRegistry, RepoManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::protocol::{Content, RepoMessage};

/// board の overall canvas layout を pane_contents に畳む際の reserved pane_id。
/// 通常 pane ではないので restore / pane 一覧から除外する (旧永続化レイヤー退役で導入)。
pub(crate) const CANVAS_LAYOUT_PANE_ID: &str = "__canvas_layout__";

/// demand-driven terminal pump の台帳（doc 50 §4.6 A6 → doc 53 R2 で reconcile 化）。
/// 実体定義は pump module 側（`terminal_pump::TerminalPumps` — entry は JoinHandle に加え
/// 照合キー `slot_pid` を持つ）。`pty_slots` と対称の入れ子。
type TerminalPumps = super::terminal_pump::TerminalPumps;

/// Application state
pub(crate) struct AppState {
    pub hub: Hub,
    /// Shutdown signal token
    pub shutdown_token: CancellationToken,
    /// Repo directory for Claude agent
    pub repo_dir: String,
    /// 解決済 repo 名 (config の `repos[].name`、 未登録ならディレクトリ名)
    ///
    /// R3 (wire cross-process delivery): `wire_send` の宛先分類で「自 repo の repo か否か」を
    /// 判定するのに使う。 `agent@<repo>` の `<repo>` が本 field と異なれば remote repo。
    /// daemon mode では空文字列 (= cross-process forward は repo mode 専用)。
    pub repo_name: String,
    /// Capability system (Agent, MIDI, Protocol)
    pub capabilities: Arc<RepoCapabilities>,
    /// VP-159 PR-4b: Agent / Service actor の supervisor 受け皿。
    ///
    /// repo mode で notify / lane-spawn を `spawn_service` 経由で起動・register、 JoinHandle を保持。
    /// daemon mode では空で構築 (= machine scope actor の register は後続 PR、 device registry の
    /// metadata register は dynamic routing vision 確定後、 cf. design-spark `mem_1CavFi5D1aMSpEkas89SvQ`)。
    /// PR-5 supervisor 統一で JoinHandle 経由の abort / await を activate する foundation。
    pub actor_registry: Arc<RwLock<ActorRegistry>>,
    /// Daemon capability for managing multiple processes (optional, only for daemon mode)
    pub daemon: Option<Arc<RwLock<RepoManagerCapability>>>,
    /// Update capability for version checking (optional, only for daemon mode)
    pub update: Option<Arc<RwLock<UpdateCapability>>>,
    /// chronista-hub federation の接続状態（daemon mode のみ更新、`/api/health` で vp-app に返す）。
    ///
    /// daemon mode では [`run_hub_federation`](crate::daemon::hub_client::run_hub_federation) が
    /// 遷移ごとに更新する。repo / test mode では `Disabled` のまま（federation は daemon のみ）。
    pub hub_status: crate::daemon::hub_client::HubFederationStatus,
    /// hub registry の available nodes cache（`/api/health` の `hub_nodes` field）。
    ///
    /// daemon mode では [`run_hub_federation`](crate::daemon::hub_client::run_hub_federation) が
    /// 接続直後 + 定期 discover で更新する（自 daemon 除外・handle dedup 済、切断で clear）。
    /// repo / test mode では常に空。
    pub hub_nodes: crate::daemon::hub_client::HubNodesCache,
    /// Interactive Claude agent (stream-json mode for structured communication)
    pub interactive_agent: Arc<RwLock<Option<InteractiveClaudeAgent>>>,
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
    /// wiremsg R5-3: 旧 `msgbox_store` (msgs table) は撤去済。
    /// msg messaging はこの wiremsg store に一本化。
    pub wiremsg_store: Option<crate::capability::WiremsgStore>,
    /// Phase A ①: wiremsg long-poll の repo 内 in-process 起床機構
    ///
    /// `wire_send` が宛先 agent を notify、 待機中の `wire_recv` を起こす。
    pub wire_notifier: crate::capability::WireNotifier,
    /// R2-b: wire delivery loop の即時 wake (command 着信時に notify)。
    /// daemon mode でのみ DeliveryActor が待ち受ける。 repo では未使用 (proxy が daemon に送るだけ)。
    pub delivery_notify: Arc<tokio::sync::Notify>,
    /// Lane Pool (Conductor/Performer registry) — Lane scope の Agent container
    /// 関連 memory: mem_1CaSsN7xj69aVQtLPQFJxQ (repo-as-Repo-Master 9 component #4)
    pub lane_pool: Arc<RwLock<super::lanes_state::LanePool>>,
    /// Phase 2 (Step E): repo の system 系 lifecycle event を 1 つの broadcast bus で配信。
    /// caller (lane_spawn_actor / routes/lanes / restart_lane / lifecycle monitor) が
    /// `state.system_event_tx.send(SystemEvent::Lane(LaneDiff::*))` 等で publish、
    /// repo の lanes publish task (`publish_lanes`) が subscribe して daemon の集約 view を
    /// 更新する経路（doc 44 P1 fold-in で旧 `spawn_daemon_uplink` の QUIC push から置換）。
    /// 将来 Pane / Agent / Process 等の lifecycle event も同 bus に variant 追加で乗せる。
    pub system_event_tx: tokio::sync::broadcast::Sender<super::lanes_state::SystemEvent>,
    /// machine 階層 Agent container (LSCM、 PR-α series / VP-109)。
    ///
    /// daemon mode (`run_daemon`) でのみ Some、 repo mode (`run`) では None。
    /// PR-α 完了後も既存 machine 階層 field (daemon / update)
    /// と重複保持 (意図的 HACK、 LSCM A6 share-nothing 整合は β 以降の cleanup PR で整理予定)。
    /// 関連: doc 12 §3 / §9、 Linear VP-109 (epic) / VP-111/112/113/114/115 ✅
    pub machine_capabilities: Option<Arc<crate::daemon::machine_capabilities::MachineCapabilities>>,
    /// Lane 階層 Agent container pool (LSCM、 PR-δ-2 / VP-136 で board を `LaneStandRegistry` 経由 host に統一)。
    ///
    /// repo mode (`run`) でのみ Some、 daemon mode (`run_daemon`) では None。
    /// PR-β-1 (VP-119) で空 HashMap 受け皿として新設、 PR-β-2 (VP-120) で board を
    /// `repo_stands.board` から本 pool の各 Lane entry に物理移管。 PR-δ-2 (VP-136) で
    /// `LaneStand` trait + `LaneStandRegistry` 経由 host に進化、 hardcoded field を
    /// trait-based generic interface に置換。 cardinality 1 → N invariant は保持。
    /// 既存 `lane_pool` / `repo_stands` とは並立 (gradual migration、 PR-γ で runner も移管予定)。
    /// 関連: doc 12 §9 catalog、 doc 13 §3 / §9 / §10 Q-7、 Linear VP-109 (epic) / VP-119 / VP-120 / VP-135 / VP-136
    pub lane_capabilities: Option<Arc<RwLock<super::lane_capabilities::LaneCapabilitiesPool>>>,
    /// S2 (doc 27 §4.1): demand-driven terminal pump の lane → session → JoinHandle map。
    ///
    /// daemon の demand hook が control reverse-route で `terminal_demand_start {lane}` を撃つと、
    /// repo は当該 Lane の**各 session** の PtySlot output を購読する pump を session ごとに spawn し、
    /// 本 map に保持する (`repo/terminal/data/{lane}/out` topic に route、session は message
    /// field で運ぶ = doc 50 §4.6 A6 Design B)。 `terminal_demand_stop {lane}` で lane の全 pump を
    /// abort して除去する (= 購読者が居る間だけ pump を回す lazy production)。
    /// 外側 key は LaneAddress の Display 形 (`"<repo>/root"` 等)、内側 key は session key。
    /// `pty_slots` と対称の入れ子（lane 単位の teardown と session 単位の付け替えを両立）。
    pub terminal_pumps: Arc<RwLock<TerminalPumps>>,
    /// Agent 委譲 (delegation) の daemon 中央 store (doc 28 §4 / §6)。
    ///
    /// **daemon mode (`run_daemon`) でのみ Some**、repo mode (`run`) では None。delegation record は
    /// wire と同じく daemon の SurrealDB に中央化 (durable、repo 再起動を跨いで生存、Daemon
    /// reconcile loop の駆動源)。repo の `handle_delegate` 等は `daemon_wire::call("/api/delegation/*")`
    /// でここに proxy する (wake = repo-local nudge は保持、cf. `repo/delegation.rs`)。
    pub delegation_store: Option<crate::capability::DelegationStore>,
    /// doc 48 Phase 2: editor bridge の pending 応答 map (request_id → oneshot)。
    ///
    /// `handle_editor_command` が登録して `EditorCommand` を broadcast、GUI からの
    /// `editor_result` (`handle_editor_result`) が解決する。timeout 時は登録側が
    /// remove するので、遅延到着した stale 応答は不在 key として無視される (idempotent)。
    pub editor_pending:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
}

impl AppState {
    // tmux decoupling PR2: `ensure_tmux` / `primary_tmux_session` / `resolve_lane_session`
    // (TmuxActor 遅延初期化 + LaneAddress ⇄ tmux session 名の翻訳層) は退役。
    // lane の解決は `resolve_lane_address`、 console I/O は PtySlot (deliver_nudge / lane_capture)。

    /// lane address 文字列（`<repo>/root` / `<repo>/performer/<name>`）を、
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

    /// 論理 lane address（`agent@<repo>[/<name>]` or bare `<repo>/<lane>`）を実 tmux
    /// session に解決し、literal text + Enter を送る（= wake）。
    ///
    /// 委譲（`repo/delegation.rs`）の `delegate` / `complete` が doer / requester を起こす
    /// 共通 helper。resolution は [`Self::resolve_lane_address`] を介す
    /// （federation 不変条件: `address → local lane` の翻訳層だけが swappable。後で
    /// `daemon-handle:` 接頭の remote 分岐を足すだけで federation 化できる）。
    ///
    /// tmux decoupling PR1: 旧 `tmux send-keys`（`send_keys_to_session`）を repo-local な
    /// [`deliver_nudge`](super::lanes_state::deliver_nudge) 直書きに置換。 delegation handler は
    /// repo プロセス内で走る（`&AppState` を持つ）ため、 PtySlot に in-process で直接届く
    /// （Daemon-side re-nudge は `lane_nudge` proxy 経由、 同じ `deliver_nudge` sink に収束）。
    ///
    /// 解決できない（lane 不在 / 非 Running / PtySlot 不在）場合は `false` を返し graceful に握る
    /// （no-lane test や実機外で panic しない / wake 取りこぼしは reconcile・pull-hook が後で拾う）。
    pub async fn nudge_lane(&self, addr: &str, text: &str) -> bool {
        let query = super::delegation::lane_query_for(addr);
        let Some(lane_addr) = self.resolve_lane_address(&query).await else {
            return false;
        };
        // session=None = root（wire mailbox `agent@<lane>` を名乗るのは root、doc 39/46 P5）。
        match super::lanes_state::deliver_nudge(&self.lane_pool, &lane_addr, None, text).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    "nudge_lane: deliver_nudge 失敗 (addr={addr}, lane={lane_addr}): {e}"
                );
                false
            }
        }
    }

    /// Send AG-UI event to connected clients (REQ-AGUI-040)
    // 要確認（audit 2026-07-18、先行実装の可能性）: AG-UI protocol の先行 API（REQ-AGUI-040）。未 call。
    #[allow(dead_code)]
    pub fn send_agui_event(&self, event: AgUiEvent) {
        self.hub.broadcast(RepoMessage::AgUi { event });
    }

    // =========================================================================
    // ペイン状態永続化（pane_contents / SurrealDB 経由、 旧 file-backed DISC 層は退役）
    // =========================================================================

    /// pane_contents (SurrealDB) から board pane 状態を RetainedStore に boot 復元する。
    ///
    /// 旧 DISC 層退役 → canonical な pane_contents を直接読む。 webview 自身は
    /// board state ask（repo-proxy 経由、旧 `/api/pp/state`）で state を読むが、 retained `show` topic を購読する経路 (MCP show 等)
    /// のため boot で RetainedStore も埋める (旧挙動保存)。 旧 DISC restore と同じく
    /// **conductor scope のみ**復元する (performer は webview が lane 切替時に board state ask で読む)。
    /// reserved な canvas-layout row は pane ではないので除外。
    pub async fn restore_pane_contents(&self) {
        let Some(vpdb) = self.vpdb.as_ref() else {
            return;
        };
        let rows = match vpdb.list_pane_contents(&self.repo_dir).await {
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
            // image_base64 は data/mime を content 1 列に畳めず board Canvas でも現状未使用
            // (mcp.rs: image_base64 は content 空文字保存) なので markdown fallback で可
            // (旧 DISC 層も実経路では同等の dead path)。
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
            let topic = format!("repo/board/command/show/root/{}", pane_id);
            store.set(
                &topic,
                RepoMessage::Show {
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

    // doc 45 段 4: `save_canvas_layout` / `load_canvas_layout` は撤去。
    // 呼び出し元は `/api/canvas/layout` の 2 handler だけで、その route ごと落ちたため
    // 読み手も書き手も居なくなった（doc 45 §3.1 — end-to-end で dead）。
    // [`CANVAS_LAYOUT_PANE_ID`] 自体は残す: 過去に書かれた reserved row が db に残っており、
    // `restore_panes` が pane 一覧から除外し続ける必要がある。
}

// --- VP-13 sub-scope E: Medium 層 route test 用 fixture ---

/// Test 用の minimal AppState builder。 各 field は default / None / in-memory mock で構築、
/// `daemon` のみ caller が optional に指定 (= 503 path / 200 path 切り替え)。
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
    daemon: Option<Arc<RwLock<RepoManagerCapability>>>,
) -> Arc<AppState> {
    build_test_app_state_with("", None, daemon).await
}

/// `build_test_app_state` の `repo_dir` / `vpdb` を差せる版。
///
/// lane 作成の intent-first bracket（descriptor + lifecycle の永続、doc 44 §9.4）のように
/// **db への書き込みが振る舞いの一部**になっている経路は、vpdb=None の fixture では
/// 「書けたつもり」を素通りさせてしまう（= guard never-fire と同型の緑）。そこだけ
/// 実 db（in-memory surrealkv）を差せるようにする。
#[cfg(test)]
pub(crate) async fn build_test_app_state_with(
    repo_dir: &str,
    vpdb: Option<crate::db::SharedVpDb>,
    daemon: Option<Arc<RwLock<RepoManagerCapability>>>,
) -> Arc<AppState> {
    use super::capabilities::CapabilityConfig;
    use super::lane_capabilities::LaneCapabilitiesPool;
    use super::lanes_state::LanePool;
    use crate::capability::WireNotifier;

    let capabilities = Arc::new(
        RepoCapabilities::new(CapabilityConfig {
            repo_dir: repo_dir.to_string(),
        })
        .await,
    );

    Arc::new(AppState {
        hub: Hub::new(),
        shutdown_token: CancellationToken::new(),
        repo_dir: repo_dir.to_string(),
        repo_name: String::new(),
        capabilities,
        actor_registry: Arc::new(RwLock::new(ActorRegistry::new())),
        daemon,
        update: None,
        hub_status: crate::daemon::hub_client::HubFederationStatus::new(),
        hub_nodes: crate::daemon::hub_client::HubNodesCache::new(),
        interactive_agent: Arc::new(RwLock::new(None)),
        port: 0,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "test".to_string(),
        process_registry: Arc::new(tokio::sync::Mutex::new(ProcessRegistry::new())),
        topic_router: Arc::new(TopicRouter::new()),
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        vpdb,
        wiremsg_store: None,
        wire_notifier: WireNotifier::new(),
        delivery_notify: Arc::new(tokio::sync::Notify::new()),
        lane_pool: Arc::new(RwLock::new(LanePool::new())),
        system_event_tx: tokio::sync::broadcast::channel::<super::lanes_state::SystemEvent>(64).0,
        machine_capabilities: None,
        lane_capabilities: Some(Arc::new(RwLock::new(LaneCapabilitiesPool::new()))),
        terminal_pumps: Arc::new(RwLock::new(HashMap::new())),
        // test fixture は repo 相当 (Daemon store 無し)。delegation の store test は
        // capability::delegation_store の単体 test が担う。
        delegation_store: None,
        editor_pending: Default::default(),
    })
}

#[cfg(test)]
mod lane_resolve_tests {
    use super::build_test_app_state;
    use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};

    /// 指定 lane の Running な LaneInfo を作る test helper
    fn running_lane(addr: LaneAddress, agent: &str) -> LaneInfo {
        LaneInfo {
            id: Default::default(),
            address: addr.clone(),
            state: LaneState::Running,
            agent: agent.to_string(),
            created_at: "2026-06-16T00:00:00Z".to_string(),
            pid: Some(1234),
            cwd: "/tmp/work".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        }
    }

    /// Running な lane は LaneAddress に解決される（conductor / performer 両方）。
    #[tokio::test]
    async fn resolve_lane_address_returns_running_lane() {
        let state = build_test_app_state(None).await;
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(running_lane(LaneAddress::root("vantage-point"), "claude"));
            pool.insert(running_lane(
                LaneAddress::performer("vantage-point", "hub-unison-client"),
                "claude",
            ));
        }

        assert_eq!(
            state.resolve_lane_address("vantage-point/root").await,
            Some(LaneAddress::root("vantage-point"))
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
            let mut info = running_lane(LaneAddress::root("vp"), "claude");
            info.state = LaneState::Dead;
            pool.insert(info);
        }
        assert_eq!(state.resolve_lane_address("vp/root").await, None);
    }
}
