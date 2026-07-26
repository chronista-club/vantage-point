//! daemon の HTTP 面 (`/api/health`) + surface が扱う wire 型
//!
//! ## doc 45 段 3 — 残っているのは health だけ
//!
//! 元は projects / processes / lanes を触る REST client (12 method) だったが、
//! control plane は Unison に寄せた ([`crate::daemon_control`])。ここに残るのは
//! **`/api/health` 1 本**で、これは統一の取りこぼしではなく doc 45 §2 の設計判断:
//! health は「他が壊れている時に動いてほしい」probe なので、Unison 層が wedge した時に
//! 診断手段ごと失わないよう、意図的に鈍い外殻 (HTTP) として置く。
//! `daemon_launcher` の起動待ちも同じ probe を叩く。
//!
//! wire 型 (`ProjectInfo` / `RunningProcess` / `LaneInfo` 等) は transport 非依存なので
//! 本 module に置いたまま — 読み手は Unison client / QUIC 購読 / sidebar push の 3 者。
//!
//! ## URL 解決
//! 1. `VP_DAEMON_URL` env var があれば優先 (例: `http://172.20.78.253:32000`)
//! 2. それ以外は `http://127.0.0.1:32000` (IPv4 loopback)
//!
//! **IPv6 `[::1]` は WSL2 → Windows の localhost 転送で通らない**ため
//! デフォルトは IPv4。WSL2 側で daemon を立ち上げて Windows の
//! vp-app から接続するケースを前提にしている。

use anyhow::Result;
use serde::Deserialize;

// R-0 (`docs/design/11-vp-app-refactor.md` § 3.0a / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
//   `LaneAddressWire` の正規定義は `lane.rs` に移管 (G2 解消、 3 重実装の 1 元化)。
//   client.rs は consumer として use で bring-into-scope する。
use crate::lane::LaneAddressWire;

// v1.0 柱 2 PR-1: ts-rs で sidebar wire 型を TS に export (test build 時のみ)。
#[cfg(test)]
use ts_rs::TS;

/// daemon の既定ポート。
///
/// VP_PROFILE 分離 (dev/brew 混在根治): brew=32000 / dev=32100。 定義は
/// `vp_paths::default_daemon_port()` (全 crate 共有の SSOT)。 dev binary と brew cask が
/// 別 node port で並列常駐できるよう、 app→daemon connect もこの port を honor する。
pub fn default_daemon_port() -> u16 {
    vp_paths::default_daemon_port()
}

/// デフォルト URL 解決
///
/// `VP_DAEMON_URL` env var → `http://127.0.0.1:{default_daemon_port()}`
fn default_base_url() -> String {
    std::env::var("VP_DAEMON_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", default_daemon_port()))
}

/// daemon の HTTP health クライアント (`/api/health` 専用)
///
/// doc 45 段 3 以降、control plane は [`crate::daemon_control::DaemonControl`] が持つ。
pub struct DaemonRpcClient {
    base_url: String,
    client: reqwest::Client,
}

/// Process kind (Architecture v4: mem_1CaSwJ?... Process Recursive)
///
/// 全 VP entity (daemon / SP / Lane / Stand) は `ProcessKind` を持つ Process として
/// homogeneous に扱う。Display metaphor は UI / log の format string のみで使い、
/// code 内 logic は kind 直値で switch する。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    /// system 全体を supervise する root process (= daemon 👑)
    Supervisor,
    /// Project に bind された runtime container (= 旧 SP / Project Core ⭐)
    /// daemon の projects 応答に kind field が無いケースは
    /// Runtime (= Project Process) 扱い (serde default)。
    #[default]
    Runtime,
    /// PTY session を持つ stream-based process (= Lane: Conductor / Performer)
    Session,
    /// 機能 service を提供する Stand process (= Echoes / Shell / board / runner ほか)
    Stand,
}

impl ProcessKind {
    /// Display 用 metaphor (UI / log の format string のみ、code logic では kind 直値で switch)
    pub fn metaphor(&self) -> &'static str {
        match self {
            ProcessKind::Supervisor => "👑 daemon",
            ProcessKind::Runtime => "⭐ Star Platinum",
            ProcessKind::Session => "📍 Lane",
            ProcessKind::Stand => "🦾 Stand",
        }
    }
}

/// Process state (全 ProcessKind 共通 state machine、Architecture v4 Idea 2)
///
/// daemon の projects 応答 (`daemon-control.projects/list`) の `process_status` の wire mirror。
///
/// daemon 側 `capability::process_manager_capability::ProcessStatus`
/// (Stopped/Starting/Running/Stopping/Error) と 1:1 対応させる。
/// **state の SSOT は daemon** ── vp-app は join で上書きせず、 この値をそのまま使う。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    /// SP 未起動 (停止中、 まだ起動していない)。
    /// default を Stopped にしているのは「未確認 = 停止扱い」が安全側のため
    /// (= 起動済と誤表示して loading spinner が永久に回る事故を防ぐ)。
    #[default]
    Stopped,
    /// 起動処理中 (SP spawn 中)
    Starting,
    /// 稼働中 (HTTP server listen 中)
    Running,
    /// 停止処理中 (graceful shutdown)
    Stopping,
    /// エラー
    Error,
}

impl ProcessStatus {
    /// snake_case string representation (sidebar JS / log での state badge match 用)
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessStatus::Stopped => "stopped",
            ProcessStatus::Starting => "starting",
            ProcessStatus::Running => "running",
            ProcessStatus::Stopping => "stopping",
            ProcessStatus::Error => "error",
        }
    }

    /// SP が生きている (稼働 or 過渡) か。 sidebar の currents 振り分け用。
    pub fn is_alive(&self) -> bool {
        matches!(
            self,
            ProcessStatus::Starting | ProcessStatus::Running | ProcessStatus::Stopping
        )
    }
}

/// Project info — `daemon-control.projects/list` レスポンス要素 (= 登録済 path identity)。
///
/// server 側 `ProjectInfo` (`capability::process_manager_capability::ProjectInfo`) と
/// 命名統一。 「list / identity 系 = Project」 「runtime lifecycle 系 = Process」 の
/// 階層 SSOT に従い、 vp-app の wire-deserialize 型は本 struct に集約する。
/// runtime port は `registry.list` (= `RunningProcess`) との join で merge する。
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
pub struct ProjectInfo {
    /// Process kind (default Runtime: daemon response 互換)
    #[serde(default)]
    pub kind: ProcessKind,
    pub name: String,
    /// Runtime kind の場合は git directory binding
    pub path: String,
    /// running の場合の port。 config の静的 port (= projects.kdl) を表す。
    /// runtime の実ポートは `fetch_projects_with_ports` で `RunningProcess` から merge される。
    #[serde(default)]
    pub port: Option<u16>,
    /// Process state ── daemon の projects 応答の `process_status` が SSOT。
    /// daemon は `process_status` キーで送るため `alias` で受け、 WebView へは
    /// `state` キーで serialize する (sidebar JS が `p.state` を読む)。
    #[serde(default, alias = "process_status")]
    pub state: ProcessStatus,
    /// Model Q: daemon canonical の active lane (presence)。projects 応答の
    /// per-project active_lane。 boot 時の復元に使う (session.json でなく daemon が源)。
    #[serde(default)]
    pub active_lane: Option<String>,
}

// 旧 HTTP `GET /api/daemon/projects` の包み (`{"projects": [...]}`) は撤去した。
// Unison `daemon-control.projects/list` は裸配列を返すため (doc 45 段 3)。
// 新旧が同じ `ProjectInfo` 一覧に落ちることは `daemon_control` の decode parity テストが固定する。

/// `/api/health` の主要 field のみを取り出した軽量レスポンス
///
/// vp-app の Activity widget で表示するため、daemon 側 `HealthResponse` の
/// stands / terminal_token / pid 等は無視。サーバ側の field 追加で壊れないよう
/// `#[serde(default)]` を付けている。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct DaemonHealthInfo {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub started_at: String,
    /// chronista-hub federation 接続状態
    /// （`"disabled"` | `"connecting"` | `"connected"` | `"disconnected"`、旧 daemon は空文字）。
    #[serde(default)]
    pub hub: String,
    /// hub の向こうに居る available nodes（`hub_nodes[]`、旧 daemon は field 不在 → 空）。
    /// daemon 側と同形なので `crate::pane::HubNode` をそのまま deserialize に使う。
    #[serde(default)]
    pub hub_nodes: Vec<crate::pane::HubNode>,
    /// L1 lifecycle: Daemon 配下 SP の presence 一覧（daemon-canonical、sidebar の ●◐○ 用）。
    /// 旧 daemon は field 不在 → 空。`path` で project 行に join する。
    #[serde(default)]
    pub processes: Vec<SpPresence>,
    /// in-app update: 新しい release が GitHub にあるか（daemon の定期チェック cache 由来）。
    /// 旧 daemon は field 不在 → false。sidebar「更新する」ボタンの表示 gate。
    #[serde(default)]
    pub update_available: bool,
    /// 最新 release version（`update_available` 時のボタン label 用、未取得は None）。
    #[serde(default)]
    pub latest_version: Option<String>,
}

/// SP の接続 presence 1 件（`/api/health` の `processes[]` 要素の lite subset）。
///
/// server 側 `ProcessHealthInfo` の {path, presence} のみ deserialize（dot 描画に必要な分）。
/// 残り field（project/port/pid/tmux_session）は serde が無視する。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpPresence {
    #[serde(default)]
    pub path: String,
    /// `"unregistered"` | `"connected"`（2 値、doc 44 §5.5 PR3）。
    #[serde(default)]
    pub presence: String,
}

/// Runtime process 情報 — Unison `registry.list` の `processes[]` 要素
/// (= 稼働中 project の lifecycle snapshot)。
///
/// server 側 `RunningProcess` (= `capability::process_manager_capability::RunningProcess`) の
/// subset で、 命名も揃える。 vp-app では Activity widget の count と
/// `fetch_projects_with_ports` での port join に使う。
///
/// doc 45 段 3 で transport は `GET /api/daemon/processes` から `registry.list` に移ったが、
/// daemon は同じ `running_processes` map を両面で共有しているので中身は同一。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RunningProcess {
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub port: u16,
}

// `LaneAddressWire` の定義は `crate::lane::LaneAddressWire` に移管 (R-0、 G2 解消)。
// 本 file 上部の `use crate::lane::LaneAddressWire;` で bring-into-scope 済。

/// Lane info (SP `/api/lanes` レスポンス要素)
///
/// vantage-point 側 `lanes_state::LaneInfo` の wire shape。
/// vp-app は `vantage-point` に依存しないので独立 lite struct で deserialize。
/// UI 表示 (sidebar の Lane 行) に必要な field のみ。
/// Serialize は SidebarState 経由で webview / disk persistence に流れるため必要。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct LaneInfo {
    pub address: LaneAddressWire,
    // doc 44 P2: `kind` / `name` を撤去（server 側 `lanes_state::LaneInfo` と対）。
    // どちらも `address` が持つ情報の複製で、真実源が 2 つある状態だった。
    // lane 名は `address.name` が唯一の在処、開発起点は予約名で表される。
    /// "spawning" | "running" | "exiting" | "dead"
    #[serde(default)]
    pub state: String,
    /// "echoes" | "shell"
    #[serde(default)]
    pub stand: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub cwd: String,
    /// Phase 5-D: Performer Lane のみ有効、 git workspace の状態 snapshot。
    #[serde(default)]
    pub performer_status: Option<PerformerStatusWire>,
    /// doc 37: active engine の session id（claude=cc_session / codex=thread id / grok=ACP sessionId、
    /// shell=None）。Echoes 共通ヘッダの session chip 用（表示専用）。旧 SP からは欠落 = None。
    #[serde(default)]
    pub engine_session_id: Option<String>,
    /// doc 39 P4: root session の stand（= slot に載る engine 種別）。Act I の session chip prefix は
    /// これを優先し（cross-engine root で slot の engine を正しく映す）、無ければ `stand`（lane 固定）に
    /// fallback。旧 SP からは欠落 = None。
    #[serde(default)]
    pub engine_stand: Option<String>,
    /// doc 40 §3 / doc 50 §4.6 A6: lane の session 構造（registry snapshot）。
    /// server（`lanes_state::LaneInfo.sessions`）が enrich して流している値で、
    /// 「どの session が root か」「各 session の act」の SSOT。boot 経路が xterm を
    /// (lane, session) で ensure するのに使う。旧 SP からは欠落 = None。
    #[serde(default)]
    pub sessions: Option<LaneSessionsWire>,
    /// FSM 投影 (2026-07-11): dev-flow FSM の現在 state。 "idle" | "working" | "hitl_pending" |
    /// "awaiting_user" | "completed" | "stuck"。 daemon が snapshot 送信時に enrich する
    /// (source = `vp flow progress` と同一判定)。 欠落 (旧 daemon) = None → sidebar は
    /// pid heuristic に fallback。 conductor lane は常に None (dev-flow FSM の対象外)。
    #[serde(default)]
    pub flow_state: Option<String>,
}

/// session act の serde default（旧 wire に field が無い時）。doc 53 R1: 旧 lane 単位
/// `console_mode` field は退役 — act の導出は `sessions`（registry snapshot）から行う
/// （Rust 側 = `app::root_act_of` / TS 側 = `sidebar/lane.ts rootActOf` の各 1 箇所）。
fn default_act() -> String {
    "tui".to_string()
}

/// lane の session roster（server `lanes_state::LaneSessionsView` の鏡）。
///
/// doc 50 §4.6 A6: 「どの session が root か」「各 session の act（tui/chat）」を boot 経路が
/// 読み、xterm を (lane, session) 単位で ensure するのに使う。
/// **doc 53 §11: GUI の roster 供給はこれ 1 本**（旧 `echoes_session_list` の fetch は
/// client から退役 — GUI 自身の動詞でしか撃たれず、CLI / MCP 由来の変化が pane に
/// 出なかった）。webview の tab strip / pane grid もここから流す。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct LaneSessionsWire {
    /// lane の器（slot / mailbox）に化身する session の key。
    #[serde(default = "default_root_session")]
    pub root: u32,
    /// 現在 focus されている session の key。
    #[serde(default = "default_root_session")]
    pub focused: u32,
    /// session 一覧（生成順）。
    #[serde(default)]
    pub sessions: Vec<LaneSessionEntryWire>,
}

/// [`LaneSessionsWire`] の 1 session。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct LaneSessionEntryWire {
    pub key: u32,
    /// engine 種別（stand 名）。
    #[serde(default)]
    pub stand: String,
    /// この session の Act（"tui" | "chat"）。serde default = "tui"（wire 後方互換）。
    #[serde(default = "default_act")]
    pub act: String,
    /// engine の会話 id（Draft = None）。session chip / tab の表示用（doc 53 §11）。
    #[serde(default)]
    pub conversation: Option<String>,
    /// この session を Chat にできるか（能力表は server が SSOT = `EngineKind`）。
    /// 名札の kind badge がこれで gate する。旧 server は送らない → false（不可に倒す）。
    #[serde(default)]
    pub chat_capable: bool,
}

/// root / focused の serde default（field 欠落 = 従来の「#1」）。
fn default_root_session() -> u32 {
    1
}

/// Phase 5-D: vantage-point 側 `lane::commands::PerformerStatus` の wire shape。
/// sidebar Performer row に branch / dirty / ahead / behind / merge 状態を表示。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct PerformerStatusWire {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub dirty_count: usize,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    #[serde(default)]
    pub has_upstream: bool,
    #[serde(default)]
    pub last_commit: String,
    #[serde(default)]
    pub is_merged: bool,
}

/// doc 11 PR-C: daemon process-proxy ask `stands_list` 応答 (`{stands:[...]}`) の 1 entry。
///
/// SP 側 `process::routes::stands::StandInfo` と wire 互換 (snake_case 統一済)。 F6④ で SP 直結
/// HTTP は撤去したが、 本 struct は ask 応答の deserialize + JS push back の serialize 用に残置。
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct StandInfo {
    /// `vp:stand:{name}` の name 部分 (例: `"echoes"` / `"shell"` / `"tmux"`、 PR-pre2 で hd → echoes rename)
    pub name: String,
    /// task ファイル先頭の `#MISE description="..."` の値
    #[serde(default)]
    pub description: String,
}

impl DaemonRpcClient {
    // doc 45 段 3: `new(port)` は撤去した。 port 指定で HTTP を叩いていたのは control plane の
    // 呼び出し元だけで、 それらは Unison に移った (daemon port は共有 connection manager が持つ)。
    // 残る唯一の caller は `Default` (= `VP_DAEMON_URL` / profile 既定の解決) なので、
    // 使われない ctor を「いつか誰か使う」で残さない。

    /// 任意の base URL で作成 (env var override / テスト用)
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    /// `/api/health` の中身を取得 (Activity widget 用)
    ///
    /// doc 45 §2: health は Unison に寄せない — 「他が壊れている時に動いてほしい」probe を
    /// Unison に載せると、Unison 層が wedge した時に診断手段ごと失う。
    /// `.mise/tasks/app/swap` (Ruby) や Swift menu bar agent も同じ endpoint を叩いており、
    /// それらに Unison client を持たせる理由もない。
    pub async fn daemon_health(&self) -> Result<DaemonHealthInfo> {
        let url = format!("{}/api/health", self.base_url);
        let info: DaemonHealthInfo = self.client.get(&url).send().await?.json().await?;
        Ok(info)
    }
}

impl Default for DaemonRpcClient {
    fn default() -> Self {
        Self::with_base_url(default_base_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// daemon は projects 応答を `process_status` キーで送る。
    /// `ProjectInfo.state` の `#[serde(alias = "process_status")]` で受けられること。
    #[test]
    fn project_info_deserializes_process_status_alias() {
        let json = r#"{"name":"vp","path":"/repos/vp","process_status":"running"}"#;
        let info: ProjectInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, ProcessStatus::Running);
    }

    /// `process_status` が無い JSON は default の Stopped になること (= 安全側)。
    #[test]
    fn project_info_defaults_to_stopped_when_status_absent() {
        let json = r#"{"name":"vp","path":"/repos/vp"}"#;
        let info: ProjectInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, ProcessStatus::Stopped);
    }

    /// WebView の sidebar JS は `p.state` を読む。 serialize は primary キー
    /// `state` で出る (alias は deserialize 専用で serialize には影響しない)。
    #[test]
    fn project_info_serializes_as_state_key() {
        let info = ProjectInfo {
            state: ProcessStatus::Running,
            ..ProjectInfo::default()
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""state":"running""#), "got: {json}");
        assert!(!json.contains("process_status"), "got: {json}");
    }
}
