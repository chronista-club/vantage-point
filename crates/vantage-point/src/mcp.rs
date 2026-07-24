//! MCP (Model Context Protocol) server implementation
//!
//! Provides tools for Claude Code to display content in browser:
//! - show: Display markdown/html/log content
//! - clear: Clear a pane
//!
//! ## 通信レイヤー
//! process チャネルは Unison QUIC で通信。
//! Ruby VM / capture 等の一部 API は HTTP フォールバック。

// running.json 不使用 — discovery モジュール経由
use rmcp::{
    ErrorData as McpError, ServiceExt, handler::server::tool::ToolRouter, model::*,
    schemars::JsonSchema, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::protocol::ProcessMessage;

// rebuild Epic L2 第二手（doc 27 §5-1/§5-2）: wire/delegation family の MCP tool は
// schema/vp-agent.kdl を SSOT に生成（cargo test -p vantage-point --test agent_tools_codegen）。
// 生成された #[tool_router(router = agent_tool_router)] impl VantageMcp を constructor で
// 手書き router と `Self::tool_router() + Self::agent_tool_router()` で合流する。
// complete / wire_recv の imperative 本体だけは下の *_impl に手書きで残し、生成 wrapper が委譲する。
#[path = "generated/agent_tools.rs"]
mod agent_tools_generated;
use agent_tools_generated::{CompleteParams, WireRecvParams, WireSendParams};

mod canvas;
mod editor;
mod lane;
mod layout;

/// Parameters for the restart tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RestartParams {
    /// Whether to open WebView after restart (default: false for headless)
    #[schemars(description = "Open WebView window after restart (default: false)")]
    pub open_viewer: Option<bool>,
}

// doc 52 §7: CanvasPane / parse_show_payload は list_canvas / read_pane（死んだ読み手）と共に撤去。
// board モデル化（2026-07-15）で show は board 経路に intercept され、retained Show を broadcast
// する者が消えた → これらは常に空を読む reader-without-writer だった。board を読む口は §4 で別途新設。

/// MCP → Process 通信クライアント
///
/// この `vp mcp` プロセスが属する Lane (VP-166 PR-4)。
///
/// cwd から判定する: cwd が `<repo>/.vp/lanes/<name>` 以下なら performer `<name>`、
/// それ以外（= repo path）なら conductor。`wire_send` / `wire_recv` の `from` 導出 /
/// `list_lanes` の `is_self` 付与に使う。
///
/// project-local lane refactor PR 2: 旧 `<performers_dir>/<parent>-<name>`
/// (global path + repo prefix) detection は撤去。 PR 1 で performer 配置が
/// `<repo>/.vp/lanes/<name>` に移行し、 legacy global path は user の mv 後
/// empty。 PR 4 で legacy 関連 code 全削除予定なので、 ここも先行して
/// project-local 一本に揃える。
#[derive(Debug, Clone)]
pub struct SelfLane {
    /// `"root"` or `"<performer-name>"`（flat 名）
    pub lane_name: String,
    /// performer context のとき `Some(parent project 名)`、conductor context のとき `None`
    pub performer_parent: Option<String>,
    /// conductor context のとき、cwd から config-only で解決した自 project 名
    /// (`Some` = `agent@<project>` の canonical identity / `None` = 未登録 cwd で解決不能
    /// = wire op を fail-closed)。performer のときは `performer_parent` が identity を持つので
    /// 未使用 (`None`)。wiremsg identity を「繋いだ SP」依存から「自分」へ移す SSOT
    /// (旧: conductor は bare `"agent"` を送り SP 正規化に依存していた)。
    pub root_project: Option<String>,
}

impl SelfLane {
    /// cwd から SelfLane を判定。
    ///
    /// 1. cwd ancestors を walk して `.vp/lanes/<name>` pattern を探す
    ///    (= [`detect_project_local_performer`] の純粋関数で test 可能)
    /// 2. 見つかり repo_root が config 登録済なら performer (parent = config 名)
    /// 3. それ以外は conductor。自 project 名を `registered_project_name_for_cwd`
    ///    (config-only / SP 非依存) で解決。登録 project なら `Some(name)` = canonical
    ///    identity、未登録 cwd なら `None` = wire op fail-closed (誤 identity を送らない)。
    /// 4. cwd / config 取得失敗 → root_project=None (fail-closed)
    pub fn detect() -> Self {
        // identity 解決不能な conductor (cwd/config 取得失敗) → None で fail-closed
        let conductor_unresolved = || SelfLane {
            lane_name: crate::process::lanes_state::ROOT_LANE_NAME.to_string(),
            performer_parent: None,
            root_project: None,
        };
        let Ok(cwd) = std::env::current_dir() else {
            return conductor_unresolved();
        };
        let Ok(config) = crate::config::Config::load() else {
            return conductor_unresolved();
        };
        // performer 判定: cwd が <repo>/.vp/lanes/<name> 配下、かつ repo が config 登録済
        if let Some((performer_name, repo_root)) = detect_project_local_performer(&cwd)
            && let Some(p) = config
                .projects
                .iter()
                .find(|p| std::path::Path::new(&p.path) == repo_root.as_path())
        {
            return SelfLane {
                lane_name: performer_name,
                performer_parent: Some(p.name.clone()),
                root_project: None, // identity は performer_parent が持つ
            };
        }
        // conductor: 自 project 名を config-only で解決 (未登録 cwd は None = fail-closed)。
        // cwd は上で取得済みのものを正規化して渡す (二重取得を避ける)。
        SelfLane {
            lane_name: crate::process::lanes_state::ROOT_LANE_NAME.to_string(),
            performer_parent: None,
            root_project: crate::resolve::match_project_name_for_path(
                &crate::config::Config::normalize_path(&cwd),
                &config,
            ),
        }
    }

    /// `wire_send` / `wire_recv` / `wire_ack` / `wire_inbox` / `flow_handoff` の `from`/`agent`
    /// 値 = この MCP プロセスの canonical wire address。
    ///
    /// - performer → `"agent@<parent>/<name>"` (parent は config 名、常に解決可)
    /// - conductor → `"agent@<project>"` (起動時 `detect()` が cwd→config で解決した自 project)
    /// - conductor で project 未解決 (未登録 cwd) → `Err` = **fail-closed**
    ///
    /// identity を「繋いだ SP の正規化」依存から「自分 (MCP)」へ移した SSOT。
    /// 旧実装は conductor が bare `"agent"` を送り、接続先 SP の project で正規化していたため、
    /// 誤 SP 接続 (`resolve_process_port` の 33000 fallback 等) で identity が化け、ack が宛先と
    /// mismatch して command 再 nudge が止まらないバグの根だった。中央 store が SSOT なので
    /// canonical を送れば接続先 SP は無関係になる (`normalize_agent_addr` は `agent@x` を
    /// 冪等で素通しするので後方互換も保たれる)。
    pub fn from_address(&self) -> Result<String, McpError> {
        match &self.performer_parent {
            Some(parent) => Ok(format!("agent@{}/{}", parent, self.lane_name)),
            None => match &self.root_project {
                Some(project) => Ok(format!("agent@{}", project)),
                None => Err(McpError::invalid_params(
                    "wire identity を解決できません: 現在の作業ディレクトリがどの登録 project 配下にもありません。`vp projects add <path>` で登録してから wire を使ってください (誤 identity 送信を防ぐ fail-closed)。".to_string(),
                    None,
                )),
            },
        }
    }
}

/// cwd ancestors を walk して `<repo>/.vp/lanes/<name>` pattern を探す純粋関数。
///
/// 戻り値: `Some((performer_name, repo_root))` if 見つかれば、 そうでなければ `None`。
/// - performer dir 直下 / 任意の子孫 cwd 両対応 (= ancestor 走査)
/// - 最初に match した ancestor (= 最も深い performer) を採用
/// - I/O なしの pure fn (test しやすい、 mock cwd 不要)
fn detect_project_local_performer(cwd: &std::path::Path) -> Option<(String, std::path::PathBuf)> {
    for ancestor in cwd.ancestors() {
        let parent = ancestor.parent()?;
        let grandparent = parent.parent()?;
        if parent.file_name().and_then(|n| n.to_str()) == Some("lanes")
            && grandparent.file_name().and_then(|n| n.to_str()) == Some(".vp")
        {
            let performer_name = ancestor.file_name()?.to_str()?.to_string();
            // performer 名は `validate_performer_name` 通過済が前提。 但し `.` 等 dotfile は除外。
            if performer_name.starts_with('.') || performer_name.is_empty() {
                return None;
            }
            let repo_root = grandparent.parent()?.to_path_buf();
            return Some((performer_name, repo_root));
        }
    }
    None
}

/// Unison QUIC で Process と通信する。
/// process チャネルは lazy 接続し、persistent に保持。
/// Ruby / capture 等の未対応メソッドは HTTP フォールバック。
pub struct VantageMcp {
    /// HTTP クライアント（QUIC 未対応の API 用フォールバック）
    client: reqwest::Client,
    /// Process の HTTP ベース URL
    process_url: Arc<Mutex<String>>,
    /// Process の HTTP ポート番号（QUIC = port + QUIC_PORT_OFFSET）
    process_port: Arc<Mutex<u16>>,
    /// Unison "process-proxy" チャネル（lazy 接続、canvas 操作も含む）
    process_channel: Arc<Mutex<Option<Arc<unison::network::channel::UnisonChannel>>>>,
    /// L0 SP-portless: この MCP が操作対象とする project の path（World "process-proxy" の
    /// handshake に渡す stable な project 識別子）。 port と違い reshuffle で揺れない。
    project_path: Arc<String>,
    /// この MCP プロセスが属する Lane（cwd 由来、VP-166 PR-4）
    self_lane: SelfLane,
    tool_router: ToolRouter<Self>,
}

impl Clone for VantageMcp {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            process_url: self.process_url.clone(),
            process_port: self.process_port.clone(),
            process_channel: self.process_channel.clone(),
            project_path: self.project_path.clone(),
            self_lane: self.self_lane.clone(),
            tool_router: Self::tool_router()
                + Self::agent_tool_router()
                + Self::canvas_router()
                + Self::editor_router()
                + Self::lane_router()
                + Self::layout_router(),
        }
    }
}

#[tool_router]
impl VantageMcp {
    pub fn new(process_port: u16, project_path: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            process_url: Arc::new(Mutex::new(format!("http://[::1]:{}", process_port))),
            process_port: Arc::new(Mutex::new(process_port)),
            process_channel: Arc::new(Mutex::new(None)),
            project_path: Arc::new(project_path),
            self_lane: SelfLane::detect(),
            tool_router: Self::tool_router()
                + Self::agent_tool_router()
                + Self::canvas_router()
                + Self::editor_router()
                + Self::lane_router()
                + Self::layout_router(),
        }
    }

    /// Unison QUIC チャネルを取得（lazy 接続）
    ///
    /// チャネルが未接続または切断済みの場合、新規接続して返す。
    async fn get_quic_channel(
        &self,
        channel_slot: &Arc<Mutex<Option<Arc<unison::network::channel::UnisonChannel>>>>,
        _channel_name: &str,
    ) -> Result<Arc<unison::network::channel::UnisonChannel>, McpError> {
        let mut guard = channel_slot.lock().await;

        // 既存チャネルがあれば再利用
        if let Some(ch) = guard.as_ref() {
            return Ok(Arc::clone(ch));
        }

        // L0 SP-portless: SP 直結 (process_port) ではなく World :32000 の "process-proxy"
        // channel に繋ぐ。 World が project_path から SP の "control" channel を逆引きして
        // process method を forward する (reverse-routing)。 World は常駐 daemon で port は
        // 固定なので、 旧来の stale-port self-heal は不要。
        let client = connect_quic(&quic_addr(crate::cli::world_port())).await?;
        // unison 内部の request timeout は default 30s。 outer timeout (wire_recv 等で
        // server_timeout + buffer = 最大 35s) より長く取らないと unison 側が先に発火するので
        // 60s に引き上げる (VP-163)。
        let channel = Arc::new(
            client
                .open_channel("process-proxy")
                .await
                .map_err(|e| {
                    McpError::internal_error(
                        format!("Unison process-proxy channel error: {}", e),
                        None,
                    )
                })?
                .with_request_timeout(std::time::Duration::from_secs(60)),
        );

        // handshake: project_path を渡す (World が path_key に正規化して control channel を逆引き)。
        channel
            .request::<serde_json::Value, serde_json::Value>(
                "subscribe",
                &serde_json::json!({ "project_path": self.project_path.as_str() }),
            )
            .await
            .map_err(|e| {
                McpError::internal_error(format!("process-proxy handshake error: {}", e), None)
            })?;

        *guard = Some(Arc::clone(&channel));
        Ok(channel)
    }

    /// Unison QUIC の "process" チャネルでメソッドを呼び出す（outer timeout = 5s）
    ///
    /// 接続失敗 / タイムアウト時はチャネルをリセットして1回リトライする。
    async fn quic_call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        self.quic_call_with_timeout(method, payload, std::time::Duration::from_secs(5))
            .await
    }

    /// Unison QUIC の "process" チャネル呼び出し（outer timeout 指定可）
    ///
    /// `wire_recv` のように **server 側が長時間ブロックする** method では、 outer timeout を
    /// server 側 timeout より長く取らないと client が先に諦め、 チャネルを reset → 空振り
    /// リトライ → 同一 thread の recv 二重発火 → msg ロスにつながる (VP-163)。 そういう
    /// method は呼び出し側で `server_timeout + buffer` を渡すこと。
    ///
    /// また、 server ハンドラが `Err(e)` を返した場合、 unison は専用 error frame を持たない
    /// ため `unison_server.rs` の dispatch loop が **成功フレームに `{"error": "<msg>"}` を詰めて**
    /// 返してくる。 これをそのまま素通しすると 呼び出し側が「成功」と誤報するので (VP-163)、
    /// その形のレスポンスは `McpError` に変換する。
    async fn quic_call_with_timeout(
        &self,
        method: &str,
        payload: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, McpError> {
        use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

        let tid = new_trace_id();
        let start = std::time::Instant::now();
        write_trace(
            &TraceEntry::new(
                "quic",
                &tid,
                "request",
                "INFO",
                format!("process.{}", method),
            )
            .with_data(payload.clone()),
        );

        let channel = self
            .get_quic_channel(&self.process_channel, "process")
            .await?;

        // タイムアウト付きリクエスト（Process 再起動時のハング防止）
        let result = tokio::time::timeout(
            timeout,
            channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
        )
        .await;

        let resp = match result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                // チャネルエラー: リセットしてリトライ
                tracing::warn!("QUIC process.{} failed, retrying: {}", method, e);
                *self.process_channel.lock().await = None;

                let channel = self
                    .get_quic_channel(&self.process_channel, "process")
                    .await?;
                tokio::time::timeout(
                    timeout,
                    channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
                )
                .await
                .map_err(|_| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry timed out", method),
                        None,
                    )
                })?
                .map_err(|e| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry failed: {}", method, e),
                        None,
                    )
                })?
            }
            Err(_) => {
                // タイムアウト: 古い接続をリセットしてリトライ
                tracing::warn!("QUIC process.{} timed out, resetting channel", method);
                *self.process_channel.lock().await = None;

                let channel = self
                    .get_quic_channel(&self.process_channel, "process")
                    .await?;
                tokio::time::timeout(
                    timeout,
                    channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
                )
                .await
                .map_err(|_| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry timed out", method),
                        None,
                    )
                })?
                .map_err(|e| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry failed: {}", method, e),
                        None,
                    )
                })?
            }
        };

        // server ハンドラの Err は `{"error": "<msg>"}` の単一キー object で返ってくる (VP-163)
        if let Some(err_msg) = rpc_response_error(&resp) {
            write_trace(
                &TraceEntry::new(
                    "quic",
                    &tid,
                    "response",
                    "ERROR",
                    format!("process.{} error: {}", method, err_msg),
                )
                .with_elapsed(start.elapsed().as_millis() as u64),
            );
            return Err(McpError::internal_error(
                format!("process.{}: {}", method, err_msg),
                None,
            ));
        }

        write_trace(
            &TraceEntry::new(
                "quic",
                &tid,
                "response",
                "INFO",
                format!("process.{} OK", method),
            )
            .with_elapsed(start.elapsed().as_millis() as u64),
        );
        Ok(resp)
    }

    // tmux decoupling PR1-2: `resolve_pane`（label/pane_id → tmux pane 解決 helper）は退役。
    // lane の宛先解決は lane address 直（`lane_nudge` / `lane_capture`）に一本化。

    /// Process に QUIC で ProcessMessage を送信（show/clear/toggle_pane/close_pane）
    async fn process_call(
        &self,
        method: &str,
        msg: &ProcessMessage,
    ) -> Result<serde_json::Value, McpError> {
        let payload = serde_json::to_value(msg)
            .map_err(|e| McpError::internal_error(format!("Serialize error: {}", e), None))?;
        self.quic_call(method, payload).await
    }

    // =========================================================================
    // dev-flow primitives (= Conductor × Performer × Memory orchestration の core 操作)
    //
    // `flow_handoff`: P4 (add_performer + wire_send + nudge) を atomic 1 step。
    // `flow_progress`: P5 (list_lanes + per-lane unread count + git status) を集約 1 view。
    //
    // 既存 primitives (add_performer / wire_send / tmux_agent_send / list_lanes) はそのまま、
    // flow_* は composition tool (= 順番に呼んで意味のある orchestration を 1 call 化)。
    // =========================================================================

    /// flow_handoff の rollback path: performer 削除 (best-effort、 失敗は log only)
    ///
    /// wire_send 失敗時など、 performer 作成は成功したが orchestration の続きが失敗した時に呼ぶ。
    /// lanes portless (doc 27 §3.4.5): 旧 SP HTTP DELETE /api/lanes を World process-proxy ask
    /// `lane_delete` に移管。 不在 performer は "Lane not found" を Err で返すので idempotent
    /// no-op として吸収する (= 旧 HTTP 404 NOT_FOUND を許容していた挙動と等価)。
    async fn flow_rollback_performer(
        &self,
        project_name: &str,
        performer_name: &str,
    ) -> Result<(), String> {
        let address = format!("{}/performer/{}", project_name, performer_name);
        let payload = serde_json::json!({ "address": address, "cleanup": true });
        match self
            .quic_call_with_timeout("lane_delete", payload, Duration::from_secs(30))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("Lane not found") => Ok(()),
            Err(e) => Err(format!("rollback lane_delete 失敗: {}", e)),
        }
    }

    /// Restart the Vantage Point Process
    ///
    /// This tool restarts the Process process while preserving session state.
    /// Useful after rebuilding the binary.
    /// HTTP ベースのプロセス管理のため QUIC は使わない。
    #[tool(
        description = "Restart the Vantage Point Process. Session state is preserved. Returns when Process is ready."
    )]
    async fn restart(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<RestartParams>,
    ) -> Result<CallToolResult, McpError> {
        let _ = params;
        // L0 finale (Push-only): 旧 SP HTTP の手動 shutdown+respawn dance (`/api/health` poll +
        // `/api/shutdown` + `vp start` 子 spawn) を撤去し、 World :32000 の restart API に委譲する。
        // World の `restart_process` が stop + `start_process`（registry-based launch verify）を
        // atomically 実行。 旧 respawn の `vp start` は撤去済の存在しないコマンドで既に壊れていた
        // (CLAUDE.md) ため、 本移行は fix も兼ねる。
        let config = crate::config::Config::load().ok();
        let project_name = match &config {
            Some(c) => crate::resolve::project_name_from_path(self.project_path.as_str(), c),
            None => std::path::Path::new(self.project_path.as_str())
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string(),
        };

        // doc 45 段 2: 旧 `POST /api/world/processes/{name}/restart` を Unison
        // `world-control.projects/restart` に差し替え。restart は内部に grace sleep +
        // 起動確認を含むので、旧 HTTP の 45s timeout に相当する外側 timeout をここで掛ける
        // (Unison client 側に per-request timeout が無いため)。
        let client = tokio::time::timeout(
            Duration::from_secs(45),
            crate::daemon::client::WorldControlClient::connect(crate::cli::world_port(), 3),
        )
        .await
        .map_err(|_| McpError::internal_error("World restart 接続 timeout (45s)", None))?
        .map_err(|e| {
            McpError::internal_error(format!("World restart に到達できません: {}", e), None)
        })?;

        let result = tokio::time::timeout(
            Duration::from_secs(45),
            client.projects_restart(&project_name),
        )
        .await
        .map_err(|_| McpError::internal_error("World restart timeout (45s)", None))?
        .map_err(|e| McpError::internal_error(format!("World restart 失敗: {}", e), None))?;

        // QUIC チャネルをリセットして再接続を強制（新 SP に張り直す）
        *self.process_channel.lock().await = None;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Process '{}' を World restart で再起動: {}",
                project_name, result
            ),
        )]))
    }

    // ========================================================================
    // wiremsg / delegation の imperative helper（生成 tool wrapper から委譲）。
    //
    // tool 定義本体は schema/vp-agent.kdl → generated/agent_tools.rs に移行済
    // （rebuild Epic L2 第二手、doc 27 §5）。ここに残すのは「宣言に落ちない」3 本:
    //   - wire_recv: server が最大 timeout 秒ブロックするため outer timeout に +5s 余裕が要る（VP-163）。
    //   - complete : outcome string → typed {kind, ...} の wire shape 変換 + validation。
    //   - wire_send: body.category 省略時の default="command" 注入（body-field default が宣言に落ちない、B1）。
    // ========================================================================

    /// wire_recv の本体（生成 tool wrapper `wire_recv` から委譲）。
    async fn wire_recv_impl(&self, params: WireRecvParams) -> Result<CallToolResult, McpError> {
        let timeout = params.timeout.unwrap_or(5).min(30);
        // agent は wire_send の from と同じ self_lane 由来 address
        let agent = self.self_lane.from_address()?;
        let payload = serde_json::json!({
            "agent": agent,
            "timeout": timeout,
        });
        // server 側が最大 `timeout` 秒ブロックするので、 outer timeout は +5s 余裕を持たせる
        // (msg_recv と同じ理由 — VP-163)
        let resp = self
            .quic_call_with_timeout(
                "wire_recv",
                payload,
                std::time::Duration::from_secs(timeout + 5),
            )
            .await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "null".to_string()),
        )]))
    }

    /// complete の本体（生成 tool wrapper `complete` から委譲、doc 28 §4）。
    async fn complete_impl(&self, params: CompleteParams) -> Result<CallToolResult, McpError> {
        // outcome string → typed Outcome の wire shape（SP 側 `serde(tag="kind")` に写す）。
        let kind = params.outcome.trim().to_lowercase();
        let outcome = match kind.as_str() {
            "done" => serde_json::json!({ "kind": "done", "result": params.result }),
            "failed" => serde_json::json!({ "kind": "failed", "reason": params.result }),
            // needs_input / needsinput どちらの綴りも NeedsInput(=Reborn) に写す。
            "needs_input" | "needsinput" => {
                serde_json::json!({ "kind": "needsinput", "question": params.result })
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("outcome は 'done' / 'failed' / 'needs_input' のみ (got: {other})"),
                    None,
                ));
            }
        };
        let payload = serde_json::json!({ "id": params.id, "outcome": outcome });
        let resp = self.quic_call("complete", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "completed".to_string()),
        )]))
    }

    /// wire_send の本体（生成 tool wrapper `wire_send` から委譲、body="custom"）。
    ///
    /// B1: CC 経由の wire_send は `body.category` 省略時に `"command"` を default 注入する。
    /// 「投げたら読んでほしい」が多数派なので、素の wire_send を「配送 + 自動読み込み + ack 必須」
    /// (= command 挙動) に昇格させる。FYI は `body.category` に `"event"` / `"data"` / `"state"` /
    /// `"log"` を明示して opt-out（明示指定は尊重し上書きしない）。default 注入をこの MCP 経路に
    /// 閉じることで、category を明示しない内部 sender（delegation / server の `world_wire::call`）を
    /// 巻き込まない（= CC 限定 scope、global 反転による予期せぬ nudge を回避）。
    async fn wire_send_impl(&self, params: WireSendParams) -> Result<CallToolResult, McpError> {
        let __self_lane = self.self_lane.from_address()?;
        // category 省略時のみ "command" を注入（明示された category は温存）。
        let mut body = params.body;
        if let Some(obj) = body.as_object_mut() {
            obj.entry("category")
                .or_insert_with(|| serde_json::Value::String("command".to_string()));
        }
        let payload = serde_json::json!({
            "from": __self_lane,
            "to": params.to,
            "body": body,
            "reply_to": params.reply_to,
        });
        let resp = self.quic_call("wire_send", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "wire message sent".to_string()),
        )]))
    }
}

// router は combined router を持つ `self.tool_router` field を指す（new() で
// `Self::tool_router() + Self::agent_tool_router()` を一度だけ構築）。
// 既定の `#[tool_handler]` は `Self::tool_router()`（手書きのみ）を呼ぶため、
// 生成 tool が list_tools/call_tool に乗らない（rebuild Epic L2 第二手の live バグ）。
#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for VantageMcp {
    fn get_info(&self) -> ServerInfo {
        // rmcp 1.6 で ServerInfo は #[non_exhaustive] になり struct expression (= `ServerInfo { ... ..Default::default() }`)
        // が外部 crate から使えなくなった。 `Default::default()` で base instance を作ってから pub field を mutate
        // する pattern で API contract を満たす (= 公式が future-compatible として用意してる upgrade path)。
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Vantage Point Process - Pin rich content (markdown, HTML, images) to the board (Paisley Park). \
             Use 'capture_window' to take a PNG screenshot of the GUI window (viewable with Read tool), \
             'show' to pin content to the board, 'clear' to clear the board, \
             'close_pane' to close a pane, 'toggle_pane' to toggle panel visibility, \
             'restart' to restart the Process, \
             'watch_file' to monitor a log file in real-time, and 'unwatch_file' to stop monitoring.\n\n\
             When using 'show', prefer content_type='markdown' as the default format. \
             Markdown renders well on the board and is easy to read. \
             Use content_type='html' only when you need precise visual layout (dashboards, diagrams with colors, interactive elements).".into()
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

/// performer context のとき parent project の path を config から引く（VP-165 (A)）。
///
/// conductor context（`performer_parent = None`）or parent が config に無い なら `None`。
fn performer_parent_path(self_lane: &SelfLane, config: &crate::config::Config) -> Option<String> {
    let parent_name = self_lane.performer_parent.as_ref()?;
    config
        .projects
        .iter()
        .find(|p| &p.name == parent_name)
        .map(|p| p.path.clone())
}

/// Process ポートを解決（MCP 通信先の決定）
///
/// VP-165 (doc 17 決定A): **discovery（= TheWorld、reconciliation の真実源）で live port を
/// 引くのを最優先**にする。stale port を踏むと別 project の SP に msg を投げ、その SP の
/// `local_project` で `from` が汚染される（VP-165 dogfood 症状 (1)）。
///
/// 優先度:
/// 1. 明示的なポート引数（Some で指定された場合）
/// 2. discovery:
///    - performer context（cwd = `vp_data_dir()/lanes/<parent>-<name>`）→ parent project の path を
///      config から引いて `find_by_project`。performer の cwd は登録 project path 配下でないので
///      `find_for_cwd` は効かない
///    - conductor context → `find_for_cwd`（cwd 一致 or 配下の running SP）
/// 3. デフォルトポート 33000
async fn resolve_process_port(explicit_port: Option<u16>) -> u16 {
    // 1. 明示的なポート指定
    if let Some(port) = explicit_port {
        return port;
    }

    // 2. discovery で live port を引く
    let self_lane = SelfLane::detect();
    match &self_lane.performer_parent {
        Some(_) => {
            // performer: parent project の SP を discovery で解決
            if let Some(parent_path) = crate::config::Config::load()
                .ok()
                .as_ref()
                .and_then(|c| performer_parent_path(&self_lane, c))
                && let Some(info) = crate::discovery::find_by_project(&parent_path).await
            {
                return info.port;
            }
        }
        None => {
            // conductor: cwd 一致（or 配下）の running SP
            if let Some(info) = crate::discovery::find_for_cwd().await {
                return info.port;
            }
        }
    }

    // 3. フォールバック
    33000
}

/// L0 SP-portless: World "process-proxy" の addressing 用に、 この MCP が属する project の
/// path（正規化済 path_key と同形）を解決する。 `resolve_process_port` と同じ discovery 経路
/// (performer→parent / conductor→cwd) で running SP の `project_dir` を引く。 SP 未起動などで
/// 引けない場合は cwd の正規化 path に fallback（World 側で normalize_path_key 再正規化される）。
async fn resolve_project_path() -> String {
    let self_lane = SelfLane::detect();
    let info = match &self_lane.performer_parent {
        Some(_) => {
            if let Some(parent_path) = crate::config::Config::load()
                .ok()
                .as_ref()
                .and_then(|c| performer_parent_path(&self_lane, c))
            {
                crate::discovery::find_by_project(&parent_path).await
            } else {
                None
            }
        }
        None => crate::discovery::find_for_cwd().await,
    };
    if let Some(info) = info {
        return info.project_dir;
    }
    // fallback: cwd の正規化 path（running SP が無くても project を addressing できる）。
    std::env::current_dir()
        .ok()
        .map(|p| crate::config::Config::normalize_path(&p))
        .unwrap_or_default()
}

/// HTTP port から同一 host の QUIC 接続先アドレスを組み立てる ([::1] = IPv6 loopback)。
///
/// Process(SP) port と World port(`cli::world_port()`、fetch_canvas_panes 等)の両方で
/// 使われる。`QUIC_PORT_OFFSET` は両者共通の前提 — SP/World でオフセットを分ける時は
/// この関数を分割すること。
fn quic_addr(http_port: u16) -> String {
    format!(
        "[::1]:{}",
        http_port + crate::process::unison_server::QUIC_PORT_OFFSET
    )
}

/// QUIC transport を組み立て、 `addr` へ接続済みの [`unison::ProtocolClient`] を返す。
///
/// VP-184: QuicClient builder。 dev default (= SkipVerification) を明示的に渡す。
/// PR-3 で trust_anchors を InternalMeshKeypair の client 半分に差し替える。
async fn connect_quic(addr: &str) -> Result<unison::ProtocolClient, McpError> {
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| McpError::internal_error(format!("Unison client error: {}", e), None))?;
    let client = unison::ProtocolClient::new(transport);
    client.connect(addr).await.map_err(|e| {
        McpError::internal_error(format!("Unison connect error ({}): {}", addr, e), None)
    })?;
    Ok(client)
}

/// Run the MCP server over stdio
pub async fn run_mcp_server(process_port: Option<u16>) -> anyhow::Result<()> {
    // rustls 0.23+ は CryptoProvider の明示的な設定が必要（QUIC 接続用）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // トレースログファイルを早期初期化
    crate::trace_log::init_log_file();

    // Resolve the actual port to use（HTTP フォールバック用に保持）
    let resolved_port = resolve_process_port(process_port).await;
    // L0 SP-portless: QUIC 経路は World "process-proxy" を project_path で addressing する。
    let project_path = resolve_project_path().await;

    // wiremsg R5-4: 旧 msgbox の registry サブシステム (Performer self-register) は撤去済。
    // wire の cross-process delivery は TheWorld の project registry を使う別経路。

    // Note: In MCP mode, we should not use tracing to stdout
    // as it interferes with JSON-RPC communication
    let service = VantageMcp::new(resolved_port, project_path)
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    service.waiting().await?;

    Ok(())
}

/// QUIC "process" チャネルのレスポンスが server ハンドラの Err かどうか判定する (VP-163)。
///
/// unison は専用の error frame を持たないため、 `unison_server.rs` の dispatch loop は
/// ハンドラの `Err(e)` を **成功フレームに `{"error": "<msg>"}` を詰めて** 返す。
/// その形 (= 単一キー `"error"` で値が string) なら err message を返す。 それ以外は `None`。
/// (`{"error": ..., "to": ...}` のような複数キーや非 string は対象外 — 通常の payload)
fn rpc_response_error(resp: &serde_json::Value) -> Option<&str> {
    let obj = resp.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    obj.get("error")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rebuild Epic L2 第二手: 生成 router（agent_tool_router）が手書き router と合流し、
    /// wire/delegation 8 tool が登録され、かつ手書き tool も消えていないことを検証する。
    /// 生成コードの forward 本体は手書きと byte-equivalent なので、runtime のリスクは
    /// 「rmcp router 合成での tool 登録」に集約される — それをここで直接 assert する。
    #[test]
    fn agent_tool_router_merges_wire_family() {
        // new()/Clone と同じ全 router 合成（手書き tool_router + 生成 agent + family canvas/lane）。
        // 合成漏れは build/test を通過し live tools/list で初めて露見するため、ここで直接 assert する。
        let router = VantageMcp::tool_router()
            + VantageMcp::agent_tool_router()
            + VantageMcp::canvas_router()
            + VantageMcp::lane_router();
        // 生成された wire/delegation 8 tool が登録されていること。
        for name in [
            "wire_send",
            "wire_recv",
            "wire_thread",
            "wire_inbox",
            "wire_ack",
            "delegate",
            "complete",
            "respond",
        ] {
            assert!(
                router.has_route(name),
                "生成 tool {name} が登録されていない"
            );
        }
        // tool_router（手書き、restart のみ）の tool が合成で失われていないこと。
        assert!(
            router.has_route("restart"),
            "手書き tool restart が失われた"
        );
        // board family の代表 tool が合成 router に登録されていること
        // （doc 52 §7: list_canvas / read_pane は撤去済み。§4/§5: read_board / update を新設）。
        for name in ["show", "clear", "capture_window", "update", "read_board"] {
            assert!(
                router.has_route(name),
                "canvas family tool {name} が合成 router に無い"
            );
        }
        // lane_router family の代表 tool が合成 router に登録されていること。
        for name in ["switch_lane", "flow_handoff", "list_lanes"] {
            assert!(
                router.has_route(name),
                "lane family tool {name} が合成 router に無い"
            );
        }
    }

    #[test]
    fn test_self_lane_from_address() {
        // wiremsg identity SSOT: conductor は解決済 project で "agent@<project>"、
        // performer は "agent@<parent>/<name>"。project 未解決の conductor は fail-closed (Err)。
        let conductor = SelfLane {
            lane_name: crate::process::lanes_state::ROOT_LANE_NAME.to_string(),
            performer_parent: None,
            root_project: Some("vantage-point".to_string()),
        };
        assert_eq!(conductor.from_address().unwrap(), "agent@vantage-point");

        let performer = SelfLane {
            lane_name: "chore".to_string(),
            performer_parent: Some("vantage-point".to_string()),
            root_project: None,
        };
        assert_eq!(
            performer.from_address().unwrap(),
            "agent@vantage-point/chore"
        );

        // 未登録 cwd の conductor (project 未解決) → fail-closed
        let unresolved = SelfLane {
            lane_name: crate::process::lanes_state::ROOT_LANE_NAME.to_string(),
            performer_parent: None,
            root_project: None,
        };
        assert!(
            unresolved.from_address().is_err(),
            "project 未解決 root は fail-closed"
        );
    }

    // --- detect_project_local_performer (project-local lane refactor PR 2) ---

    #[test]
    fn detect_pl_performer_finds_performer_dir_itself() {
        use std::path::{Path, PathBuf};
        let cwd = Path::new("/Users/makoto/repos/creo-memories/.vp/lanes/or-integration");
        let result = detect_project_local_performer(cwd);
        assert_eq!(
            result,
            Some((
                "or-integration".to_string(),
                PathBuf::from("/Users/makoto/repos/creo-memories"),
            ))
        );
    }

    #[test]
    fn detect_pl_performer_finds_performer_from_nested_subdir() {
        use std::path::{Path, PathBuf};
        // performer 配下の任意の階層から呼んでも親 performer が見つかる
        let cwd =
            Path::new("/Users/makoto/repos/creo-memories/.vp/lanes/or-integration/apps/server/src");
        let result = detect_project_local_performer(cwd);
        assert_eq!(
            result,
            Some((
                "or-integration".to_string(),
                PathBuf::from("/Users/makoto/repos/creo-memories"),
            ))
        );
    }

    #[test]
    fn detect_pl_performer_returns_none_for_plain_repo_cwd() {
        // 通常の repo cwd (= conductor context) は detect されない
        let cwd = std::path::Path::new("/Users/makoto/repos/creo-memories");
        assert_eq!(detect_project_local_performer(cwd), None);
    }

    #[test]
    fn detect_pl_performer_returns_none_for_random_path() {
        let cwd = std::path::Path::new("/tmp/random/dir");
        assert_eq!(detect_project_local_performer(cwd), None);
    }

    #[test]
    fn detect_pl_performer_ignores_lanes_without_vp_grandparent() {
        // `/foo/lanes/bar` だけだと `.vp` 親が無いので match しない
        let cwd = std::path::Path::new("/foo/lanes/bar");
        assert_eq!(detect_project_local_performer(cwd), None);
    }

    #[test]
    fn detect_pl_performer_ignores_dotfile_performer_names() {
        // `.vp/lanes/.hidden` のような dot 始まり performer 名は除外 (= validate_performer_name 同等)
        let cwd = std::path::Path::new("/repo/.vp/lanes/.hidden");
        assert_eq!(detect_project_local_performer(cwd), None);
    }

    #[test]
    fn detect_pl_performer_innermost_wins_for_nested_vp_lanes() {
        // 病的 case: performer 配下にさらに `.vp/lanes/<inner>` がある (= nested vp 構成)
        // ancestor は cwd から root へ走るので、 最も深い (= innermost) performer が選ばれる
        use std::path::{Path, PathBuf};
        let cwd = Path::new("/outer/.vp/lanes/A/.vp/lanes/B");
        let result = detect_project_local_performer(cwd);
        assert_eq!(
            result,
            Some(("B".to_string(), PathBuf::from("/outer/.vp/lanes/A")))
        );
    }

    #[test]
    fn test_quic_addr_format() {
        // QUIC_PORT_OFFSET = 0 — process port がそのまま QUIC port になる。
        assert_eq!(quic_addr(33003), "[::1]:33003");
        assert_eq!(quic_addr(33000), "[::1]:33000");
    }

    #[test]
    fn test_performer_parent_path_resolution() {
        use crate::config::{Config, ProjectConfig};
        let mut cfg = Config::default();
        cfg.projects.push(ProjectConfig {
            name: "vantage-point".to_string(),
            path: "/Users/x/repos/vantage-point".to_string(),
            port: None,
            enabled: true,
            slot: None,
        });

        // performer → parent の path
        let performer = SelfLane {
            lane_name: "chore".to_string(),
            performer_parent: Some("vantage-point".to_string()),
            root_project: None,
        };
        assert_eq!(
            performer_parent_path(&performer, &cfg).as_deref(),
            Some("/Users/x/repos/vantage-point")
        );

        // conductor context → None（performer_parent が無い）
        let conductor = SelfLane {
            lane_name: crate::process::lanes_state::ROOT_LANE_NAME.to_string(),
            performer_parent: None,
            root_project: None,
        };
        assert_eq!(performer_parent_path(&conductor, &cfg), None);

        // config に無い parent → None
        let unknown = SelfLane {
            lane_name: "x".to_string(),
            performer_parent: Some("not-in-config".to_string()),
            root_project: None,
        };
        assert_eq!(performer_parent_path(&unknown, &cfg), None);
    }

    #[test]
    fn test_rpc_response_error_detection() {
        // server ハンドラの Err 形式 → err message を取り出す
        assert_eq!(
            rpc_response_error(
                &serde_json::json!({"error": "自分自身('agent')への送信はできません"})
            ),
            Some("自分自身('agent')への送信はできません")
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!({"error": "不明なメソッド: process.foo"})),
            Some("不明なメソッド: process.foo")
        );

        // 通常の成功 payload は対象外
        assert_eq!(
            rpc_response_error(&serde_json::json!({"status": "ok", "id": "x"})),
            None
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!({"message": null, "reason": "timeout"})),
            None
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!({"message": {"id": "x"}})),
            None
        );

        // 複数キーで "error" を含んでも、 これは err frame ではない (= HTTP handler 等の形)
        assert_eq!(
            rpc_response_error(&serde_json::json!({"error": "x", "to": "agent"})),
            None
        );

        // "error" が非 string、 object 以外
        assert_eq!(rpc_response_error(&serde_json::json!({"error": 42})), None);
        assert_eq!(
            rpc_response_error(&serde_json::json!({"error": {"nested": true}})),
            None
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!("just a string")),
            None
        );
        assert_eq!(rpc_response_error(&serde_json::json!(null)), None);
        assert_eq!(rpc_response_error(&serde_json::json!([1, 2, 3])), None);
        assert_eq!(rpc_response_error(&serde_json::json!({})), None);
    }
}
