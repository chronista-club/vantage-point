//! TheWorld daemon HTTP クライアント
//!
//! Mac の `TheWorldClient.swift` に相当する Rust 実装。
//! port 32000 の TheWorld と HTTP で対話する。
//!
//! ## URL 解決
//! 1. `VP_WORLD_URL` env var があれば優先 (例: `http://172.20.78.253:32000`)
//! 2. それ以外は `http://127.0.0.1:32000` (IPv4 loopback)
//!
//! **IPv6 `[::1]` は WSL2 → Windows の localhost 転送で通らない**ため
//! デフォルトは IPv4。WSL2 側で daemon を立ち上げて Windows の
//! vp-app から接続するケースを前提にしている。

use anyhow::Result;
use serde::Deserialize;

/// TheWorld の既定ポート
pub const DEFAULT_WORLD_PORT: u16 = 32000;

/// デフォルト URL 解決
///
/// `VP_WORLD_URL` env var → `http://127.0.0.1:32000`
fn default_base_url() -> String {
    std::env::var("VP_WORLD_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", DEFAULT_WORLD_PORT))
}

/// TheWorld daemon HTTP クライアント
pub struct TheWorldClient {
    base_url: String,
    client: reqwest::Client,
}

/// Process kind (Architecture v4: mem_1CaSwJ?... Process Recursive)
///
/// 全 VP entity (TheWorld / SP / Lane / Stand) は `ProcessKind` を持つ Process として
/// homogeneous に扱う。Display metaphor は UI / log の format string のみで使い、
/// code 内 logic は kind 直値で switch する。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    /// system 全体を supervise する root process (= TheWorld 👑)
    Supervisor,
    /// Project に bind された runtime container (= 旧 SP / Project Core ⭐)
    /// TheWorld の `/api/world/projects` response に kind field が無いケースは
    /// Runtime (= Project Process) 扱い (serde default)。
    #[default]
    Runtime,
    /// PTY session を持つ stream-based process (= Lane: Lead / Worker)
    Session,
    /// 機能 service を提供する worker process (= Stand: HD / TH / PP / GE / HP)
    Worker,
}

impl ProcessKind {
    /// Display 用 metaphor (UI / log の format string のみ、code logic では kind 直値で switch)
    pub fn metaphor(&self) -> &'static str {
        match self {
            ProcessKind::Supervisor => "👑 TheWorld",
            ProcessKind::Runtime => "⭐ Star Platinum",
            ProcessKind::Session => "📍 Lane",
            ProcessKind::Worker => "🦾 Stand",
        }
    }
}

/// Process state (全 ProcessKind 共通 state machine、Architecture v4 Idea 2)
///
/// `lanes_state::LaneState` (Spawning/Running/Exiting/Dead) を superset 包含し、
/// 全 ProcessKind に共通の state 軸として extend。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// 起動中 (Stand process spawn 中、ccws clone 中など)
    Spawning,
    /// 動作中 (PTY I/O 流れる、HTTP server listen 中)
    #[default]
    Running,
    /// 待機 (input 待ち、idle で work していない)
    Idle,
    /// 処理中 (active task ありで busy)
    Working,
    /// 一時停止 (suspended、resume 可能)
    Pausing,
    /// 終了中 (graceful shutdown)
    Exiting,
    /// 終了済 (process 死亡、auto-respawn or removed)
    Dead,
}

impl ProcessState {
    /// snake_case string representation (sidebar JS / log での state badge match 用)
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessState::Spawning => "spawning",
            ProcessState::Running => "running",
            ProcessState::Idle => "idle",
            ProcessState::Working => "working",
            ProcessState::Pausing => "pausing",
            ProcessState::Exiting => "exiting",
            ProcessState::Dead => "dead",
        }
    }
}

/// Process info (Architecture v4: 旧 ProjectInfo の Process abstraction 化)
///
/// TheWorld `/api/world/projects` レスポンス要素を Process として扱う。
/// kind / state field は v4 で追加、現状の TheWorld response に未含 (default で Runtime/Running)。
/// Sprint 2+ で TheWorld 側に kind/state を含める対応。
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct ProcessInfo {
    /// Process kind (default Runtime: TheWorld response 互換)
    #[serde(default)]
    pub kind: ProcessKind,
    pub name: String,
    /// Runtime kind の場合は git directory binding
    pub path: String,
    /// running の場合の port (Runtime のみ Some、Sprint 1 では旧 ProjectInfo と互換)
    #[serde(default)]
    pub port: Option<u16>,
    /// Process state (default Running)
    #[serde(default)]
    pub state: ProcessState,
}

// `pub type ProjectInfo = ProcessInfo;` alias は Phase 1a 完了で全 caller が ProcessInfo に移行、削除。
// Architecture v4: mem_1CaTpCQH8iLJ2PasRcPjHv

#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    projects: Vec<ProcessInfo>,
}

/// `/api/health` の主要 field のみを取り出した軽量レスポンス
///
/// vp-app の Activity widget で表示するため、TheWorld 側 `HealthResponse` の
/// stands / terminal_token / pid 等は無視。サーバ側の field 追加で壊れないよう
/// `#[serde(default)]` を付けている。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct WorldHealthInfo {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub started_at: String,
}

/// 稼働中 process 情報 (`/api/world/processes` レスポンス要素)
///
/// サーバ側 `RunningProcess` (capability/process_manager_capability.rs) の subset。
/// Activity widget で count にしか使わないので最小限。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RunningProcessInfo {
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub port: u16,
}

#[derive(Debug, Deserialize)]
struct ProcessesResponse {
    #[serde(default)]
    processes: Vec<RunningProcessInfo>,
}

/// Lane address (SP `/api/lanes` レスポンス要素の field)
///
/// 関連 memory: mem_1CaSugEk1W2vr5TAdfDn5D (多 scope architecture)、
/// mem_1CaSuu8xMyWqXzLiKHmYdV (使用範囲ベース scope rule)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaneAddressWire {
    #[serde(default)]
    pub project: String,
    /// "lead" | "worker"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Lane info (SP `/api/lanes` レスポンス要素)
///
/// vantage-point 側 `lanes_state::LaneInfo` の wire shape。
/// vp-app は `vantage-point` に依存しないので独立 lite struct で deserialize。
/// UI 表示 (sidebar の Lane 行) に必要な field のみ。
/// Serialize は SidebarState 経由で webview / disk persistence に流れるため必要。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaneInfo {
    pub address: LaneAddressWire,
    /// "lead" | "worker"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    /// "spawning" | "running" | "exiting" | "dead"
    #[serde(default)]
    pub state: String,
    /// "heavens_door" | "the_hand"
    #[serde(default)]
    pub stand: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub cwd: String,
    /// Phase 5-D: Worker Lane のみ有効、 git workspace の状態 snapshot
    #[serde(default)]
    pub worker_status: Option<WorkerStatusWire>,
}

/// Phase 5-D: vantage-point 側 `ccws::commands::WorkerStatus` の wire shape。
/// sidebar Worker row に branch / dirty / ahead / behind / merge 状態を表示。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerStatusWire {
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

#[derive(Debug, Deserialize)]
struct LanesResponse {
    #[serde(default)]
    lanes: Vec<LaneInfo>,
}

impl TheWorldClient {
    /// ポート指定で IPv4 loopback に向ける
    pub fn new(port: u16) -> Self {
        Self::with_base_url(format!("http://127.0.0.1:{}", port))
    }

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

    /// プロジェクト一覧を取得
    pub async fn list_projects(&self) -> Result<Vec<ProcessInfo>> {
        let url = format!("{}/api/world/projects", self.base_url);
        let resp: ProjectsResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.projects)
    }

    /// health check ping
    pub async fn ping(&self) -> Result<bool> {
        let url = format!("{}/api/health", self.base_url);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    /// `/api/health` の中身を取得 (Activity widget 用)
    pub async fn world_health(&self) -> Result<WorldHealthInfo> {
        let url = format!("{}/api/health", self.base_url);
        let info: WorldHealthInfo = self.client.get(&url).send().await?.json().await?;
        Ok(info)
    }

    /// 稼働中 process 一覧
    pub async fn list_processes(&self) -> Result<Vec<RunningProcessInfo>> {
        let url = format!("{}/api/world/processes", self.base_url);
        let resp: ProcessesResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.processes)
    }

    /// SP の `/api/lanes` を呼んで Lane 一覧を取得
    ///
    /// 用途: vp-app が project (= SP) ごとに Lane list を fetch して sidebar に反映する
    /// (A4-3a)。`base_url` に SP の URL (例: `http://127.0.0.1:33000`) を指定して
    /// この client を作る。同じ struct を World (32000) にも SP (33000+) にも使える。
    ///
    /// 関連 memory: mem_1CaSugEk1W2vr5TAdfDn5D (多 scope: Lane scope は SP の所有)
    pub async fn list_lanes(&self) -> Result<Vec<LaneInfo>> {
        let url = format!("{}/api/lanes", self.base_url);
        let resp: LanesResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.lanes)
    }

    /// プロジェクトを追加 (POST /api/world/projects)
    ///
    /// サーバ側 `AddProjectRequest`: `{ name: String, path: String }`
    /// 成功時はサーバが追加した `ProjectInfo` を返す (本実装では破棄)。
    pub async fn add_project(&self, name: &str, path: &str) -> Result<()> {
        let url = format!("{}/api/world/projects", self.base_url);
        let body = serde_json::json!({ "name": name, "path": path });
        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("add_project HTTP {}: {}", status, text);
        }
        Ok(())
    }

    /// SP (Project Process = Star Platinum ⭐) を起動 (POST /api/world/processes/{name}/start)
    ///
    /// TheWorld が `vp sp start -C <project_path>` を `current_dir = project_path` で
    /// child process として fork する。 完了後 SP は QUIC で TheWorld に self-register、
    /// もしくは TheWorld の port scanner (33000-33010) が pick up する (D10 Reconciliation)。
    ///
    /// vp-app は「Current project が dead 状態」 のときの auto-spawn trigger として呼ぶ。
    /// State は TheWorld 側にある (mem_1CaTpCQH8iLJ2PasRcPjHv: 👑 が ⭐ を supervise) ので、
    /// 既に SP が立ち上がっていれば TheWorld 側で `Process already running` エラーが返る。
    pub async fn start_process(&self, project_name: &str) -> Result<()> {
        let url = format!(
            "{}/api/world/processes/{}/start",
            self.base_url, project_name
        );
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("start_process HTTP {}: {}", status, text);
        }
        Ok(())
    }

    /// Phase 5-C: Process restart (stop + 500ms grace + start chain)
    pub async fn restart_process(&self, project_name: &str) -> Result<()> {
        let url = format!(
            "{}/api/world/processes/{}/restart",
            self.base_url, project_name
        );
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("restart_process HTTP {}: {}", status, text);
        }
        Ok(())
    }

    /// Phase 3-A: SP に Worker Lane を create (`POST /api/lanes`)。
    /// `branch` 指定時は SP が `ccws new <name> <branch>` で worker dir を作成して spawn する。
    /// `base_url` は SP の URL (例: `http://127.0.0.1:33002`) を指定。
    pub async fn create_worker_lane(&self, name: &str, branch: Option<&str>) -> Result<()> {
        let url = format!("{}/api/lanes", self.base_url);
        let mut body = serde_json::json!({
            "kind": "worker",
            "name": name,
        });
        if let Some(b) = branch {
            body["branch"] = serde_json::Value::String(b.to_string());
        }
        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("create_worker_lane HTTP {}: {}", status, text);
        }
        Ok(())
    }

    /// Phase 4-A: SP の Worker Lane を削除 (`DELETE /api/lanes?address=<addr>`)。
    /// `address` は Display 形 (`<project>/worker/<name>`)。 Lead は server 側で 400 で拒否される。
    pub async fn delete_lane(&self, address: &str) -> Result<()> {
        // address は `/` を含むので URL encode する (worker/<name> 部分が path 化されないように)
        let encoded = address
            .replace('%', "%25")
            .replace('&', "%26")
            .replace('=', "%3D")
            .replace(' ', "%20");
        let url = format!("{}/api/lanes?address={}", self.base_url, encoded);
        let resp = self.client.delete(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("delete_lane HTTP {}: {}", status, text);
        }
        Ok(())
    }
}

impl Default for TheWorldClient {
    fn default() -> Self {
        Self::with_base_url(default_base_url())
    }
}
