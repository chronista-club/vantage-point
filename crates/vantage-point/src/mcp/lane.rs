//! MCP lane / flow family tools — switch_lane / list_lanes / add_performer /
//! delete_performer / flow_handoff / flow_progress。
//!
//! mcp.rs から family module に分割（手書きのまま、description / signature を逐語保持）。
//! helper（resolve_pane / quic_call / process_call / flow_rollback_performer 等）は親
//! mcp.rs に据え置き、子 module から呼ぶ。
use super::*;

/// Parameters for the switch_lane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SwitchLaneParams {
    /// Lane token to activate within the current project
    #[schemars(
        description = "Lane token to activate in the current project's vp-app: 'conductor' (lead) or a performer name (e.g. 'feat-api')."
    )]
    pub lane: String,
}

/// lane JSON（`lanes_list` の要素）から lane 名を取り出す。
///
/// doc 44 P2: 名前の在処は `address.name` **のみ**（旧 `LaneInfo.kind` / 複製 `name` は撤去）。
/// MCP は JSON を直に触るため型変更がコンパイル時に伝わらない — 旧 field を読んでいた箇所は
/// 全て None に落ちて `"unknown"` / `"unnamed"` を返す壊れ方をしていた（doc 44 §6.4 の同型）。
fn lane_name_of(lane: &serde_json::Value) -> String {
    lane.get("address")
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// lane JSON を旧 `kind` 語彙（`"conductor"` / `"performer"`）に射影する。
///
/// MCP tool の `kind` param は client との契約なので語彙は据え置き、判定だけ名前ベースにした
/// （開発起点は予約名 `conductor`、それ以外が旧 performer）。
fn lane_kind_label(lane: &serde_json::Value) -> &'static str {
    if lane_name_of(lane) == crate::process::lanes_state::CONDUCTOR_LANE_NAME {
        "conductor"
    } else {
        "performer"
    }
}

/// Parameters for the add_performer tool (R5: lane clone + Performer Lane spawn).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddPerformerParams {
    /// Performer name. Used as the `name` field of the Lane address (`<project>/performer/<name>`).
    #[schemars(
        description = "Performer name (人間可読の短い slug、 例: 'feat-api', 'sub'). Lane address の `<project>/performer/<name>` 部分になる。"
    )]
    pub name: String,
    /// Optional branch. If omitted, server auto-derives `<git-user>/<sanitized-name>`.
    #[schemars(
        description = "Lane clone する branch 名 (省略可)。 省略時は server が `git config user.name` から `<user>/<name>` を auto-derive。"
    )]
    pub branch: Option<String>,
    /// Optional Lane Stand. Defaults to "echoes".
    #[schemars(
        description = "Lane Stand 種類 (engine): 'echoes' (default、 claude) / 'codex' (OpenAI Codex CLI) / 'grok' (xAI Grok CLI) / 'opencode' (opencode、 model は opencode config) / 'shell'。"
    )]
    pub stand: Option<String>,
    /// Optional base ref for the worktree fork point (co-evolution #2).
    #[schemars(
        description = "worktree の分岐元 ref (省略可)。未 push の local branch も可 (conductor の feature branch 上の未 merge 土台を wing に配れる)。省略時は performer-files.kdl の base-ref → origin/HEAD → main。"
    )]
    pub base: Option<String>,
    /// Optional claude model alias for this lane (co-evolution #1).
    #[schemars(
        description = "この lane の claude model alias (省略可、例: 'opus' / 'sonnet' / 'haiku' / 'claude-fable-5')。task 難度に合わせて指定する (機械的作業=sonnet / 中核設計=opus 等)。Act I spawn・respawn・Act II engine が共有。省略時は config の default-lane-model (既定 Opus)。"
    )]
    pub model: Option<String>,
}

/// Parameters for the delete_performer tool (VP-124 Phase 1).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeletePerformerParams {
    /// Performer name to delete.
    #[schemars(
        description = "Performer name to delete (例: 'keystage', 'feat-api')。 Lane address の `<project>/performer/<name>` の `<name>` 部分。"
    )]
    pub name: String,

    /// Whether to also remove the lane workspace dir.
    #[schemars(
        description = "Lane workspace dir も削除するか (default: true)。 false で SP pool + tmux session のみ kill、 dir 残置 (debug / forensic 用途)。"
    )]
    #[serde(default)]
    pub cleanup: Option<bool>,
}

/// Parameters for the list_lanes tool (VP-124 Phase 1).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListLanesParams {
    /// Lane kind filter.
    #[schemars(
        description = "Lane kind フィルタ: 'conductor' or 'performer'。 省略時は両方含む。"
    )]
    #[serde(default)]
    pub kind: Option<String>,

    /// Lane state filter.
    #[schemars(
        description = "Lane state フィルタ: 'running' / 'spawning' / 'exiting' / 'dead'。 省略時は全状態。"
    )]
    #[serde(default)]
    pub state: Option<String>,
}

/// Parameters for the flow_handoff tool (dev-flow primitive: handoff in 1 call)
///
/// P4 (= 3-step orchestration: add_performer + wire_send + lane_nudge) を atomic 1 step に圧縮する。
/// 失敗時は performer 削除で rollback、 dirty state を残さない。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FlowHandoffParams {
    /// Performer name (新規作成する performer の slug)
    #[schemars(
        description = "Performer name (例: 'feat-api', 'sub')。 Lane address の `<project>/performer/<name>` 部分。"
    )]
    pub name: String,

    /// Optional branch (省略時は SP 側で `<git-user>/<sanitized-name>` を auto-derive)
    #[schemars(description = "Lane clone する branch (省略時は SP が auto-derive)。")]
    #[serde(default)]
    pub branch: Option<String>,

    /// Optional Lane Stand (default: "echoes")
    #[schemars(
        description = "Lane Stand (engine): 'echoes' (default、 claude) / 'codex' / 'grok' / 'opencode' / 'shell'。"
    )]
    #[serde(default)]
    pub stand: Option<String>,

    /// Optional base ref for the worktree fork point (co-evolution #2)
    #[schemars(
        description = "worktree の分岐元 ref (省略可)。未 push の local branch も可。省略時は performer-files.kdl の base-ref → origin/HEAD → main。"
    )]
    #[serde(default)]
    pub base: Option<String>,

    /// Optional claude model alias for this lane (co-evolution #1)
    #[schemars(
        description = "worker の claude model alias (省略可、例: 'opus' / 'sonnet' / 'haiku' / 'claude-fable-5')。task 難度に合わせて指定 (機械的=sonnet / 中核設計=opus)。省略時は config の default-lane-model (既定 Opus)。"
    )]
    #[serde(default)]
    pub model: Option<String>,

    /// Task spec — wire_send body の markdown 仕様 (= worker への指示)
    #[schemars(
        description = "Worker への markdown 仕様 (= wire body の `task_spec` field)。 多行 markdown 推奨。"
    )]
    pub task_spec: String,

    /// Mode: "hitl" (default、 nudge 後応答期待) or "auto" (nudge 後放置)
    #[schemars(
        description = "実行モード: 'hitl' (default、 nudge 後 worker からの応答を期待) / 'auto' (nudge 後放置、 完了 wire のみ受信)。"
    )]
    #[serde(default)]
    pub mode: Option<String>,

    /// Nudge enable (default: true)。 false で lane_nudge を skip。
    #[schemars(
        description = "lane_nudge で wire_recv 受信を促す nudge を発火するか (default: true)。 false で send のみ実行 (= 完全 async)。"
    )]
    #[serde(default)]
    pub nudge: Option<bool>,
}

/// Parameters for the flow_progress tool (dev-flow primitive: parallel work 集約 view)
///
/// P5 (= list_lanes + wire_recv unread + tmux_capture を別々に叩く) を 1 view に。 read-only、 cache OK。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FlowProgressParams {
    /// (現在未使用) 将来 multi-project 拡張時に使う slot
    #[schemars(
        description = "(reserved) 将来 multi-project view 拡張用。 現状省略可、 cwd の SP を見る。"
    )]
    #[serde(default)]
    pub project: Option<String>,
}

#[tool_router(router = lane_router, vis = "pub(crate)")]
impl VantageMcp {
    /// vp-app の active Lane を切り替える（B1: Unison-native、per-project）。
    #[tool(
        description = "Switch the active lane shown in the vp-app PP Canvas of the CURRENT project. `lane` is a lane token: 'conductor' (lead) or a performer name. Routes over Unison (local SP → canvas channel → vp-app). Primarily for ROTO / CLI driven view control; avoid switching the human's view unsolicited."
    )]
    async fn switch_lane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<SwitchLaneParams>,
    ) -> Result<CallToolResult, McpError> {
        // QUIC で local SP に SwitchLane を送る → hub → canvas channel → vp-app。
        // 旧: TheWorld(:32000) HTTP に global broadcast（project 切替意味論）。per-lane PP 後は
        // local SP への per-project 経路に統一（lane-within-project の active 切替）。
        let msg = ProcessMessage::SwitchLane {
            lane: params.lane.clone(),
        };
        self.process_call("switch_lane", &msg).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Switched active lane to '{}'", params.lane),
        )]))
    }

    /// R5: 現 project の SP に Performer Lane を新規作成 (lane clone + PtySlot spawn)。
    ///
    /// - cwd ベースで自動的に local SP を解決 (`self.process_url`)。
    /// - branch 省略時は server 側で `<git-user>/<sanitized-name>` を auto-derive。
    /// - 名前重複は HTTP 409 CONFLICT、 lane clone 失敗は 500 で返ってくる。
    #[tool(
        description = "Create a new Performer Lane in the current project (lane clone + spawn). Resolves the local SP via cwd. If `branch` is omitted, the server auto-derives `<git-user>/<sanitized-name>`. Returns the Lane address `<project>/performer/<name>` on success. Use this to spawn isolated parallel work (e.g. feature branches, exploratory experiments)."
    )]
    async fn add_performer(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<AddPerformerParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "name は必須です (空文字不可)".to_string(),
                None,
            ));
        }
        let mut body = serde_json::json!({ "name": params.name });
        if let Some(b) = params.branch.as_ref().filter(|s| !s.trim().is_empty()) {
            body["branch"] = serde_json::Value::String(b.clone());
        }
        if let Some(s) = params.stand.as_ref().filter(|s| !s.trim().is_empty()) {
            body["stand"] = serde_json::Value::String(s.clone());
        }
        if let Some(b) = params.base.as_ref().filter(|s| !s.trim().is_empty()) {
            body["base"] = serde_json::Value::String(b.clone());
        }
        if let Some(m) = params.model.as_ref().filter(|s| !s.trim().is_empty()) {
            body["model"] = serde_json::Value::String(m.clone());
        }
        // lanes portless (doc 27 §3.4.5): 旧 SP HTTP POST /api/lanes を World process-proxy ask
        // `lane_create` に移管。 lane clone は 数 sec ~ 数 10 sec かかるので outer timeout 60s。
        // server Err (CONFLICT="already exists"/"既に存在" 等) は quic_call_with_timeout が
        // McpError に変換して返す (= 旧 HTTP の非 2xx → McpError 経路と等価)。
        let parsed = self
            .quic_call_with_timeout("lane_create", body, Duration::from_secs(60))
            .await?;
        // 成功 body は LaneInfo JSON。 address だけ抽出して短い human 向け text を返す。
        let addr = parsed
            .get("address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                parsed.get("address").and_then(|a| {
                    let proj = a.get("project")?.as_str()?;
                    let nm = a.get("name")?.as_str()?;
                    // doc 44 P2: address 表示形は `<project>/<name>`
                    Some(format!("{}/{}", proj, nm))
                })
            })
            .unwrap_or_else(|| params.name.clone());
        let cwd = parsed.get("cwd").and_then(|v| v.as_str()).unwrap_or("?");
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Performer Lane created: {}\n  cwd: {}", addr, cwd),
        )]))
    }

    /// Delete a Performer Lane in the current project (VP-124 Phase 1).
    ///
    /// 3-step orchestration を 1 call で完結: SP pool removal + child PTY kill + tmux session kill +
    /// (optional) lane workspace dir cleanup。 server-side `delete_lane_orchestrated` への薄い HTTP
    /// wrapper、 cwd ベースで自動的に local SP と project を解決。
    #[tool(
        description = "Delete a Performer Lane in the current project. SP pool removal + child PTY kill + tmux session kill + lane workspace dir cleanup を 1 call で完結 (= 旧来の手動 3 step `vp lane rm` + `tmux kill-session` + `curl -X DELETE` を置換)。 cwd ベースで local SP を自動解決、 cleanup=false で dir 残置 (debug 用途)。 Conductor Lane は削除不可 (architecture rule、 SP shutdown が path)。"
    )]
    async fn delete_performer(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<DeletePerformerParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "name は必須です (空文字不可)".to_string(),
                None,
            ));
        }

        // F6②: 旧 SP 直結 (/api/health + DELETE /api/lanes reqwest) を World process-proxy ask
        // (lane_delete) に移管。 project_name は self.project_path の basename から取得する
        // (SP health round-trip 不要、 port reshuffle で揺れない stable identifier)。 add_performer と
        // 異なり full address を渡す design (DELETE は SP 側で project 補完しない)。
        let project_name = std::path::Path::new(self.project_path.as_str())
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("project_path basename 取得失敗: {}", self.project_path),
                    None,
                )
            })?;
        let address = format!("{}/performer/{}", project_name, params.name);
        let cleanup = params.cleanup.unwrap_or(true);

        // World process-proxy 経由で SP の lane_delete を ask (workspace cleanup 等 orchestration を
        // 含むため outer timeout 30s)。 server Err は quic_call_with_timeout が McpError に変換して返す。
        let payload = serde_json::json!({ "address": address, "cleanup": cleanup });
        match self
            .quic_call_with_timeout("lane_delete", payload, Duration::from_secs(30))
            .await
        {
            Ok(resp) => {
                // 成功 body は DeletedLaneInfo JSON。 human 向けに要点だけ要約。
                let pid = resp
                    .get("pid")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "(no pid)".to_string());
                let cleanup_status = resp
                    .get("cleanup")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(skipped)");
                Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                    format!(
                        "Performer Lane deleted: {}\n  pid: {} (killed)\n  cleanup: {}",
                        address, pid, cleanup_status
                    ),
                )]))
            }
            Err(e) => {
                // 冪等性: 既に無い Performer の delete は SP が DeleteLaneError::LaneNotFound
                // ("Lane not found: ...") を返す → no-op 成功扱い。 真の異常と区別し、 AI agent が
                // 「もう消えてる」 と判別できるようにする (旧 HTTP 404 idempotent path の置換)。
                if e.to_string().contains("Lane not found") {
                    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "Performer Lane already gone (no-op, idempotent): {}",
                            address
                        ),
                    )]))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// List Lanes in the current project with comprehensive routing info (VP-124 Phase 1).
    ///
    /// Conductor Lane Echoes が「lane を operate するすべての座標」 を 1 call で取得するための tool。
    /// GET /api/lanes wrapper、 各 Lane に mailbox_addresses (per-Lane Stands の wire address)、
    /// top-level に project_addresses + world_addresses を synthesize。
    #[tool(
        description = "List all Lanes (Conductor + Performers) in the current project with comprehensive routing info. Each Lane returns: address, kind, state, stand, pid, cwd, tmux session, performer_status, AND mailbox_addresses (= wire-ready addresses for `wire_send`)。 Each lane's mailbox_addresses has two entries: `agent` (= the lane's Claude session inbox, e.g. `agent@vantage-point` for conductor or `agent@vantage-point/chore` for performer 'chore') and `canvas` (= the lane's Canvas / Paisley Park inbox, e.g. `canvas@vantage-point/chore`)。 Top-level also returns project_addresses (e.g. `gold_experience@<project>`) and world_addresses (e.g. `bastet@world`)。 Use this to discover Performers, decide deletion targets, pick wire routes for wire_send。 Replaces multi-step `vp ps` + manual lane inspection。"
    )]
    async fn list_lanes(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ListLanesParams>,
    ) -> Result<CallToolResult, McpError> {
        // lanes portless (doc 27 §3.4.5): project 名は self.project_path から導出 (旧 /api/health
        // round-trip 撤去)。 config 登録名を SSOT とする (basename ではない)。 `vp projects rename`
        // で name != basename になっても、wire store の識別子 (agent@<config名>) と list_lanes が
        // 返す mailbox address を一致させる。旧 basename 由来だと rename 時に永続 mismatch し、その
        // address 宛 command が誰の ack とも一致せず再 nudge する第2のバグ経路だった (identity SSOT)。
        let project = match crate::config::Config::load() {
            Ok(config) => {
                crate::resolve::project_name_from_path(self.project_path.as_str(), &config)
            }
            // config 読めない異常系のみ basename fallback (従来挙動)
            Err(_) => std::path::Path::new(self.project_path.as_str())
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string(),
        };

        // 全 lane を World process-proxy ask `lanes_list` で取得 (旧 GET /api/lanes)。
        let resp = self.quic_call("lanes_list", serde_json::json!({})).await?;

        let lanes_in = resp
            .get("lanes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // フィルタ + mailbox_addresses 注入
        let mut lanes_out: Vec<serde_json::Value> = Vec::new();
        for mut lane in lanes_in.into_iter() {
            // kind / state filter
            //
            // doc 44 P2: lane に種別 field は無くなったため、判定は**名前**で行う
            // （開発起点は予約名 "conductor"、それ以外が旧 performer）。
            // tool の param 名 `kind` は MCP client との契約なので語彙は据え置き。
            if let Some(k) = &params.kind
                && lane_kind_label(&lane) != k.as_str()
            {
                continue;
            }
            if let Some(s) = &params.state
                && lane.get("state").and_then(|v| v.as_str()) != Some(s.as_str())
            {
                continue;
            }

            // mailbox_addresses 計算 (= per-Lane の wire address。VP-166 設計 doc 16)。
            //
            // 各 Lane (conductor / performer) は 2 つの box を持つ:
            //   - `agent#<lane>`  = その lane の Claude session 宛 (= coding-assistant inbox)
            //   - `canvas#<lane>` = その lane の Canvas / PP 宛 (PR-5 で配線)
            // actor 名は `stands.rs` の `id` 体系 (`ECHOES.id = "agent"` / `PAISLEY_PARK.id = "canvas"`)。
            // JoJo 愛称 (`echoes` / `paisley_park`) は表示専用なので wire には出さない。
            // wire syntax は `<stand-id>@<project>/<lane>` (conductor は `/lane` 省略可)。
            // 旧実装の `<JoJo名>.<lane>@<project>` (`.` 区切り) は `parse_address` で弾かれる不正形だった。
            // doc 44 P2: lane 名は `address.name` が唯一の在処（旧 `kind` / 複製 `name` は撤去）。
            let lane_label = lane_name_of(&lane);
            // conductor は `agent@<project>` (lane 省略 = conductor)、performer は `agent@<project>/<name>`
            let lane_suffix = if lane_label == "conductor" {
                String::new()
            } else {
                format!("/{}", lane_label)
            };
            let mailbox = serde_json::json!({
                "agent": format!("agent@{}{}", project, lane_suffix),
                "canvas": format!("canvas@{}{}", project, lane_suffix),
            });
            // VP-166 PR-4: この MCP プロセスの lane と一致する entry に `is_self` を付与
            // (SP は caller を知らないので MCP 側で post-process)。agent は自分の entry を
            // 見つけて mailbox_addresses["agent"] = 自分の正規アドレス、を読める。
            let is_self = lane_label == self.self_lane.lane_name;

            if let Some(obj) = lane.as_object_mut() {
                obj.insert("mailbox_addresses".to_string(), mailbox);
                obj.insert("is_self".to_string(), serde_json::Value::Bool(is_self));
            }
            lanes_out.push(lane);
        }

        // top-level に project / world Stand addresses を synthesize
        let result = serde_json::json!({
            "project": project,
            "lanes": lanes_out,
            "project_addresses": {
                "gold_experience": format!("gold_experience@{}", project),
            },
            "world_addresses": {
                "bastet": "bastet@world",
            },
        });

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// flow_handoff: 新 Performer 作成 + 初手 wire_send + nudge を atomic に
    #[tool(
        description = "Atomic dev-flow handoff: (1) Performer Lane 新規作成、 (2) task_spec を wire_send (= 初手 thread root)、 (3) `nudge=true` (default) 時は lane_nudge で wire_recv を促す。 失敗時は performer 削除で rollback。 既存 3 step (add_performer + wire_send + nudge) を 1 call に圧縮 (= dev-flow P4 = 'handoff' を 1 call で完結)。"
    )]
    async fn flow_handoff(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<FlowHandoffParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "name は必須です (空文字不可)".to_string(),
                None,
            ));
        }
        if params.task_spec.trim().is_empty() {
            return Err(McpError::invalid_params(
                "task_spec は必須です (空文字不可)".to_string(),
                None,
            ));
        }
        let mode = params
            .mode
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("hitl")
            .to_string();
        if mode != "hitl" && mode != "auto" {
            return Err(McpError::invalid_params(
                format!("mode は 'hitl' or 'auto' のみ (got: {})", mode),
                None,
            ));
        }
        let nudge = params.nudge.unwrap_or(true);

        // ── Step 1: Performer 作成 (= add_performer と同型 path、 World process-proxy ask `lane_create`) ──
        // lanes portless (doc 27 §3.4.5): 旧 SP HTTP POST /api/lanes を撤去。 lane clone は
        // 数 sec ~ 数 10 sec かかるので outer timeout 60s。 server Err は quic_call_with_timeout が
        // McpError に変換 (= 旧 HTTP 非 2xx → McpError と等価)。
        let mut create_body = serde_json::json!({ "name": params.name });
        if let Some(b) = params.branch.as_ref().filter(|s| !s.trim().is_empty()) {
            create_body["branch"] = serde_json::Value::String(b.clone());
        }
        if let Some(s) = params.stand.as_ref().filter(|s| !s.trim().is_empty()) {
            create_body["stand"] = serde_json::Value::String(s.clone());
        }
        if let Some(b) = params.base.as_ref().filter(|s| !s.trim().is_empty()) {
            create_body["base"] = serde_json::Value::String(b.clone());
        }
        if let Some(m) = params.model.as_ref().filter(|s| !s.trim().is_empty()) {
            create_body["model"] = serde_json::Value::String(m.clone());
        }
        let lane_info = self
            .quic_call_with_timeout("lane_create", create_body, Duration::from_secs(60))
            .await?;

        // address は string 形式 (例: "vantage-point/performer/feat-api") を期待。
        // 旧 add_performer と同型の fallback 経路で project / name から合成も可能。
        let project_name = lane_info
            .pointer("/address/project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let performer_name = lane_info
            .pointer("/address/name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| params.name.clone());
        let cwd = lane_info
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let derived_branch = lane_info
            .pointer("/address/branch")
            .or_else(|| lane_info.get("branch"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let project_name = match project_name {
            Some(p) => p,
            None => {
                // performer 作成は成功したが address.project が読めない → rollback
                let _ = self
                    .flow_rollback_performer("<unknown>", &performer_name)
                    .await;
                return Err(McpError::internal_error(
                    "lane_create response に address.project がありません".to_string(),
                    None,
                ));
            }
        };

        let performer_address = format!("agent@{}/{}", project_name, performer_name);
        let lane_address = format!("{}/performer/{}", project_name, performer_name);

        // ── Step 2: wire_send (initial task spec を root thread として送信) ──
        // body は { task_spec, mode, priority?, scope_outs? } 等の自由 schema。
        // mode を payload に同梱しておくと、 worker 側が後で判断材料に使える。
        let wire_body = serde_json::json!({
            "kind": "task",
            // R2-b: category は delivery policy selector (command = ack されるまで再掲示対象)
            "category": "command",
            "task_spec": params.task_spec,
            "mode": mode,
        });
        let from = self.self_lane.from_address()?;
        let send_payload = serde_json::json!({
            "from": from,
            "to": [performer_address.clone()],
            "body": wire_body,
            "reply_to": serde_json::Value::Null,
        });
        let send_resp = match self.quic_call("wire_send", send_payload).await {
            Ok(v) => v,
            Err(e) => {
                // rollback: performer を削除して dirty state を残さない
                let _ = self
                    .flow_rollback_performer(&project_name, &performer_name)
                    .await;
                return Err(McpError::internal_error(
                    format!(
                        "flow_handoff: wire_send 失敗 (performer rollback 済): {}",
                        e
                    ),
                    None,
                ));
            }
        };
        let wire_msg_id = send_resp
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        // ── Step 3: nudge — lane_nudge proxy で worker に wire_recv を促す ──
        // tmux decoupling PR1: 旧 resolve_pane + tmux_send_keys の置換。 lane address を
        // 直接渡し、 SP 側 write_nudge が PtySlot に書く。 best-effort — nudge 失敗で handoff
        // 全体は失敗扱いにしない (= wire は届いており worker は自走可、 nudge は immediacy 向上目的)。
        let mut nudge_status = if nudge { "skipped" } else { "off" }.to_string();
        if nudge {
            let nudge_text = "conductor から task が届いています。 mcp__vantage-point__wire_recv で確認、 内容に従って着手してください。 質問は wire_send + reply_to で thread 返信。\n".to_string();
            let send = self
                .quic_call(
                    "lane_nudge",
                    serde_json::json!({
                        "lane": lane_address,
                        "text": nudge_text,
                    }),
                )
                .await;
            nudge_status = match send {
                Ok(_) => "sent".to_string(),
                Err(e) => format!("failed (best-effort): {}", e),
            };
        }

        let result = serde_json::json!({
            "performer_address": performer_address,
            "lane_address": lane_address,
            "wire_msg_id": wire_msg_id,
            "performer_dir": cwd,
            "branch": derived_branch,
            "mode": mode,
            "nudge": nudge_status,
        });
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// flow_progress: parallel work 集約 view (read-only)
    #[tool(
        description = "Parallel work 集約 view: 現 project の全 Lane (conductor + performers) の performer_status (git ahead/behind/dirty/merged) と per-lane 未読 wire 数を 1 view で返す。 read-only (= cursor は触らない)、 cache OK。 dev-flow P5 (= 並列追跡) で list_lanes + wire_recv + tmux_capture を別々に叩く代替。"
    )]
    async fn flow_progress(
        &self,
        rmcp::handler::server::wrapper::Parameters(_params): rmcp::handler::server::wrapper::Parameters<FlowProgressParams>,
    ) -> Result<CallToolResult, McpError> {
        // lanes portless (doc 27 §3.4.5): project 名は self.project_path から導出 (旧 /api/health
        // round-trip 撤去)。 config 登録名を SSOT とする (basename ではない)。 `vp projects rename`
        // で name != basename になっても、wire store の識別子 (agent@<config名>) と mailbox address を
        // 一致させる。旧 basename 由来だと rename 時に永続 mismatch し、 ack が一致せず再 nudge する
        // 第2のバグ経路だった (wiremsg identity SSOT 一本化)。 list_lanes と同型。
        let project = match crate::config::Config::load() {
            Ok(config) => {
                crate::resolve::project_name_from_path(self.project_path.as_str(), &config)
            }
            // config 読めない異常系のみ basename fallback (従来挙動)
            Err(_) => std::path::Path::new(self.project_path.as_str())
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string(),
        };

        // 全 lane (conductor + performers) を World process-proxy ask `lanes_list` で取得
        // (= performer_status 込み、 旧 GET /api/lanes)。
        let lanes_resp = self.quic_call("lanes_list", serde_json::json!({})).await?;
        let lanes_in = lanes_resp
            .get("lanes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut performers: Vec<serde_json::Value> = Vec::new();
        let mut conductor_unread: u64 = 0;
        let mut conductor_unread_by_thread = serde_json::Value::Object(Default::default());
        for lane in lanes_in {
            // doc 44 P2: 名前の在処は `address.name` のみ、開発起点は予約名で判る。
            let lane_label = lane_name_of(&lane);
            let kind = lane_kind_label(&lane);
            let agent_addr = if kind == "conductor" {
                format!("agent@{}", project)
            } else {
                format!("agent@{}/{}", project, lane_label)
            };

            // wire unread count (cursor 不触り = read-only)
            let unread_resp = self
                .quic_call(
                    "wire_unread_count",
                    serde_json::json!({ "agent": agent_addr }),
                )
                .await;
            let (unread_total, by_thread) = match unread_resp {
                Ok(v) => (
                    v.get("total").and_then(|x| x.as_u64()).unwrap_or(0),
                    v.get("by_thread")
                        .cloned()
                        .unwrap_or(serde_json::Value::Object(Default::default())),
                ),
                Err(_) => (0, serde_json::Value::Object(Default::default())),
            };

            if kind == "conductor" {
                conductor_unread = unread_total;
                conductor_unread_by_thread = by_thread;
                continue;
            }

            // performer entry を整形 (= performer_status を浅く展開)
            let state = lane
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let stand = lane
                .get("stand")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cwd = lane.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
            let performer_status = lane
                .get("performer_status")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            // 5-state FSM derive (= 2026-05-28 conductor 説示 control surrender model)。
            // 最新 wire activity + performer_status から flow_state を推論、 control_surrender も derive。
            let latest_resp = self
                .quic_call(
                    "wire_latest_msg",
                    serde_json::json!({ "agent": agent_addr }),
                )
                .await;
            let latest_view = latest_resp
                .ok()
                .as_ref()
                .and_then(|v| v.get("message"))
                .and_then(crate::flow::LatestMsgView::from_json);
            let performer_status_view =
                crate::flow::PerformerStatusView::from_json(&performer_status);
            // AwaitingUser 判定: 未 ack needs_user (best-effort、 失敗は None = 判定 off で degrade)
            let needs_user_view = self
                .quic_call(
                    "wire_needs_user_pending",
                    serde_json::json!({ "agent": agent_addr }),
                )
                .await
                .ok()
                .as_ref()
                .and_then(|v| v.get("message"))
                .and_then(crate::flow::LatestMsgView::from_json);
            let fsm = crate::flow::derive_flow_state(
                latest_view.as_ref(),
                performer_status_view,
                &agent_addr,
                needs_user_view.as_ref(),
            );

            performers.push(serde_json::json!({
                "name": lane_label,
                "address": format!("agent@{}/{}", project, lane_label),
                "state": state,
                "stand": stand,
                "cwd": cwd,
                "performer_status": performer_status,
                "unread_wire_count": unread_total,
                "unread_by_thread": by_thread,
                "flow_state": fsm.state,
                "control_surrender": fsm.control_surrender,
                "state_reason": fsm.state_reason,
                "last_state_transition_at": fsm.last_state_transition_at,
            }));
        }

        let result = serde_json::json!({
            "project": project,
            "conductor": {
                "address": format!("agent@{}", project),
                "unread_wire_count": conductor_unread,
                "unread_by_thread": conductor_unread_by_thread,
            },
            "performers": performers,
        });
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }
}
