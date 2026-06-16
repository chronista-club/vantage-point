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

/// Parameters for the show tool
///
/// ## doc 19 PP Canvas Stack Model (2026-05-27)
///
/// `append` field は spec から omit。 mcp__show は **canvas に新 item を push** する
/// semantic に統一されたため、 「既存に追記」 は新 item 化で表現する。
/// 外部 MCP client が `append: true` を送ってきても serde が unknown field を silent
/// ignore (= backward compat)、 stack model 上は無効。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// Content to display
    #[schemars(description = "Content to display (markdown, html, or plain text)")]
    pub content: String,

    /// Content type (markdown, html, log, url)
    #[schemars(
        description = "Content type: 'markdown' (default), 'html', 'log', or 'url' (display a web page in an iframe)"
    )]
    pub content_type: Option<String>,

    /// Pane ID
    ///
    /// doc 19 PP Canvas Stack Model: vp-app の canvas-handler は pane_id を無視して
    /// 全 show を PP body の stack に集約する (= dead field、 backward compat のため
    /// 残置)。 v2 で削除候補。
    #[schemars(description = "Pane ID (currently ignored; reserved for future)")]
    pub pane_id: Option<String>,

    /// Pane title (for tab display)
    #[schemars(description = "Title for the pane tab. If not provided, the pane_id is used.")]
    pub title: Option<String>,
}

/// Parameters for the clear tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClearParams {
    /// Pane ID to clear
    #[schemars(description = "Pane ID to clear (default: 'main')")]
    pub pane_id: Option<String>,
}

/// Parameters for the restart tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RestartParams {
    /// Whether to open WebView after restart (default: false for headless)
    #[schemars(description = "Open WebView window after restart (default: false)")]
    pub open_viewer: Option<bool>,
}

/// Parameters for the toggle_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TogglePaneParams {
    /// Pane ID to toggle ("left" or "right")
    #[schemars(description = "Pane ID to toggle: 'left' for left panel, 'right' for right panel")]
    pub pane_id: String,

    /// Explicit visibility state
    #[schemars(
        description = "Set explicit visibility: true = show, false = hide. If not provided, toggles current state."
    )]
    pub visible: Option<bool>,
}

/// Parameters for the close_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClosePaneParams {
    /// Pane ID to close
    #[schemars(description = "ID of the pane to close")]
    pub pane_id: String,
}

/// Parameters for wire_send tool (Phase A ①: threaded wiremsg inbox)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WireSendParams {
    /// 宛先 agent address 群
    #[schemars(
        description = "Destination agent addresses (e.g. ['agent@vantage-point', 'agent@vantage-point/chore']). At least one recipient is expected for a new thread."
    )]
    pub to: Vec<String>,

    /// メッセージ本文（JSON object）
    //
    // `serde_json::Value` のまま JsonSchema を導出すると type 無しの schema になり、
    // MCP client が body を string と解釈して JSON 文字列で送ってしまう (= SurrealDB
    // の `wire_messages.body TYPE object` で reject される)。 `with` で object 型の
    // schema を明示し、 client に object を送らせる。 Rust 型は Value のまま保持し、
    // string で来た場合は handle_wire_send の coerce_wire_body が救済する。
    #[schemars(
        description = "Message body as a JSON object.",
        with = "std::collections::HashMap<String, serde_json::Value>"
    )]
    pub body: serde_json::Value,

    /// 返信先メッセージID（指定すると既存 thread への reply になる）
    #[schemars(
        description = "If set, this message is a reply within the existing thread of the given message ID (a wire message id returned by a previous wire_send / wire_recv). If omitted, a new thread is started."
    )]
    pub reply_to: Option<String>,
}

/// Parameters for wire_recv tool (Phase A ①: threaded wiremsg inbox)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WireRecvParams {
    /// 受信タイムアウト（秒）
    #[schemars(
        description = "Timeout in seconds to wait for unread messages (default: 5, max: 30). Returns immediately if unread messages exist."
    )]
    pub timeout: Option<u64>,
}

/// Parameters for wire_inbox tool (refactor R1 PR-B: 在庫確認、 cursor 非破壊)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WireInboxParams {}

/// Parameters for wire_thread tool (R2: ancestor-chain 取得)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WireThreadParams {
    /// 系譜を辿る起点となる wire message id
    #[schemars(
        description = "The wire message id (returned by a previous wire_send / wire_recv) to trace ancestors from."
    )]
    pub message_id: String,
}

/// Parameters for wire_ack tool (R2-a: per-message ack 台帳、 決定 D3)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WireAckParams {
    /// ack する wire message id
    #[schemars(
        description = "The wire message id (returned by wire_recv) to acknowledge. Ack the message after you have actually handled it."
    )]
    pub message_id: String,
}

/// Parameters for the watch_file tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WatchFileParams {
    /// File path to watch
    #[schemars(description = "Absolute path to the log file to watch")]
    pub path: String,

    /// Pane ID to display logs in
    #[schemars(description = "Pane ID to display watched logs in")]
    pub pane_id: String,

    /// Log format
    #[schemars(description = "Log format: 'json_lines' (default) or 'plain'")]
    pub format: Option<String>,

    /// Level filter regex
    #[schemars(description = "Regex to filter log levels, e.g. 'INFO|WARN|ERROR'")]
    pub filter: Option<String>,

    /// Targets to exclude
    #[schemars(description = "List of target names to exclude from display")]
    pub exclude_targets: Option<Vec<String>>,

    /// Pane title
    #[schemars(description = "Title for the pane tab")]
    pub title: Option<String>,

    /// Display style
    #[schemars(description = "Display style: 'terminal' (default) or 'plain'")]
    pub style: Option<String>,
}

/// Parameters for the unwatch_file tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UnwatchFileParams {
    /// Pane ID to stop watching
    #[schemars(description = "Pane ID to stop file watching for")]
    pub pane_id: String,
}

/// Parameters for the switch_lane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SwitchLaneParams {
    /// Lane name (project name) to switch to
    #[schemars(
        description = "Lane name (project name) to switch the Canvas to. e.g. 'vantage-point', 'creo-memories'"
    )]
    pub lane: String,
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
        description = "Lane Stand 種類: 'echoes' (default、 Claude CLI) or 'shell' (shell)。"
    )]
    pub stand: Option<String>,
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
/// P4 (= 3-step orchestration: add_performer + wire_send + tmux send-keys) を atomic 1 step に圧縮する。
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
    #[schemars(description = "Lane Stand: 'echoes' (default、 Claude CLI) or 'shell'。")]
    #[serde(default)]
    pub stand: Option<String>,

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

    /// Nudge enable (default: true)。 false で tmux send-keys を skip。
    #[schemars(
        description = "tmux send-keys で wire_recv 受信を促す nudge を発火するか (default: true)。 false で send のみ実行 (= 完全 async)。"
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

/// Parameters for the eval_ruby tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EvalRubyParams {
    /// Ruby code to execute
    #[schemars(description = "Ruby code to execute (mutually exclusive with 'file')")]
    pub code: Option<String>,

    /// Ruby file path to execute (relative to project dir)
    #[schemars(
        description = "Ruby file path to execute, relative to project directory (mutually exclusive with 'code')"
    )]
    pub file: Option<String>,

    /// Pane ID to display results in
    #[schemars(description = "Pane ID to display results in (default: 'main')")]
    pub pane_id: Option<String>,
}

/// Parameters for the port_roles tool (no args、 placeholder for schemars→rmcp 1.6 compat).
///
/// schemars 1.x は `Parameters<()>` を `{const: null}` schema として generate するが、
/// rmcp 1.6 + MCP spec は inputSchema に `{type: "object"}` を必須化。 空 struct で
/// `{type: "object", properties: {}}` を生成させて MCP client validation を通過させる。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortRolesParams {}

/// Parameters for the run_ruby tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RunRubyParams {
    /// Ruby code to run as daemon
    #[schemars(
        description = "Ruby code to run as a long-running daemon process (mutually exclusive with 'file')"
    )]
    pub code: Option<String>,

    /// Ruby file path to run as daemon (relative to project dir)
    #[schemars(
        description = "Ruby file path to run as daemon, relative to project directory (mutually exclusive with 'code')"
    )]
    pub file: Option<String>,

    /// Process display name
    #[schemars(description = "Display name for the process (default: filename or 'daemon')")]
    pub name: Option<String>,

    /// Pane ID to stream output to
    #[schemars(description = "Pane ID to stream output to (default: 'main')")]
    pub pane_id: Option<String>,
}

/// Parameters for the stop_ruby tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StopRubyParams {
    /// Process ID to stop
    #[schemars(
        description = "Ruby process ID to stop (e.g. 'rb-0001'). Use list_ruby to see running processes."
    )]
    pub process_id: String,
}

/// Parameters for the capture_canvas tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptureCanvasParams {
    /// Save path
    #[schemars(
        description = "Save path for the PNG screenshot (default: /tmp/vp-canvas-{timestamp}.png)"
    )]
    pub path: Option<String>,

    /// Capture specific pane only
    #[schemars(description = "Capture only a specific pane by its pane_id")]
    pub pane_id: Option<String>,
}

/// Parameters for the read_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadPaneParams {
    /// pane_id to read (省略時: pane が 1 つだけならそれを返す)
    #[schemars(
        description = "The pane_id to read. If omitted and exactly one pane is on the Canvas, that pane is returned."
    )]
    pub pane_id: Option<String>,
}

/// Canvas の 1 pane の最新内容 (= retained Show、 list_canvas / read_pane の共通中間表現)
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CanvasPane {
    pub pane_id: String,
    pub content_type: String,
    pub content: String,
    pub title: Option<String>,
}

/// "canvas" channel の retained Show payload (ProcessMessage::Show JSON) を CanvasPane に
/// parse する (純関数)。
///
/// Show 以外 (clear / toggle 等) や形不正は None。 Content は外部タグ enum なので
/// `content` object の唯一の key が content_type (markdown/html/log/url)、 値が本文。
pub(crate) fn parse_show_payload(v: &serde_json::Value) -> Option<CanvasPane> {
    if v.get("type")?.as_str()? != "show" {
        return None;
    }
    let pane_id = v.get("pane_id")?.as_str()?.to_string();
    let content_obj = v.get("content")?.as_object()?;
    let (content_type, content_val) = content_obj.iter().next()?;
    let content = content_val.as_str().unwrap_or("").to_string();
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    Some(CanvasPane {
        pane_id,
        content_type: content_type.clone(),
        content,
        title,
    })
}

#[cfg(test)]
mod canvas_read_tests {
    use super::*;

    #[test]
    fn parse_show_extracts_pane() {
        let v = serde_json::json!({
            "type": "show", "pane_id": "main",
            "content": {"markdown": "# Hello"}, "append": false, "title": "My Pane"
        });
        let p = parse_show_payload(&v).expect("show parse");
        assert_eq!(p.pane_id, "main");
        assert_eq!(p.content_type, "markdown");
        assert_eq!(p.content, "# Hello");
        assert_eq!(p.title.as_deref(), Some("My Pane"));
    }

    #[test]
    fn parse_show_html_without_title() {
        let v = serde_json::json!({
            "type": "show", "pane_id": "side",
            "content": {"html": "<b>x</b>"}, "append": false
        });
        let p = parse_show_payload(&v).expect("show parse");
        assert_eq!(p.content_type, "html");
        assert_eq!(p.title, None);
    }

    #[test]
    fn parse_rejects_non_show_and_malformed() {
        assert!(
            parse_show_payload(&serde_json::json!({"type":"clear","pane_id":"main"})).is_none()
        );
        assert!(parse_show_payload(&serde_json::json!({"foo": 1})).is_none());
    }
}

/// capture_terminal ツールのパラメータ
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptureTerminalParams {
    /// 保存先パス
    #[schemars(
        description = "Save path for the PNG screenshot (default: /tmp/vp-terminal-{timestamp}.png)"
    )]
    pub path: Option<String>,
}

/// tmux ペイン分割のパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxSplitParams {
    /// 水平分割 (true) or 垂直分割 (false)。デフォルト: true
    #[schemars(description = "Horizontal split (true, default) or vertical split (false)")]
    pub horizontal: Option<bool>,
    /// 新しいペインで実行するコマンド（省略するとデフォルトシェル）
    #[schemars(
        description = "Command to run in the new pane (e.g. 'claude --dangerously-skip-permissions'). Defaults to shell."
    )]
    pub command: Option<String>,
    /// コンテンツ種別: "shell" (The Hand ✋), "agent"/"echoes" (Echoes 💬、 旧 HD 📖), "canvas"/"pp" (Paisley Park 🧭)
    #[schemars(
        description = "Content type for the new pane: 'shell' (The Hand, default shell), 'agent'/'echoes' (Echoes 💬, Claude CLI; 'hd' legacy alias), 'canvas'/'pp' (Paisley Park). Overridden by 'command' if both specified."
    )]
    pub content_type: Option<String>,
}

/// tmux ペインキャプチャのパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxCaptureParams {
    /// ペイン ID（例: %0）またはエージェント label。省略すると全ペインをキャプチャ。
    #[schemars(
        description = "Pane ID (e.g. %0) or agent label (e.g. 'Moody Blues'). If omitted, captures all panes."
    )]
    pub pane_id: Option<String>,
}

/// エージェントデプロイのパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxAgentDeployParams {
    /// エージェント名（"Moody Blues", "Sticky Fingers" 等）
    #[schemars(description = "Agent label (e.g. 'Moody Blues', 'Sticky Fingers')")]
    pub label: String,
    /// 新しいペインで実行するコマンド
    #[schemars(
        description = "Command to run in the new pane (e.g. 'claude --dangerously-skip-permissions')"
    )]
    pub command: Option<String>,
    /// 実行中タスクの説明
    #[schemars(description = "Description of the task this agent is performing")]
    pub task: Option<String>,
    /// 水平分割 (true) or 垂直分割 (false)。デフォルト: true
    #[schemars(description = "Horizontal split (true, default) or vertical split (false)")]
    pub horizontal: Option<bool>,
}

/// エージェントステータス更新のパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxAgentStatusParams {
    /// ペイン ID またはエージェント label
    #[schemars(description = "Pane ID (e.g. %3) or agent label (e.g. 'Moody Blues')")]
    pub pane_id: String,
    /// ステータス（"running", "waiting", "done", "error"）
    #[schemars(description = "Agent status: 'running', 'waiting', 'done', or 'error'")]
    pub status: String,
    /// タスク説明（更新する場合）
    #[schemars(description = "Updated task description (optional)")]
    pub task: Option<String>,
}

/// エージェントへのテキスト送信パラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxAgentSendParams {
    /// ペイン ID またはエージェント label
    #[schemars(description = "Pane ID (e.g. %3) or agent label (e.g. 'Moody Blues')")]
    pub pane_id: String,
    /// 送信するテキスト（改行はそのまま送信される）
    #[schemars(description = "Text to send to the pane (newlines are sent as-is)")]
    pub text: String,
}

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
    /// `"conductor"` or `"<performer-name>"`（flat 名）
    pub lane_name: String,
    /// performer context のとき `Some(parent project 名)`、conductor context のとき `None`
    pub performer_parent: Option<String>,
}

impl SelfLane {
    /// cwd から SelfLane を判定。 失敗時（cwd 取れない / config 読めない 等）は conductor 扱い。
    ///
    /// 1. cwd ancestors を walk して `.vp/lanes/<name>` pattern を探す
    ///    (= [`detect_project_local_performer`] の純粋関数で test 可能)
    /// 2. 見つかれば repo_root を config.projects[].path と完全一致で resolve → parent 確定
    /// 3. どちらか失敗 → conductor fallback
    pub fn detect() -> Self {
        let conductor = || SelfLane {
            lane_name: "conductor".to_string(),
            performer_parent: None,
        };
        let Ok(cwd) = std::env::current_dir() else {
            return conductor();
        };
        let Some((performer_name, repo_root)) = detect_project_local_performer(&cwd) else {
            return conductor();
        };
        let Ok(config) = crate::config::Config::load() else {
            return conductor();
        };
        let parent = config
            .projects
            .iter()
            .find(|p| std::path::Path::new(&p.path) == repo_root.as_path());
        match parent {
            Some(p) => SelfLane {
                lane_name: performer_name,
                performer_parent: Some(p.name.clone()),
            },
            None => conductor(), // performer dir 検出済だが config に repo 未登録 → 安全 fallback
        }
    }

    /// `wire_send` / `wire_recv` の `from` フィールド値: conductor は `"agent"`（bare）、
    /// performer は `"agent@<parent>/<name>"`。
    ///
    /// bare は SP 入口（`unison_server.rs::normalize_agent_addr`、wiremsg N1）で
    /// `agent@<project>` に正規化される。MCP プロセスは conductor 時に project 名を
    /// 持たないため、canonical 化は project 名を知る SP 側の責務とする。
    pub fn from_address(&self) -> String {
        match &self.performer_parent {
            Some(parent) => format!("agent@{}/{}", parent, self.lane_name),
            None => "agent".to_string(),
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
    /// Unison "process" チャネル（lazy 接続、canvas 操作も含む）
    process_channel: Arc<Mutex<Option<Arc<unison::network::channel::UnisonChannel>>>>,
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
            self_lane: self.self_lane.clone(),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl VantageMcp {
    pub fn new(process_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            process_url: Arc::new(Mutex::new(format!("http://[::1]:{}", process_port))),
            process_port: Arc::new(Mutex::new(process_port)),
            process_channel: Arc::new(Mutex::new(None)),
            self_lane: SelfLane::detect(),
            tool_router: Self::tool_router(),
        }
    }

    /// Process に HTTP POST でメッセージを送信
    ///
    /// `endpoint` は `/api/show` 等の API パス。
    /// `body` は JSON シリアライズ可能なペイロード。
    ///
    /// 接続失敗時は Process ポートを再解決してリトライする（lazy reconnect）。
    async fn http_post(
        &self,
        endpoint: &str,
        body: &impl Serialize,
    ) -> Result<serde_json::Value, McpError> {
        use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

        let tid = new_trace_id();
        let start = std::time::Instant::now();
        let url = format!("{}{}", self.process_url.lock().await, endpoint);

        write_trace(
            &TraceEntry::new("mcp", &tid, "request", "INFO", format!("POST {}", endpoint))
                .with_data(serde_json::to_value(body).unwrap_or_default()),
        );

        let resp = match self
            .client
            .post(&url)
            .json(body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_connect() => {
                // 接続失敗 → ポートを再解決してリトライ
                let new_url = self.try_reconnect(endpoint).await;
                if let Some(retry_url) = new_url {
                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "reconnect",
                        "INFO",
                        format!("Process 再検出: {}", retry_url),
                    ));
                    self.client
                        .post(&retry_url)
                        .json(body)
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                        .map_err(|e2| {
                            McpError::internal_error(
                                format!("Process 通信失敗 ({}): {}. Is vp running?", endpoint, e2),
                                None,
                            )
                        })?
                } else if let Some(auto_url) = self.auto_start_process(endpoint).await {
                    // running.json にも見つからない → Process を自動起動
                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "auto_start",
                        "INFO",
                        format!("Process 自動起動後リトライ: {}", auto_url),
                    ));
                    self.client
                        .post(&auto_url)
                        .json(body)
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                        .map_err(|e2| {
                            McpError::internal_error(
                                format!("Process 通信失敗 ({}): {}. Process auto-start succeeded but request failed.", endpoint, e2),
                                None,
                            )
                        })?
                } else {
                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "error",
                        "ERROR",
                        format!("POST {} 失敗（自動起動も失敗）: {}", endpoint, e),
                    ));
                    return Err(McpError::internal_error(
                        format!(
                            "Process 通信失敗 ({}): {}. Auto-start failed. Run `vp sp start` manually.",
                            endpoint, e
                        ),
                        None,
                    ));
                }
            }
            Err(e) => {
                write_trace(&TraceEntry::new(
                    "mcp",
                    &tid,
                    "error",
                    "ERROR",
                    format!("POST {} 失敗: {}", endpoint, e),
                ));
                return Err(McpError::internal_error(
                    format!("Process 通信失敗 ({}): {}. Is vp running?", endpoint, e),
                    None,
                ));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            write_trace(&TraceEntry::new(
                "mcp",
                &tid,
                "error",
                "ERROR",
                format!("POST {} HTTP {}", endpoint, status),
            ));
            return Err(McpError::internal_error(
                format!("Process returned HTTP {}: {}", status, endpoint),
                None,
            ));
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("レスポンスのパースに失敗: {}", e), None)
        })?;

        write_trace(
            &TraceEntry::new(
                "mcp",
                &tid,
                "response",
                "INFO",
                format!("POST {} OK", endpoint),
            )
            .with_elapsed(start.elapsed().as_millis() as u64),
        );

        Ok(json)
    }

    /// Unison QUIC チャネルを取得（lazy 接続）
    ///
    /// チャネルが未接続または切断済みの場合、新規接続して返す。
    async fn get_quic_channel(
        &self,
        channel_slot: &Arc<Mutex<Option<Arc<unison::network::channel::UnisonChannel>>>>,
        channel_name: &str,
    ) -> Result<Arc<unison::network::channel::UnisonChannel>, McpError> {
        let mut guard = channel_slot.lock().await;

        // 既存チャネルがあれば再利用
        if let Some(ch) = guard.as_ref() {
            return Ok(Arc::clone(ch));
        }

        // 新規接続。 startup 時に解決した process_port は、 discovery 一時障害や SP
        // 未起動のタイミングだと 33000 fallback を掴んでいることがある。 connect に
        // 失敗したら discovery で port を引き直し、 1 回だけリトライする (stale port
        // self-heal — HTTP 経路の try_reconnect と対称)。
        let port = *self.process_port.lock().await;
        let client = match connect_quic(&quic_addr(port)).await {
            Ok(client) => client,
            Err(first_err) => match self.rediscover_process_port().await {
                Some(fresh_port) => connect_quic(&quic_addr(fresh_port)).await?,
                None => return Err(first_err),
            },
        };
        // unison 内部の request timeout は default 30s。 `quic_call_with_timeout` の outer
        // timeout (wire_recv で server_timeout + buffer = 最大 35s) より長く取らないと
        // unison 側が先に発火してしまうので、 余裕を持って 60s に引き上げる (VP-163)。
        let channel = Arc::new(
            client
                .open_channel(channel_name)
                .await
                .map_err(|e| {
                    McpError::internal_error(
                        format!("Unison {} channel error: {}", channel_name, e),
                        None,
                    )
                })?
                .with_request_timeout(std::time::Duration::from_secs(60)),
        );

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

    /// discovery で Process port を引き直し、 startup 時の値と違えば `process_port` /
    /// `process_url` を更新して新 port を返す。 変わらなければ `None`。
    ///
    /// startup 時の [`resolve_process_port`] は discovery 一時障害 / SP 未起動の
    /// タイミングだと 33000 fallback を掴む。 接続失敗時にこれを呼んで self-heal する。
    /// 注: `process_channel` は touch しない — channel lock を保持する
    /// [`Self::get_quic_channel`] からも呼ばれるため (deadlock 回避)。 channel の
    /// 張り直しは呼び出し側の責務。
    async fn rediscover_process_port(&self) -> Option<u16> {
        let info = crate::discovery::find_for_cwd().await?;
        let mut port_guard = self.process_port.lock().await;
        if *port_guard == info.port {
            return None;
        }
        *port_guard = info.port;
        *self.process_url.lock().await = format!("http://[::1]:{}", info.port);
        Some(info.port)
    }

    /// Process ポートを再解決し、変わっていれば URL を更新してリトライ用 URL を返す。
    ///
    /// HTTP 経路の接続失敗時に呼ばれる。 port 解決は [`Self::rediscover_process_port`]
    /// に集約。 port が変わった時のみ QUIC チャネルもリセットしてリトライ URL を返す。
    async fn try_reconnect(&self, endpoint: &str) -> Option<String> {
        let new_port = self.rediscover_process_port().await?;
        // ポートが変わったので QUIC チャネルもリセット
        *self.process_channel.lock().await = None;
        Some(format!("http://[::1]:{}{}", new_port, endpoint))
    }

    /// label または pane_id を受け取り、(pane_id, 表示名) を返す
    ///
    /// `%` で始まる場合もそうでない場合も `tmux_resolve_pane` を1回呼ぶ。
    /// サーバー側で pane_id → 即返却 + meta 取得、label → 逆引き + meta 取得を統一処理。
    async fn resolve_pane(&self, query: &str) -> Result<(String, String), McpError> {
        if query.starts_with('%') {
            // pane_id → meta を取得して表示名を生成
            let display = match self
                .quic_call("tmux_resolve_pane", serde_json::json!({"query": query}))
                .await
            {
                Ok(resp) => {
                    if let Some(label) = resp.pointer("/meta/label").and_then(|v| v.as_str()) {
                        format!("{} ({})", label, query)
                    } else {
                        query.to_string()
                    }
                }
                Err(_) => query.to_string(),
            };
            return Ok((query.to_string(), display));
        }
        let resp = self
            .quic_call("tmux_resolve_pane", serde_json::json!({"query": query}))
            .await?;
        let pane_id = resp
            .get("pane_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                McpError::invalid_params(format!("ペインが見つかりません: {}", query), None)
            })?;
        let label = resp.pointer("/meta/label").and_then(|v| v.as_str());
        let display = match label {
            Some(l) => format!("{} ({})", l, pane_id),
            None => pane_id.clone(),
        };
        Ok((pane_id, display))
    }

    /// Process が見つからない場合に自動起動する
    ///
    /// `vp sp start` をバックグラウンドで spawn し、
    /// health check ポーリングで起動完了を待つ。
    /// 成功したら `process_url` を更新し、新しい URL を返す。
    async fn auto_start_process(&self, endpoint: &str) -> Option<String> {
        use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

        let tid = new_trace_id();
        let cwd = std::env::current_dir().ok()?;
        let cwd_str = cwd.display().to_string();

        write_trace(&TraceEntry::new(
            "mcp",
            &tid,
            "auto_start",
            "INFO",
            format!("Process 自動起動: project_dir={}", cwd_str),
        ));

        // vp sp start をデタッチ実行
        let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());
        let spawn_result = std::process::Command::new(&vp_bin)
            .args(["sp", "start", "-C", &cwd_str])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        if let Err(e) = spawn_result {
            write_trace(&TraceEntry::new(
                "mcp",
                &tid,
                "auto_start",
                "ERROR",
                format!("vp sp start spawn 失敗: {}", e),
            ));
            return None;
        }

        // running.json からポートを取得し、health check で起動完了を確認
        // 最大 5 秒（200ms × 25回）
        let poll_interval = Duration::from_millis(200);
        let max_attempts = 25;

        for _ in 0..max_attempts {
            tokio::time::sleep(poll_interval).await;

            // 稼働中 Process を検索（TheWorld API → HTTP スキャンフォールバック）
            let process_info = match crate::discovery::find_for_cwd().await {
                Some(info) => info,
                None => continue,
            };

            let new_base = format!("http://[::1]:{}", process_info.port);
            let health_url = format!("{}/api/health", new_base);

            // health check
            match self
                .client
                .get(&health_url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    // 起動完了 — process_url / process_port を更新、QUIC チャネルもリセット
                    let mut current = self.process_url.lock().await;
                    *current = new_base.clone();
                    *self.process_port.lock().await = process_info.port;
                    *self.process_channel.lock().await = None;

                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "auto_start",
                        "INFO",
                        format!("Process 自動起動成功: port={}", process_info.port),
                    ));

                    return Some(format!("{}{}", new_base, endpoint));
                }
                _ => continue,
            }
        }

        write_trace(&TraceEntry::new(
            "mcp",
            &tid,
            "auto_start",
            "ERROR",
            "Process 自動起動タイムアウト（5秒）".to_string(),
        ));

        None
    }

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

    /// Canvas の表示 Lane を切り替える
    #[tool(
        description = "Switch the active lane (project) in the PP Canvas window. The lane name is the project name shown in the lane bar."
    )]
    async fn switch_lane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<SwitchLaneParams>,
    ) -> Result<CallToolResult, McpError> {
        // TheWorld の HTTP API 経由で Canvas に switch_lane を送信
        let world_port = crate::cli::WORLD_PORT;
        let url = format!("http://[::1]:{}/api/canvas/switch_lane", world_port);
        let body = serde_json::json!({ "lane": params.lane });

        let client = reqwest::Client::new();
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                    format!("Switched Canvas lane to '{}'", params.lane),
                )]))
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                Err(McpError::internal_error(
                    format!("TheWorld API error: {} {}", status, text),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to reach TheWorld: {}", e),
                None,
            )),
        }
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
        let mut body = serde_json::json!({
            "kind": "performer",
            "name": params.name,
        });
        if let Some(b) = params.branch.as_ref().filter(|s| !s.trim().is_empty()) {
            body["branch"] = serde_json::Value::String(b.clone());
        }
        if let Some(s) = params.stand.as_ref().filter(|s| !s.trim().is_empty()) {
            body["stand"] = serde_json::Value::String(s.clone());
        }
        let url = format!("{}/api/lanes", self.process_url.lock().await);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(60)) // lane clone は 数 sec ~ 数 10 sec かかる
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // server からのエラー文を素直に伝える (CONFLICT = 名前重複等)
            return Err(McpError::internal_error(
                format!("SP /api/lanes {}: {}", status, text),
                None,
            ));
        }
        // 成功 body は LaneInfo JSON。 address だけ抽出して短い human 向け text を返す。
        let parsed: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let addr = parsed
            .get("address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                parsed.get("address").and_then(|a| {
                    let proj = a.get("project")?.as_str()?;
                    let nm = a.get("name")?.as_str()?;
                    Some(format!("{}/performer/{}", proj, nm))
                })
            })
            .unwrap_or_else(|| format!("performer/{}", params.name));
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

        // SP の project name を /api/health から取得 (project_dir basename = project name)。
        // address 構築のため必要、 add_performer と異なり POST body に name 1 つだけ渡せば SP 側で
        // project 補完される pattern が使えない (DELETE は full address を query で受ける design)。
        let process_url = self.process_url.lock().await.clone();
        let health = self
            .client
            .get(format!("{}/api/health", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("/api/health parse 失敗: {}", e), None)
            })?;
        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::internal_error("/api/health に project_dir なし".to_string(), None)
            })?;
        let project_name = std::path::Path::new(project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("project_dir の basename 取得失敗: {}", project_dir),
                    None,
                )
            })?;

        let address = format!("{}/performer/{}", project_name, params.name);
        let cleanup = params.cleanup.unwrap_or(true);

        // reqwest 0.12 の RequestBuilder.query は &[(impl Serialize, impl Serialize)] を取るが、
        // `&str` の tuple slice の type 推論が出ないので manual percent-encoding で URL 構築。
        // address 内の `/` は `%2F` にする以外は ASCII safe (project name / performer name は git
        // branch 互換 slug のため英数 + dash + underscore のみ)。
        let address_enc = address.replace('/', "%2F");
        let url = format!(
            "{}/api/lanes?address={}&cleanup={}",
            process_url, address_enc, cleanup
        );
        let resp = self
            .client
            .delete(&url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("DELETE /api/lanes 失敗: {}", e), None)
            })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // 冪等性: 既に無い Performer の delete (SP は LaneNotFound → 404) は no-op 成功扱い。
        // 真の異常 (500 等) と区別し、 AI agent が「もう消えてる」と判別できるようにする。
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                format!(
                    "Performer Lane already gone (no-op, idempotent): {}",
                    address
                ),
            )]));
        }
        if !status.is_success() {
            return Err(McpError::internal_error(
                format!("SP DELETE /api/lanes {}: {}", status, text),
                None,
            ));
        }

        // 成功 body は DeletedLaneInfo JSON。 human 向けに要点だけ要約。
        let parsed: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let pid = parsed
            .get("pid")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(no pid)".to_string());
        let tmux_killed = parsed
            .get("tmux_killed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cleanup_status = parsed
            .get("cleanup")
            .and_then(|v| v.as_str())
            .unwrap_or("(skipped)");

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Performer Lane deleted: {}\n  pid: {} (killed)\n  tmux_killed: {}\n  cleanup: {}",
                address, pid, tmux_killed, cleanup_status
            ),
        )]))
    }

    /// List Lanes in the current project with comprehensive routing info (VP-124 Phase 1).
    ///
    /// Conductor Lane Echoes が「lane を operate するすべての座標」 を 1 call で取得するための tool。
    /// GET /api/lanes wrapper、 各 Lane に mailbox_addresses (per-Lane Stands の wire address)、
    /// top-level に project_addresses + world_addresses を synthesize。
    #[tool(
        description = "List all Lanes (Conductor + Performers) in the current project with comprehensive routing info. Each Lane returns: address, kind, state, stand, pid, cwd, tmux session, performer_status, AND mailbox_addresses (= wire-ready addresses for `wire_send`)。 Each lane's mailbox_addresses has two entries: `agent` (= the lane's Claude session inbox, e.g. `agent@vantage-point` for conductor or `agent@vantage-point/chore` for performer 'chore') and `canvas` (= the lane's Canvas / Paisley Park inbox, e.g. `canvas@vantage-point/chore`)。 Top-level also returns project_addresses (e.g. `gold_experience@<project>`) and world_addresses (e.g. `hermit_purple@world`)。 Use this to discover Performers, decide deletion targets, pick wire routes for wire_send。 Replaces multi-step `vp ps` + `curl /api/lanes`。"
    )]
    async fn list_lanes(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ListLanesParams>,
    ) -> Result<CallToolResult, McpError> {
        let process_url = self.process_url.lock().await.clone();

        // project name は /api/health から (= delete_performer と同型 pattern)。
        // 旧実装 (`lanes_in.first().get("project")`) は SP 起動直後 lanes 空の race
        // window で `"unknown"` fallback → `gold_experience@unknown` 等の偽 address を
        // 返す bug があり、 PR-β-4 review feedback (`feedback_jsonschema_field_scope`)
        // 同様 contract 強度のため authoritative source `/api/health.project_dir` に統一。
        let health = self
            .client
            .get(format!("{}/api/health", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("/api/health parse 失敗: {}", e), None)
            })?;
        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::internal_error("/api/health に project_dir なし".to_string(), None)
            })?;
        let project = std::path::Path::new(project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("project_dir basename 取得失敗: {}", project_dir),
                    None,
                )
            })?
            .to_string();

        let resp = self
            .client
            .get(format!("{}/api/lanes", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| McpError::internal_error(format!("/api/lanes parse 失敗: {}", e), None))?;

        let lanes_in = resp
            .get("lanes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // フィルタ + mailbox_addresses 注入
        let mut lanes_out: Vec<serde_json::Value> = Vec::new();
        for mut lane in lanes_in.into_iter() {
            // kind / state filter
            if let Some(k) = &params.kind
                && lane.get("kind").and_then(|v| v.as_str()) != Some(k.as_str())
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
            let lane_label = match lane.get("kind").and_then(|v| v.as_str()) {
                Some("conductor") => "conductor".to_string(),
                Some("performer") => lane
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed")
                    .to_string(),
                _ => "unknown".to_string(),
            };
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
                "hermit_purple": "hermit_purple@world",
            },
        });

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
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

    /// flow_handoff: 新 Performer 作成 + 初手 wire_send + nudge を atomic に
    #[tool(
        description = "Atomic dev-flow handoff: (1) Performer Lane 新規作成、 (2) task_spec を wire_send (= 初手 thread root)、 (3) `nudge=true` (default) 時は tmux send-keys で wire_recv を促す。 失敗時は performer 削除で rollback。 既存 3 step (add_performer + wire_send + tmux_agent_send) を 1 call に圧縮 (= dev-flow P4 = 'handoff' を 1 call で完結)。"
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

        // ── Step 1: Performer 作成 (= add_performer と同型 path、 HTTP POST /api/lanes) ──
        let process_url = self.process_url.lock().await.clone();
        let mut create_body = serde_json::json!({
            "kind": "performer",
            "name": params.name,
        });
        if let Some(b) = params.branch.as_ref().filter(|s| !s.trim().is_empty()) {
            create_body["branch"] = serde_json::Value::String(b.clone());
        }
        if let Some(s) = params.stand.as_ref().filter(|s| !s.trim().is_empty()) {
            create_body["stand"] = serde_json::Value::String(s.clone());
        }
        let create_url = format!("{}/api/lanes", process_url);
        let resp = self
            .client
            .post(&create_url)
            .json(&create_body)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(McpError::internal_error(
                format!("SP /api/lanes {}: {}", status, text),
                None,
            ));
        }
        let lane_info: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);

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
                    .flow_rollback_performer(&process_url, "<unknown>", &performer_name)
                    .await;
                return Err(McpError::internal_error(
                    "SP response from /api/lanes に address.project がありません".to_string(),
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
        let from = self.self_lane.from_address();
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
                    .flow_rollback_performer(&process_url, &project_name, &performer_name)
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

        // ── Step 3: nudge — tmux send-keys で worker に wire_recv を促す ──
        // tmux 経路は best-effort。 nudge 失敗で handoff 全体は失敗扱いにしない
        // (= wire は届いており worker は自走可、 nudge は immediacy の向上目的)。
        let mut nudge_status = if nudge { "skipped" } else { "off" }.to_string();
        if nudge {
            // tmux pane は lane address ("project/performer/name") で resolve できる
            // (resolve_pane が tmux_resolve_pane → meta から label 引き)。
            let nudge_text = "conductor から task が届いています。 mcp__vantage-point__wire_recv で確認、 内容に従って着手してください。 質問は wire_send + reply_to で thread 返信。\n".to_string();
            match self.resolve_pane(&lane_address).await {
                Ok((pane_id, _display)) => {
                    let send_keys = self
                        .quic_call(
                            "tmux_send_keys",
                            serde_json::json!({
                                "pane_id": pane_id,
                                "keys": nudge_text,
                            }),
                        )
                        .await;
                    nudge_status = match send_keys {
                        Ok(_) => "sent".to_string(),
                        Err(e) => format!("failed (best-effort): {}", e),
                    };
                }
                Err(e) => {
                    nudge_status = format!("pane resolve failed (best-effort): {}", e);
                }
            }
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

    /// flow_handoff の rollback path: performer 削除 (best-effort、 失敗は log only)
    ///
    /// wire_send 失敗時など、 performer 作成は成功したが orchestration の続きが失敗した時に呼ぶ。
    /// `<project>/performer/<name>` を address 化して DELETE /api/lanes に送る。
    async fn flow_rollback_performer(
        &self,
        process_url: &str,
        project_name: &str,
        performer_name: &str,
    ) -> Result<(), String> {
        let address = format!("{}/performer/{}", project_name, performer_name);
        let address_enc = address.replace('/', "%2F");
        let url = format!(
            "{}/api/lanes?address={}&cleanup=true",
            process_url, address_enc
        );
        let resp = self
            .client
            .delete(&url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("DELETE /api/lanes 失敗: {}", e))?;
        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
            return Err(format!(
                "rollback DELETE /api/lanes {}: {}",
                status,
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    /// flow_progress: parallel work 集約 view (read-only)
    #[tool(
        description = "Parallel work 集約 view: 現 project の全 Lane (conductor + performers) の performer_status (git ahead/behind/dirty/merged) と per-lane 未読 wire 数を 1 view で返す。 read-only (= cursor は触らない)、 cache OK。 dev-flow P5 (= 並列追跡) で list_lanes + wire_recv + tmux_capture を別々に叩く代替。"
    )]
    async fn flow_progress(
        &self,
        rmcp::handler::server::wrapper::Parameters(_params): rmcp::handler::server::wrapper::Parameters<FlowProgressParams>,
    ) -> Result<CallToolResult, McpError> {
        let process_url = self.process_url.lock().await.clone();

        // project name は /api/health から (delete_performer / list_lanes と同型 pattern)。
        let health: serde_json::Value = self
            .client
            .get(format!("{}/api/health", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("/api/health parse 失敗: {}", e), None)
            })?;
        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::internal_error("/api/health に project_dir なし".to_string(), None)
            })?;
        let project = std::path::Path::new(project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("project_dir basename 取得失敗: {}", project_dir),
                    None,
                )
            })?
            .to_string();

        // 全 lane (conductor + performers) を /api/lanes から取得 (= performer_status 込み)
        let lanes_resp: serde_json::Value = self
            .client
            .get(format!("{}/api/lanes", process_url))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json()
            .await
            .map_err(|e| McpError::internal_error(format!("/api/lanes parse 失敗: {}", e), None))?;
        let lanes_in = lanes_resp
            .get("lanes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut performers: Vec<serde_json::Value> = Vec::new();
        let mut conductor_unread: u64 = 0;
        let mut conductor_unread_by_thread = serde_json::Value::Object(Default::default());
        for lane in lanes_in {
            let kind = lane
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let lane_label = if kind == "conductor" {
                "conductor".to_string()
            } else {
                lane.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed")
                    .to_string()
            };
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
            let fsm = crate::flow::derive_flow_state(
                latest_view.as_ref(),
                performer_status_view,
                &agent_addr,
            );

            performers.push(serde_json::json!({
                "name": lane_label,
                "address": format!("agent@{}/{}", project, lane.get("name").and_then(|v| v.as_str()).unwrap_or("")),
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

    /// Show content in the browser viewer
    #[tool(
        description = "Display content in the Vantage Point browser viewer. Supports markdown, html, log, and url formats. Use content_type='url' to embed a web page in an iframe."
    )]
    async fn show(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());
        let content_type = params
            .content_type
            .unwrap_or_else(|| "markdown".to_string());

        // content_type → protocol::Content enum 変換
        let content = match content_type.as_str() {
            "html" => crate::protocol::Content::Html(params.content),
            "log" => crate::protocol::Content::Log(params.content),
            "url" => crate::protocol::Content::Url(params.content),
            _ => crate::protocol::Content::Markdown(params.content),
        };

        // doc 19 PP Canvas Stack Model: append は spec から omit。 protocol layer の
        // ProcessMessage::Show.append は keep (= wire 互換)、 値は false 固定で送る。
        // WebView 側 canvas-handler が stack model で新 item として push する。
        let msg = ProcessMessage::Show {
            pane_id: pane_id.clone(),
            content,
            append: false,
            title: params.title,
        };

        self.process_call("show", &msg).await?;

        // VP-83 Phase 2.2: Native App に Canvas pane auto-open 通知を送信。
        // 既に Canvas pane があれば Swift 側で no-op、無ければ自動生成。
        #[cfg(target_os = "macos")]
        {
            let port = *self.process_port.lock().await;
            crate::notify::post_canvas_open(port);
        }

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Content displayed in pane '{}'", pane_id),
        )]))
    }

    /// Clear content in a pane
    #[tool(description = "Clear content in a specific pane of the browser viewer")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let msg = ProcessMessage::Clear {
            pane_id: pane_id.clone(),
        };
        self.process_call("clear", &msg).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' cleared", pane_id),
        )]))
    }

    /// Toggle side panel visibility
    #[tool(
        description = "Toggle side panel visibility in the Vantage Point browser viewer. Use pane_id 'left' or 'right'."
    )]
    async fn toggle_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TogglePaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let state_desc = match params.visible {
            Some(true) => "shown",
            Some(false) => "hidden",
            None => "toggled",
        };

        let msg = ProcessMessage::TogglePane {
            pane_id: params.pane_id.clone(),
            visible: params.visible,
        };
        self.process_call("toggle_pane", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' {}", params.pane_id, state_desc),
        )]))
    }

    /// Close a pane
    #[tool(description = "Close a pane in the Vantage Point browser viewer.")]
    async fn close_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClosePaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let msg = ProcessMessage::Close {
            pane_id: params.pane_id.clone(),
        };
        self.process_call("close_pane", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' closed", params.pane_id),
        )]))
    }

    /// Watch a log file and display it in real-time in a pane
    #[tool(
        description = "Watch a log file and display new lines in real-time in a Vantage Point pane. Supports JSON Lines and plain text formats with level filtering and target exclusion."
    )]
    async fn watch_file(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<WatchFileParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::file_watcher::{WatchConfig, WatchFormat, WatchStyle};

        let format = match params.format.as_deref() {
            Some("plain") => WatchFormat::Plain,
            _ => WatchFormat::JsonLines,
        };

        let style = match params.style.as_deref() {
            Some("plain") => WatchStyle::Plain,
            _ => WatchStyle::Terminal,
        };

        let config = WatchConfig {
            path: params.path.clone(),
            pane_id: params.pane_id.clone(),
            format,
            filter: params.filter,
            exclude_targets: params.exclude_targets.unwrap_or_default(),
            title: params.title,
            style,
        };

        let payload = serde_json::to_value(&config)
            .map_err(|e| McpError::internal_error(format!("Serialize error: {}", e), None))?;
        self.quic_call("watch_file", payload).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Now watching '{}' → pane '{}'", params.path, params.pane_id),
        )]))
    }

    /// Stop watching a file
    #[tool(description = "Stop watching a file for a specific pane.")]
    async fn unwatch_file(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<UnwatchFileParams>,
    ) -> Result<CallToolResult, McpError> {
        self.quic_call(
            "unwatch_file",
            serde_json::json!({"pane_id": params.pane_id}),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Stopped watching pane '{}'", params.pane_id),
        )]))
    }

    /// Split the current tmux window to create a new pane.
    ///
    /// tmux split-window で新しいペインを作成する。
    /// performer 起動や並列 CC セッション作成に使う。
    #[tool(
        description = "Split the current tmux window to create a new pane. Use this to spawn parallel performers (e.g. Claude Code sessions, shell commands). Returns the new pane ID."
    )]
    async fn tmux_split(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxSplitParams>,
    ) -> Result<CallToolResult, McpError> {
        let horizontal = params.horizontal.unwrap_or(true);
        let mut payload = serde_json::json!({"horizontal": horizontal});
        if let Some(cmd) = &params.command {
            payload["command"] = serde_json::Value::String(cmd.clone());
        }
        if let Some(ct) = &params.content_type {
            payload["content_type"] = serde_json::Value::String(ct.clone());
        }
        let resp = self.quic_call("tmux_split", payload).await?;
        let pane_id = resp
            .pointer("/pane/id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let cmd_display = resp
            .pointer("/pane/command")
            .and_then(|v| v.as_str())
            .unwrap_or("shell");
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("New pane created: {} ({})", pane_id, cmd_display),
        )]))
    }

    /// Capture tmux pane content as text.
    ///
    /// tmux capture-pane で指定ペイン（または全ペイン）のターミナル出力をテキストとして取得する。
    /// AI エージェントが他のペインの状態を把握するのに使う。
    #[tool(
        description = "Capture tmux pane content as text. If pane_id is omitted, captures all panes in the session. Useful for monitoring performer progress or reading terminal output from other panes."
    )]
    async fn tmux_capture(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxCaptureParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(query) = &params.pane_id {
            // 単一ペインキャプチャ（label でも指定可能）
            let (pane_id, display) = self.resolve_pane(query).await?;
            let resp = self
                .quic_call("tmux_capture", serde_json::json!({"pane_id": pane_id}))
                .await?;
            let content = resp.get("content").and_then(|v| v.as_str()).unwrap_or("");
            Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                format!("=== Pane {} ===\n{}", display, content),
            )]))
        } else {
            // 全ペインキャプチャ
            let resp = self
                .quic_call("tmux_capture_all", serde_json::json!({}))
                .await?;
            let captures = resp
                .get("captures")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut output = String::new();
            for cap in &captures {
                let pane_id = cap
                    .pointer("/pane/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let cmd = cap
                    .pointer("/pane/command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let content = cap.get("content").and_then(|v| v.as_str()).unwrap_or("");
                // エージェントメタデータがあればラベルを併記
                let label = cap.pointer("/agent/label").and_then(|v| v.as_str());
                let header = match label {
                    Some(l) => format!("=== {} ({}) [{}] ===", l, pane_id, cmd),
                    None => format!("=== Pane {} ({}) ===", pane_id, cmd),
                };
                output.push_str(&format!("{}\n{}\n\n", header, content));
            }

            if output.is_empty() {
                output = "No tmux panes found (tmux not active?)".to_string();
            }

            Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                output.trim_end().to_string(),
            )]))
        }
    }

    /// Show tmux pane dashboard on Canvas.
    ///
    /// 全 tmux ペインをキャプチャして Canvas に markdown ダッシュボードとして表示する。
    #[tool(
        description = "Show a tmux pane dashboard on Canvas. Captures all panes in the current tmux session and displays them as a markdown dashboard. Great for monitoring parallel performers."
    )]
    async fn tmux_dashboard(&self) -> Result<CallToolResult, McpError> {
        // 全ペインキャプチャ
        let resp = self
            .quic_call("tmux_capture_all", serde_json::json!({}))
            .await?;
        let captures = resp
            .get("captures")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if captures.is_empty() {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                "No tmux panes found (tmux not active?)".to_string(),
            )]));
        }

        // エージェント情報を持つペインと通常ペインを分類
        let mut agent_panes = Vec::new();
        let mut normal_panes = Vec::new();
        for cap in &captures {
            if cap.get("agent").is_some() && !cap["agent"].is_null() {
                agent_panes.push(cap);
            } else {
                normal_panes.push(cap);
            }
        }

        // markdown ダッシュボードを構築
        let mut md = String::from("# tmux Dashboard\n\n");

        // エージェントパイプライン表示
        if !agent_panes.is_empty() {
            md.push_str("## Agent Pipeline\n\n");
            md.push_str("| Pane | Agent | Status | Task |\n");
            md.push_str("|------|-------|--------|------|\n");
            for cap in &agent_panes {
                let pane_id = cap
                    .pointer("/pane/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let label = cap
                    .pointer("/agent/label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let status = cap
                    .pointer("/agent/status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let task = cap
                    .pointer("/agent/task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let status_icon = match status {
                    "running" => "🟢",
                    "waiting" => "⏳",
                    "done" => "✅",
                    "error" => "🔴",
                    _ => "⚪",
                };
                md.push_str(&format!(
                    "| {} | {} | {} {} | {} |\n",
                    pane_id, label, status_icon, status, task
                ));
            }
            md.push('\n');
        }

        // 全ペインの出力表示
        for cap in &captures {
            let pane_id = cap
                .pointer("/pane/id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let cmd = cap
                .pointer("/pane/command")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let width = cap
                .pointer("/pane/width")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let height = cap
                .pointer("/pane/height")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let active = cap
                .pointer("/pane/active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = cap.get("content").and_then(|v| v.as_str()).unwrap_or("");

            // エージェント名があれば表示
            let agent_label = cap.pointer("/agent/label").and_then(|v| v.as_str());
            let agent_status = cap.pointer("/agent/status").and_then(|v| v.as_str());

            // 最後の数行だけ表示（ダッシュボード向け）
            let tail_lines: Vec<&str> = content.lines().rev().take(15).collect();
            let tail: String = tail_lines.into_iter().rev().collect::<Vec<_>>().join("\n");

            let active_marker = if active { " *" } else { "" };
            let agent_info = match (agent_label, agent_status) {
                (Some(label), Some(status)) => format!(" — {} ({})", label, status),
                _ => String::new(),
            };
            md.push_str(&format!(
                "## {} `{}` ({}x{}){}{}\n\n```\n{}\n```\n\n",
                pane_id, cmd, width, height, active_marker, agent_info, tail
            ));
        }

        // Canvas に表示
        self.quic_call(
            "show",
            serde_json::json!({
                "pane_id": "tmux-dashboard",
                "content": {"Markdown": md},
                "title": "tmux Dashboard",
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Dashboard shown on Canvas ({} panes)", captures.len()),
        )]))
    }

    /// Deploy an agent to a new tmux pane.
    ///
    /// tmux split + エージェントメタデータ設定を1コールで実行。
    /// team-bucciarati 等のパイプラインから呼ばれる想定。
    #[tool(
        description = "Deploy an agent to a new tmux pane. Creates a split pane, runs the command, and tags it with agent metadata (label, status, task). Returns the pane ID for subsequent status updates."
    )]
    async fn tmux_agent_deploy(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxAgentDeployParams>,
    ) -> Result<CallToolResult, McpError> {
        // 1. ペイン分割
        let horizontal = params.horizontal.unwrap_or(true);
        let mut split_payload = serde_json::json!({"horizontal": horizontal});
        if let Some(cmd) = &params.command {
            split_payload["command"] = serde_json::Value::String(cmd.clone());
        }
        let resp = self.quic_call("tmux_split", split_payload).await?;
        let pane_id = resp
            .pointer("/pane/id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        // 2. エージェントメタデータ設定
        self.quic_call(
            "tmux_set_agent_meta",
            serde_json::json!({
                "pane_id": pane_id,
                "label": params.label,
                "status": "running",
                "task": params.task,
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Agent '{}' deployed to pane {} (status: running, task: {})",
                params.label,
                pane_id,
                params.task.as_deref().unwrap_or("-")
            ),
        )]))
    }

    /// Update agent status on a tmux pane.
    ///
    /// エージェントの実行ステータスを更新する。ダッシュボードに反映される。
    #[tool(
        description = "Update the status of an agent running in a tmux pane. Status values: 'running', 'waiting', 'done', 'error'. Optionally update the task description."
    )]
    async fn tmux_agent_status(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxAgentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let (pane_id, display) = self.resolve_pane(&params.pane_id).await?;
        let mut payload = serde_json::json!({
            "pane_id": pane_id,
            "status": params.status,
        });
        if let Some(task) = &params.task {
            payload["task"] = serde_json::Value::String(task.clone());
        }
        self.quic_call("tmux_update_agent_status", payload).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Agent status updated: pane={}, status={}",
                display, params.status
            ),
        )]))
    }

    /// Send text input to a tmux pane (agent intervention).
    ///
    /// tmux send-keys でペインにテキストを送信する。
    /// エージェントへの介入やユーザー入力の自動化に使う。
    #[tool(
        description = "Send text input to a tmux pane via send-keys. Use this to intervene in an agent's execution or provide input. Text is sent as-is (include '\\n' for Enter)."
    )]
    async fn tmux_agent_send(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxAgentSendParams>,
    ) -> Result<CallToolResult, McpError> {
        let (pane_id, display) = self.resolve_pane(&params.pane_id).await?;
        self.quic_call(
            "tmux_send_keys",
            serde_json::json!({
                "pane_id": pane_id,
                "keys": params.text,
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Sent to pane {}: {:?}", display, params.text),
        )]))
    }

    /// Capture the Canvas window as a PNG screenshot
    ///
    /// html2canvas で Canvas の DOM をキャプチャし、PNG ファイルとして保存する。
    /// 保存されたファイルは Claude の Read ツールで画像として確認可能。
    #[tool(
        description = "Capture the Canvas window as a PNG screenshot. The saved file can be viewed with the Read tool."
    )]
    async fn capture_canvas(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CaptureCanvasParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = serde_json::json!({
            "path": params.path,
            "pane_id": params.pane_id,
        });

        // TheWorld 経由で Canvas にキャプチャリクエスト（Canvas は常に TheWorld の WS に接続）
        let world_port = crate::cli::WORLD_PORT;
        let url = format!("http://[::1]:{}/api/canvas/capture", world_port);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(
                    format!("Canvas capture 通信失敗: {}. Is vp running?", e),
                    None,
                )
            })?;

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Canvas capture レスポンスパース失敗: {}", e), None)
        })?;

        let status = json
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        if status != "ok" {
            let msg = json
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            return Err(McpError::internal_error(
                format!("Canvas capture 失敗: {}", msg),
                None,
            ));
        }

        let saved_path = json
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let width = json.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
        let height = json.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
        let size_bytes = json.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Screenshot saved: {}\nSize: {}x{} ({} bytes)\nUse the Read tool to view this image.",
                saved_path, width, height, size_bytes
            ),
        )]))
    }

    /// Paisley Park Canvas の pane 一覧 (= 表示中の各 pane の最新内容) を返す。
    #[tool(
        description = "List the panes currently on the Paisley Park Canvas (PP). Returns each pane_id with its latest content's title, content_type, and a short preview. Use read_pane to fetch a pane's full source content (e.g. to save it to memory). Reads the retained snapshot over the Unison canvas channel."
    )]
    async fn list_canvas(&self) -> Result<CallToolResult, McpError> {
        let panes = self.fetch_canvas_panes().await?;
        if panes.is_empty() {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                "Canvas に表示中の pane はありません (retained snapshot が空)。".to_string(),
            )]));
        }
        let mut lines = vec![format!("Canvas panes ({}):", panes.len())];
        for p in &panes {
            let preview: String = p
                .content
                .chars()
                .take(80)
                .collect::<String>()
                .replace('\n', " ");
            lines.push(format!(
                "- pane_id={} [{}] title={} | {}",
                p.pane_id,
                p.content_type,
                p.title.as_deref().unwrap_or("(none)"),
                preview
            ));
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            lines.join("\n"),
        )]))
    }

    /// Canvas pane の全文を返す (= remember 等に渡せるソース内容)。
    #[tool(
        description = "Read the full source content of a Paisley Park Canvas pane by pane_id (markdown/html/log/url text), so it can be saved to creo-memories (mcp__creo-memories__remember) or otherwise processed. If pane_id is omitted and exactly one pane exists, that pane is returned. Reads over the Unison canvas channel."
    )]
    async fn read_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ReadPaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let panes = self.fetch_canvas_panes().await?;
        let target = match &params.pane_id {
            Some(id) => panes.iter().find(|p| &p.pane_id == id),
            None if panes.len() == 1 => panes.first(),
            None => None,
        };
        let Some(p) = target else {
            let ids: Vec<&str> = panes.iter().map(|p| p.pane_id.as_str()).collect();
            let hint = if params.pane_id.is_some() {
                format!("pane_id が見つかりません。 現在の pane: {:?}", ids)
            } else {
                format!(
                    "pane が複数あります。 pane_id を指定してください: {:?}",
                    ids
                )
            };
            return Err(McpError::invalid_params(hint, None));
        };
        let header = format!(
            "pane_id: {}\ncontent_type: {}\ntitle: {}\n---\n",
            p.pane_id,
            p.content_type,
            p.title.as_deref().unwrap_or("(none)")
        );
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("{}{}", header, p.content),
        )]))
    }

    /// "canvas" Unison channel に one-shot 接続し、 retained snapshot (pane_id ごとの最新 Show)
    /// を drain して返す (Unison-native read、 app.rs::run_canvas_session のパターン再利用)。
    ///
    /// QUIC listener は SP の HTTP と同 port (UDP)。 retained は接続直後に届くため、
    /// live update を待たないよう短い timeout で drain する。
    async fn fetch_canvas_panes(&self) -> Result<Vec<CanvasPane>, McpError> {
        use unison::ProtocolClient;
        use unison::network::MessageType;
        use unison::network::TrustAnchors;
        use unison::network::quic::QuicClient;

        let port = *self.process_port.lock().await;
        let addr = format!("[::1]:{}", port);
        let transport = QuicClient::builder()
            .trust_anchors(TrustAnchors::SkipVerification)
            .build()
            .map_err(|e| McpError::internal_error(format!("QUIC client build: {}", e), None))?;
        let client = ProtocolClient::new(transport);
        client.connect(&addr).await.map_err(|e| {
            McpError::internal_error(format!("canvas connect {}: {}", addr, e), None)
        })?;
        let channel = client
            .open_channel("canvas")
            .await
            .map_err(|e| McpError::internal_error(format!("open canvas channel: {}", e), None))?;

        // pane_id ごとに最新を保持 (同一 pane_id の複数 Show は後勝ち)。
        let mut panes: std::collections::HashMap<String, CanvasPane> =
            std::collections::HashMap::new();
        loop {
            match tokio::time::timeout(Duration::from_millis(500), channel.recv()).await {
                Ok(Ok(msg)) => {
                    if msg.msg_type != MessageType::Event || msg.method != "pane" {
                        continue;
                    }
                    if let Ok(v) = msg.payload_as_value()
                        && let Some(p) = parse_show_payload(&v)
                    {
                        panes.insert(p.pane_id.clone(), p);
                    }
                }
                Ok(Err(_)) => break, // channel closed
                Err(_) => break,     // timeout = snapshot drained
            }
        }
        let mut out: Vec<CanvasPane> = panes.into_values().collect();
        out.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
        Ok(out)
    }

    /// VantagePoint.app のターミナルウィンドウを PNG スクリーンショットとしてキャプチャ
    ///
    /// macOS の screencapture コマンドで VantagePoint ウィンドウをキャプチャする。
    /// 保存されたファイルは Claude の Read ツールで画像として確認可能。
    #[tool(
        description = "Capture the VantagePoint.app terminal window as a PNG screenshot. The saved file can be viewed with the Read tool. Use this to inspect rendering issues, verify UI changes, or debug visual problems."
    )]
    async fn capture_terminal(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CaptureTerminalParams>,
    ) -> Result<CallToolResult, McpError> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let save_path = params
            .path
            .unwrap_or_else(|| format!("/tmp/vp-terminal-{}.png", ts));

        // VantagePoint ウィンドウの CGWindowID を取得
        // NOTE: kCGWindowOwnerName はアプリの表示名（"Vantage Point"）であり
        //       バイナリ名（"VantagePoint"）とは異なる。
        //       JXA の ObjC bridge は権限制限で動作しないことがあるため swift -e を使用。
        //       Layer=0 かつ Name 非空のウィンドウがメインウィンドウ。
        let swift_script = r#"
import CoreGraphics
let windows = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as? [[String: Any]] ?? []
// layer=0 で最も大きいウィンドウをメインウィンドウとみなす
// （ウィンドウタイトルが空の場合があるため name ではなくサイズで判定）
var bestId = 0
var bestArea = 0
for w in windows {
    let owner = w["kCGWindowOwnerName"] as? String ?? ""
    if owner.contains("Vantage") || owner == "VantagePoint" {
        let layer = w["kCGWindowLayer"] as? Int ?? -1
        if layer == 0 {
            let bounds = w["kCGWindowBounds"] as? [String: Any] ?? [:]
            let width = bounds["Width"] as? Int ?? 0
            let height = bounds["Height"] as? Int ?? 0
            let area = width * height
            if area > bestArea {
                bestArea = area
                bestId = w["kCGWindowNumber"] as? Int ?? 0
            }
        }
    }
}
if bestId > 0 { print(bestId) }
"#;
        let wid_output = tokio::process::Command::new("swift")
            .args(["-e", swift_script])
            .output()
            .await
            .map_err(|e| McpError::internal_error(format!("swift 実行失敗: {}", e), None))?;

        let window_id = if wid_output.status.success() {
            let id = String::from_utf8_lossy(&wid_output.stdout)
                .trim()
                .to_string();
            if id.is_empty() {
                return Err(McpError::internal_error(
                    "VantagePoint ウィンドウが見つかりません。VantagePoint.app が起動しているか確認してください。".to_string(),
                    None,
                ));
            }
            id
        } else {
            let stderr = String::from_utf8_lossy(&wid_output.stderr);
            return Err(McpError::internal_error(
                format!(
                    "VantagePoint ウィンドウ ID 取得失敗（VantagePoint.app が未起動の可能性）: {}",
                    stderr.trim()
                ),
                None,
            ));
        };

        let mut cmd = tokio::process::Command::new("screencapture");
        cmd.args(["-x", "-o"]); // -x: サウンドなし, -o: 影なし
        cmd.args(["-l", &window_id]); // -l: ウィンドウ ID 指定
        cmd.arg(&save_path);

        let output = cmd.output().await.map_err(|e| {
            McpError::internal_error(format!("screencapture 実行失敗: {}", e), None)
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(McpError::internal_error(
                format!("screencapture 失敗: {}", stderr),
                None,
            ));
        }

        // ファイルサイズ取得
        let metadata = tokio::fs::metadata(&save_path)
            .await
            .map_err(|e| McpError::internal_error(format!("保存ファイル確認失敗: {}", e), None))?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Terminal screenshot saved: {}\nSize: {} bytes\nUse the Read tool to view this image.",
                save_path,
                metadata.len()
            ),
        )]))
    }

    /// Execute Ruby code and display results in a pane
    #[tool(
        description = "Execute Ruby code or a Ruby file and display the results in a Canvas pane. For short-lived execution (scripts, data processing). Use run_ruby for long-running daemon processes."
    )]
    async fn eval_ruby(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<EvalRubyParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let body = serde_json::json!({
            "code": params.code,
            "file": params.file,
            "pane_id": pane_id,
        });

        let resp = self.http_post("/api/ruby/eval", &body).await?;

        let stdout = resp.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let stderr = resp.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
        let exit_code = resp.get("exit_code").and_then(|v| v.as_i64());
        let elapsed = resp.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);

        let mut result = format!("Ruby eval completed in {}ms", elapsed);
        if let Some(code) = exit_code
            && code != 0
        {
            result.push_str(&format!(" (exit code: {})", code));
        }
        if !stdout.is_empty() {
            result.push_str(&format!("\n\nstdout:\n{}", stdout));
        }
        if !stderr.is_empty() {
            result.push_str(&format!("\n\nstderr:\n{}", stderr));
        }

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            result,
        )]))
    }

    /// Run Ruby code as a long-running daemon process
    #[tool(
        description = "Run Ruby code or a Ruby file as a long-running daemon process. Output is streamed to a Canvas pane in real-time. Use stop_ruby to gracefully stop the process. Use list_ruby to see running processes."
    )]
    async fn run_ruby(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<RunRubyParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let body = serde_json::json!({
            "code": params.code,
            "file": params.file,
            "name": params.name,
            "pane_id": pane_id,
        });

        let resp = self.http_post("/api/ruby/run", &body).await?;

        let process_id = resp
            .get("process_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Ruby daemon started: {} (streaming to pane '{}'). Use stop_ruby with process_id='{}' to stop.",
                process_id, pane_id, process_id
            ),
        )]))
    }

    /// Stop a running Ruby daemon process
    #[tool(
        description = "Gracefully stop a running Ruby daemon process. Sends a shutdown signal and waits for the process to exit."
    )]
    async fn stop_ruby(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<StopRubyParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = serde_json::json!({
            "process_id": params.process_id,
        });

        self.http_post("/api/ruby/stop", &body).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Ruby process '{}' stop signal sent", params.process_id),
        )]))
    }

    /// List running Ruby daemon processes
    #[tool(
        description = "List all running Ruby daemon processes with their IDs, names, pane IDs, and status."
    )]
    async fn list_ruby(&self) -> Result<CallToolResult, McpError> {
        let url = format!("{}/api/ruby/list", self.process_url.lock().await);
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Ruby list 通信失敗: {}. Is vp running?", e), None)
            })?;

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Ruby list レスポンスパース失敗: {}", e), None)
        })?;

        let processes = json.get("processes").and_then(|v| v.as_array());
        let result = match processes {
            Some(procs) if procs.is_empty() => "No running Ruby processes.".to_string(),
            Some(procs) => {
                let mut lines = vec!["Running Ruby processes:".to_string()];
                for p in procs {
                    let id = p.get("process_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let pane = p.get("pane_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = p
                        .get("status")
                        .map(|v| format!("{}", v))
                        .unwrap_or_else(|| "?".to_string());
                    lines.push(format!(
                        "  {} - {} (pane: {}, status: {})",
                        id, name, pane, status
                    ));
                }
                lines.join("\n")
            }
            None => "No running Ruby processes.".to_string(),
        };

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            result,
        )]))
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
        let url = self.process_url.lock().await;
        let base_url = url.clone();
        drop(url);

        // URL からポートを抽出（失敗時は VP_PROCESS_PORT → 33000 の順でフォールバック）
        let port: u16 = base_url
            .split(':')
            .next_back()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                std::env::var("VP_PROCESS_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(33000)
            });

        // 1. Get current Process info (project_dir)
        let health_url = format!("{}/api/health", base_url);
        let health_resp = self.client.get(&health_url).send().await.map_err(|e| {
            McpError::internal_error(format!("Failed to get Process health: {}", e), None)
        })?;

        let health: serde_json::Value = health_resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Failed to parse health response: {}", e), None)
        })?;

        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();

        // 2. Send shutdown request
        let shutdown_url = format!("{}/api/shutdown", base_url);
        let _ = self.client.post(&shutdown_url).send().await;

        // 3. Wait for Process to stop (poll health endpoint)
        let stop_timeout = Duration::from_secs(10);
        let poll_interval = Duration::from_millis(200);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > stop_timeout {
                return Err(McpError::internal_error(
                    "Timeout waiting for Process to stop".to_string(),
                    None,
                ));
            }

            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                    // Still running, wait
                    tokio::time::sleep(poll_interval).await;
                }
                _ => {
                    // Process is down
                    break;
                }
            }
        }

        // 4. Start new Process process
        let open_viewer = params.open_viewer.unwrap_or(false);
        let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());
        let mut cmd = std::process::Command::new(&vp_bin);
        cmd.arg("start")
            .arg("-C")
            .arg(&project_dir)
            .arg("-p")
            .arg(port.to_string());

        if !open_viewer {
            cmd.arg("--headless");
        }

        // Spawn detached process
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        cmd.spawn().map_err(|e| {
            McpError::internal_error(format!("Failed to spawn new Process: {}", e), None)
        })?;

        // 5. Wait for new Process to be ready
        let start_timeout = Duration::from_secs(15);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > start_timeout {
                return Err(McpError::internal_error(
                    "Timeout waiting for Process to start".to_string(),
                    None,
                ));
            }

            tokio::time::sleep(poll_interval).await;

            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                    // Process is up — QUIC チャネルをリセットして再接続を強制
                    *self.process_channel.lock().await = None;

                    return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "Process restarted successfully on port {}. Project: {}",
                            port, project_dir
                        ),
                    )]));
                }
                _ => {
                    // Not ready yet, continue polling
                }
            }
        }
    }

    // =========================================================================
    // wiremsg — threaded inbox (Phase A ①、 設計 mem_1CbD9H1KGQykBaFG8XXVsn)
    //
    // agent 間メッセージングの正規 channel (threading 対応 inbox)。
    // 旧 msgbox (`msg_*`) は wiremsg 再設計 R5-1 で MCP tool / QUIC handler を撤去済み。
    // wire_send は新規 thread の root を作るか、 reply_to 指定で既存 thread に返信する。
    // wire_recv は呼び出し agent の参加 thread の未読 message を long-poll で取得する。
    // =========================================================================

    /// Send a wiremsg (new thread, or a reply when reply_to is set)
    #[tool(
        description = "Send a threaded wire message. Without `reply_to`, starts a NEW thread (root message). With `reply_to` (a wire message id), posts a REPLY into that message's thread. Recipients receive the message as unread; the sender does not see their own root message. Use wire_recv to read replies. This is the PRIMARY channel for inter-agent communication. Set body.category to one of {command, event, state, data, log} to control delivery policy: 'command' messages are re-nudged to the recipient until they wire_ack; omitted category defaults to 'event' (no nudge)."
    )]
    async fn wire_send(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<WireSendParams>,
    ) -> Result<CallToolResult, McpError> {
        // from は self_lane から導出 (conductor は "agent"、 performer は "agent@<parent>/<name>")
        let from = self.self_lane.from_address();
        let payload = serde_json::json!({
            "from": from,
            "to": params.to,
            "body": params.body,
            "reply_to": params.reply_to,
        });
        let resp = self.quic_call("wire_send", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "wire message sent".to_string()),
        )]))
    }

    /// Receive unread wiremsg messages from this agent's threads
    #[tool(
        description = "Receive unread messages from all wire threads this agent participates in. Waits up to `timeout` seconds (default 5, max 30); returns immediately if unread messages exist. Each returned message has `id`, `prev`, `from`, `to`, `body`, `created_at`, `local_seq`. A thread is identified by its root message id (follow `prev` to the message whose `prev` is null). Reading advances this agent's read cursor so messages are not re-delivered. This is the PRIMARY channel for inter-agent communication."
    )]
    async fn wire_recv(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<WireRecvParams>,
    ) -> Result<CallToolResult, McpError> {
        let timeout = params.timeout.unwrap_or(5).min(30);
        // agent は wire_send の from と同じ self_lane 由来 address
        let agent = self.self_lane.from_address();
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

    /// Trace the ancestor-chain (lineage) of a wire message
    #[tool(
        description = "Return the ancestor-chain (lineage from the thread root down to the given message) of a wire message. Each returned message has `id`, `prev`, `from`, `to`, `body`, `created_at`, `local_seq`, and the array is ordered root-first (chronological). This is READ-ONLY: it does NOT advance the wire_recv read cursor, so it is safe to call repeatedly. Use it to fetch backlog / context when you join a thread partway through (e.g. after receiving a reply via wire_recv and needing the messages that led up to it). It returns only the lineage of the given message, not the full branch tree; since each message carries its `prev`, the result collapses cleanly into a linear (or tree) view."
    )]
    async fn wire_thread(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<WireThreadParams>,
    ) -> Result<CallToolResult, McpError> {
        let payload = serde_json::json!({
            "message_id": params.message_id,
        });
        let resp = self.quic_call("wire_thread", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "null".to_string()),
        )]))
    }

    /// Check unread wire message counts WITHOUT consuming them (cursor-safe peek)
    #[tool(
        description = "Check this agent's unread wire message inventory WITHOUT reading them: returns `total` (unread count) and `by_thread` (root message id → unread count). This is READ-ONLY: unlike wire_recv it does NOT advance the read cursor, so it is safe to call repeatedly to decide whether a wire_recv is worth doing. Use this at natural boundaries (task start/end) to avoid leaving replies unread."
    )]
    async fn wire_inbox(
        &self,
        rmcp::handler::server::wrapper::Parameters(_params): rmcp::handler::server::wrapper::Parameters<WireInboxParams>,
    ) -> Result<CallToolResult, McpError> {
        // agent は wire_send / wire_recv と同じ self_lane 由来 address (SP 側で正規化される)
        let agent = self.self_lane.from_address();
        let payload = serde_json::json!({ "agent": agent });
        let resp = self.quic_call("wire_unread_count", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "null".to_string()),
        )]))
    }

    /// Acknowledge a wire message (per-message ack ledger, independent of the read cursor)
    #[tool(
        description = "Acknowledge (ack) a wire message AFTER you have actually handled it. The ack ledger is independent of the wire_recv read cursor: receiving a command via wire_recv does NOT count as handling it — an unacked command stays eligible for re-notification by the delivery loop. Returns `acked: true` for a new ack, `false` if this agent already acked the message (idempotent). Use the `id` field of a message returned by wire_recv."
    )]
    async fn wire_ack(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<WireAckParams>,
    ) -> Result<CallToolResult, McpError> {
        // agent は wire_send / wire_recv と同じ self_lane 由来 address (SP 側で正規化される)
        let agent = self.self_lane.from_address();
        let payload = serde_json::json!({
            "message_id": params.message_id,
            "agent": agent,
        });
        let resp = self.quic_call("wire_ack", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "null".to_string()),
        )]))
    }

    // ========================================================================
    // VP Port Management (VP-83 refinement, memory: mem_1CaKCbNE24KTQDuf9x4Eim)
    //
    // Deterministic port layout に基づき、slot × lane × role から port / URL
    // を透過的に計算して返す。agent は問い合わせた URL を WebFetch で
    // 読み込む等、自律 workflow で port を問い合わせできる。
    // ========================================================================

    /// Port 計算: slot × lane × role → port
    #[tool(
        description = "Compute the deterministic port for a given project slot + lane index + role. Returns the port number. Use port_url to get the full URL."
    )]
    async fn port_show(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PortShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let port = match params.role.as_deref() {
            Some(role) => layout.port(params.slot, params.lane, role),
            None => layout.lane_base(params.slot, params.lane),
        };
        let text = match port {
            Some(p) => format!("{}", p),
            None => format!(
                "no port: slot={}, lane={}, role={:?}",
                params.slot, params.lane, params.role
            ),
        };
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            text,
        )]))
    }

    /// URL 生成: `http://localhost:{port}`
    #[tool(
        description = "Generate a localhost URL (http://localhost:{port}) for a project slot + lane + role. Convenience wrapper over port_show."
    )]
    async fn port_url(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PortUrlParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let text = match layout.url(params.slot, params.lane, &params.role) {
            Some(u) => u,
            None => format!(
                "no URL: slot={}, lane={}, role={}",
                params.slot, params.lane, params.role
            ),
        };
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            text,
        )]))
    }

    /// Role → offset table
    #[tool(
        description = "List all registered port roles (agent/dev_server/db_admin/canvas/preview) with their offsets within a Lane."
    )]
    async fn port_roles(
        &self,
        // VP-XXX (rmcp 1.6 follow-up): `Parameters<()>` は schemars 1.x で `{const: null}` schema を
        // 生成 → rmcp 1.6 が MCP spec 厳格 check で reject (`tools[18].inputSchema.type` expected
        // `"object"`)。 空 struct (`PortRolesParams`) で `{type: "object", properties: {}}` 形式を
        // 生成させて MCP client の zod validation を通過させる。
        _params: rmcp::handler::server::wrapper::Parameters<PortRolesParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let mut out = format!("Role offsets (lane_size={}):\n", layout.lane_size);
        for (name, offset) in layout.valid_roles() {
            out.push_str(&format!("  +{:>2}  {}\n", offset, name));
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            out,
        )]))
    }

    /// 1 Project slot の全割当一覧 (Markdown)
    #[tool(
        description = "Show the full port layout for one project slot: SP HTTP, SP Unison, and all Lane × role combinations. Returns Markdown-formatted text."
    )]
    async fn port_layout(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PortLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let Some(base) = layout.project_base(params.slot) else {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                format!(
                    "slot {} is out of range (max_projects={})",
                    params.slot, layout.max_projects
                ),
            )]));
        };
        let mut md = format!("# Project slot {} (base {})\n\n", params.slot, base);
        md.push_str(&format!(
            "- SP HTTP: `{}`\n- SP Unison: `{}`\n\n",
            layout.sp_port(params.slot).unwrap(),
            layout.unison_port(params.slot).unwrap()
        ));
        for lane in 0..layout.max_lanes_per_project() {
            let Some(lb) = layout.lane_base(params.slot, lane) else {
                continue;
            };
            let label = if lane == 0 { "Conductor" } else { "Performer" };
            md.push_str(&format!("## Lane {} ({}) — base `{}`\n\n", lane, label, lb));
            for (role, offset) in layout.valid_roles() {
                if let Some(p) = layout.port(params.slot, lane, &role) {
                    md.push_str(&format!("- +{} `{}`: `{}`\n", offset, role, p));
                }
            }
            md.push('\n');
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            md,
        )]))
    }
}

// --- Port Management tool params ---

/// Parameters for `port_show`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortShowParams {
    /// Project slot (0-based, see port_slot_list for mapping)
    #[schemars(description = "Project slot index (0-19)")]
    pub slot: u16,
    /// Lane index (0 = Conductor, 1+ = Performer)
    #[schemars(description = "Lane index within project slot (0 = Conductor, 1+ = Performer)")]
    #[serde(default)]
    pub lane: u16,
    /// Role (agent / dev_server / db_admin / canvas / preview)
    #[schemars(
        description = "Role within Lane (agent/dev_server/db_admin/canvas/preview). Omit to get the Lane base port."
    )]
    pub role: Option<String>,
}

/// Parameters for `port_url`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortUrlParams {
    pub slot: u16,
    #[serde(default)]
    pub lane: u16,
    pub role: String,
}

/// Parameters for `port_layout`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortLayoutParams {
    /// Project slot to show
    pub slot: u16,
}

#[tool_handler]
impl rmcp::ServerHandler for VantageMcp {
    fn get_info(&self) -> ServerInfo {
        // rmcp 1.6 で ServerInfo は #[non_exhaustive] になり struct expression (= `ServerInfo { ... ..Default::default() }`)
        // が外部 crate から使えなくなった。 `Default::default()` で base instance を作ってから pub field を mutate
        // する pattern で API contract を満たす (= 公式が future-compatible として用意してる upgrade path)。
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Vantage Point Process - Display rich content (markdown, HTML, images) in a browser viewer. \
             Use 'capture_canvas' to take a PNG screenshot of the Canvas (viewable with Read tool), \
             'show' to display content, 'clear' to clear panes, \
             'close_pane' to close a pane, 'toggle_pane' to toggle panel visibility, \
             'restart' to restart the Process, \
             'watch_file' to monitor a log file in real-time, and 'unwatch_file' to stop monitoring.\n\n\
             When using 'show', prefer content_type='markdown' as the default format. \
             Markdown renders well in the Canvas and is easy to read. \
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
/// 引くのを最優先**にする。`VP_PROCESS_PORT` env は tmux セッション作成時の snapshot で、
/// port reshuffle（config の project リスト変更）に追従しない → stale を踏むと別 project の
/// SP に msg を投げ、その SP の `local_project` で `from` が汚染される（VP-165 dogfood 症状 (1)）。
/// env は discovery 一時障害時の fallback に格下げ。
///
/// 優先度:
/// 1. 明示的なポート引数（Some で指定された場合）
/// 2. discovery:
///    - performer context（cwd = `vp_data_dir()/lanes/<parent>-<name>`）→ parent project の path を
///      config から引いて `find_by_project`。performer の cwd は登録 project path 配下でないので
///      `find_for_cwd` は効かない
///    - conductor context → `find_for_cwd`（cwd 一致 or 配下の running SP）
/// 3. `VP_PROCESS_PORT` env（discovery 障害 / parent SP 未起動 時の fast path、reshuffle 後は古い可能性）
/// 4. デフォルトポート 33000
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

    // 3. VP_PROCESS_PORT env（fallback、reshuffle 後は古い可能性あり）
    if let Ok(env_port) = std::env::var("VP_PROCESS_PORT")
        && let Ok(port) = env_port.parse::<u16>()
    {
        return port;
    }

    // 4. フォールバック
    33000
}

/// Process port から QUIC 接続先アドレスを組み立てる ([::1] = IPv6 loopback)。
fn quic_addr(process_port: u16) -> String {
    format!(
        "[::1]:{}",
        process_port + crate::process::unison_server::QUIC_PORT_OFFSET
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

    // Resolve the actual port to use
    let resolved_port = resolve_process_port(process_port).await;

    // wiremsg R5-4: 旧 msgbox の registry サブシステム (Performer self-register) は撤去済。
    // wire の cross-process delivery は TheWorld の project registry を使う別経路。

    // Note: In MCP mode, we should not use tracing to stdout
    // as it interferes with JSON-RPC communication
    let service = VantageMcp::new(resolved_port)
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

    #[test]
    fn test_self_lane_from_address() {
        // VP-166 PR-4: conductor は bare "agent"、performer は "agent@<parent>/<name>"
        let conductor = SelfLane {
            lane_name: "conductor".to_string(),
            performer_parent: None,
        };
        assert_eq!(conductor.from_address(), "agent");

        let performer = SelfLane {
            lane_name: "chore".to_string(),
            performer_parent: Some("vantage-point".to_string()),
        };
        assert_eq!(performer.from_address(), "agent@vantage-point/chore");
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
        };
        assert_eq!(
            performer_parent_path(&performer, &cfg).as_deref(),
            Some("/Users/x/repos/vantage-point")
        );

        // conductor context → None（performer_parent が無い）
        let conductor = SelfLane {
            lane_name: "conductor".to_string(),
            performer_parent: None,
        };
        assert_eq!(performer_parent_path(&conductor, &cfg), None);

        // config に無い parent → None
        let unknown = SelfLane {
            lane_name: "x".to_string(),
            performer_parent: Some("not-in-config".to_string()),
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

    // --- ShowParams serde (doc 19 regression guards) ---

    /// doc 19: `append` field は ShowParams から omit 済み。
    /// 旧クライアントが `append: true` を送っても serde の unknown field として
    /// silent ignore され、 deserialize が成功すること (= backward compat)。
    #[test]
    fn show_params_silently_ignores_append_true() {
        let json = r#"{"content":"hello","append":true}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "hello");
        assert!(params.content_type.is_none());
        assert!(params.pane_id.is_none());
        assert!(params.title.is_none());
    }

    /// `append` が無くても (= 新クライアント形式) deserialize が成功すること。
    #[test]
    fn show_params_deserializes_without_append() {
        let json = r#"{"content":"world","content_type":"html","title":"My Page"}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "world");
        assert_eq!(params.content_type.as_deref(), Some("html"));
        assert_eq!(params.title.as_deref(), Some("My Page"));
    }

    /// `append: false` も silent ignore される (= 古い show handler の常時 false 送信経路)。
    #[test]
    fn show_params_silently_ignores_append_false() {
        let json = r#"{"content":"test","append":false,"pane_id":"main"}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "test");
        assert_eq!(params.pane_id.as_deref(), Some("main"));
    }

    /// `content` フィールドが必須であることを確認 (= 省略時は deserialize error)。
    #[test]
    fn show_params_requires_content_field() {
        let json = r#"{"content_type":"markdown"}"#;
        let result: Result<ShowParams, _> = serde_json::from_str(json);
        assert!(result.is_err(), "content が無くても成功してしまう");
    }
}
