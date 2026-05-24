//! Process Manager Capability - Process プロセス管理
//!
//! 複数のProject Processを管理するCapability。
//! メニューバーアプリ（Swift）からREST API経由で操作される。
//!
//! ## 役割
//!
//! - Project Processのライフサイクル管理（起動・停止・監視）
//! - QUIC Registry チャネル経由での Process 発見
//! - REST API提供
//!
//! ## 使用例
//!
//! ```ignore
//! let mut manager = ProcessManagerCapability::new();
//! manager.initialize(&ctx).await?;
//!
//! // プロジェクト一覧取得
//! let projects = world.list_projects().await;
//!
//! // Process起動
//! world.start_process("my-project").await?;
//! ```

use crate::capability::core::{Capability, CapabilityContext, CapabilityError, CapabilityResult};
use crate::capability::{CapabilityEvent, CapabilityInfo, CapabilityState};
use crate::config::Config;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::RwLock;

/// PID が生存しているか確認（crossplat）
fn is_pid_alive(pid: u32) -> bool {
    crate::platform::process_alive(pid)
}

/// プロジェクト情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// プロジェクト名
    pub name: String,
    /// プロジェクトパス
    pub path: PathBuf,
    /// Process状態
    pub process_status: ProcessStatus,
    /// 指定ポート（config.toml の port フィールド、永続化時に保持）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// SP 自動起動の有効/無効
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Port slot (VP-165: deterministic port layout)。 一度割り当てたら永続。
    /// VP-188: SSOT は projects.kdl。 capability は load/persist で round-trip するのみ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u16>,
}

fn default_enabled() -> bool {
    true
}

/// Process状態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    /// 停止中
    Stopped,
    /// 起動中
    Starting,
    /// 稼働中
    Running,
    /// 停止処理中
    Stopping,
    /// エラー
    Error,
}

/// 稼働中Process情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningProcess {
    /// プロジェクト名
    pub project_name: String,
    /// ポート番号
    pub port: u16,
    /// プロセスID
    pub pid: u32,
    /// プロジェクトパス
    pub project_path: PathBuf,
    /// tmux セッション名（`{project}-vp` 形式）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
}

/// 正規化パスキーを生成（HashMap のキーに使用）
///
/// ディレクトリパスを正規化した String を返す。
/// `running_processes` / `projects` の一意キーとして使用。
pub fn normalize_path_key(path: &std::path::Path) -> String {
    Config::normalize_path(path)
}

/// VP-165 PR-5b: `start_process` 内 `wait_for_health` の判定結果
#[derive(Debug)]
enum HealthCheckResult {
    /// `/api/health` の `project_dir` が期待値と一致 → 自分の SP が立った
    Ours,
    /// `/api/health` は応答したが `project_dir` が別 project → 外部衝突 (auto-reassign trigger)
    WrongProject(String),
    /// timeout だが port は listening = 非 VP プロセス占有 (auto-reassign trigger)
    Occupied,
    /// timeout かつ port も応答せず = SP crashed or never started
    Timeout,
}

/// Conductor Capability
#[derive(Clone)]
pub struct ProcessManagerCapability {
    /// 現在の状態
    state: CapabilityState,
    /// 登録プロジェクト一覧（キー: 正規化パス）— インメモリキャッシュ
    projects: Arc<RwLock<HashMap<String, ProjectInfo>>>,
    /// プロジェクトの並び順（正規化パスの Vec、config.toml の [[projects]] 順を保持）
    project_order: Arc<RwLock<Vec<String>>>,
    /// 稼働中Process一覧（キー: 正規化パス）— インメモリキャッシュ
    running_processes: Arc<RwLock<HashMap<String, RunningProcess>>>,
    /// 前回のヘルスチェックで稼働中だった Process（クラッシュ検知用）
    previously_running: Arc<RwLock<HashMap<String, RunningProcess>>>,
    /// Phase 1b: 各 Project の Lane registry（キー: 正規化パス）—
    /// SP が register payload に lanes を載せて push、 disconnect で全 Lane drop。
    /// agent (Echoes on Claude CLI) が `GET /api/lanes` で resolve するための cache。
    #[allow(clippy::type_complexity)]
    lane_registry: Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
    /// 設定
    config: Option<Config>,
    /// vpバイナリパス
    vp_binary_path: Option<PathBuf>,
    /// SurrealDB クライアント（Some なら DB に二重書き込み）
    vpdb: Option<crate::db::SharedVpDb>,
}

impl ProcessManagerCapability {
    /// 新しいProcessManagerCapabilityを作成
    pub fn new() -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            projects: Arc::new(RwLock::new(HashMap::new())),
            project_order: Arc::new(RwLock::new(Vec::new())),
            running_processes: Arc::new(RwLock::new(HashMap::new())),
            previously_running: Arc::new(RwLock::new(HashMap::new())),
            lane_registry: Arc::new(RwLock::new(HashMap::new())),
            config: None,
            vp_binary_path: None,
            vpdb: None,
        }
    }

    /// SurrealDB クライアントを設定
    pub fn set_vpdb(&mut self, vpdb: crate::db::SharedVpDb) {
        self.vpdb = Some(vpdb);
    }

    /// running_processes の共有参照を取得（DaemonState と共有するため）
    pub fn running_processes_ref(&self) -> Arc<RwLock<HashMap<String, RunningProcess>>> {
        self.running_processes.clone()
    }

    /// projects の共有参照を取得（DaemonState と共有するため）
    pub fn projects_ref(&self) -> Arc<RwLock<HashMap<String, ProjectInfo>>> {
        self.projects.clone()
    }

    /// Phase 1b: lane_registry の共有参照を取得（DaemonState と共有するため）
    #[allow(clippy::type_complexity)]
    pub fn lane_registry_ref(
        &self,
    ) -> Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>> {
        self.lane_registry.clone()
    }

    /// 設定を読み込み
    ///
    /// VP-188: registered projects の SSOT を embedded DB → `~/.config/vp/projects.kdl`
    /// に移行。 `Config::load()` が projects.kdl を `config.projects` にマージするため
    /// (= read 経路を 1 本化)、 ここは `config.projects` から HashMap に同期する。
    /// VP-182 の「DB dir 変更で projects 消失」 regression を構造的に解消 (council 2026-05-16)。
    pub async fn load_config(&mut self) -> CapabilityResult<()> {
        let config = Config::load().map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to load config: {}", e))
        })?;

        let mut projects = self.projects.write().await;
        let mut order = self.project_order.write().await;
        projects.clear();
        order.clear();

        for project in &config.projects {
            let key = normalize_path_key(&PathBuf::from(&project.path));
            order.push(key.clone());
            projects.insert(
                key,
                ProjectInfo {
                    name: project.name.clone(),
                    path: project.path.clone().into(),
                    process_status: ProcessStatus::Stopped,
                    port: None, // port は動的割当 (port_layout が slot から計算)
                    enabled: project.enabled,
                    slot: project.slot,
                },
            );
        }
        drop(projects);
        drop(order);

        self.config = Some(config);
        Ok(())
    }

    /// 現在の projects HashMap を projects.kdl に書き出す (VP-188)。
    ///
    /// `project_order` の順序で `ProjectsFile` を組み立てて atomic write する。
    /// add / delete / rename / reorder / set_enabled の各操作後に呼ぶ。
    /// test 環境では `ProjectsFile::save()` が no-op なので本番ファイルを破壊しない。
    async fn persist_projects(&self) -> CapabilityResult<()> {
        let projects = self.projects.read().await;
        let order = self.project_order.read().await;
        let entries: Vec<crate::projects_file::ProjectEntry> = order
            .iter()
            .filter_map(|key| {
                projects
                    .get(key)
                    .map(|p| crate::projects_file::ProjectEntry {
                        name: p.name.clone(),
                        path: p.path.to_string_lossy().to_string(),
                        // enabled=true は省略 (= projects.kdl をミニマムに)、 false のみ明記
                        enabled: if p.enabled { None } else { Some(false) },
                        slot: p.slot,
                    })
            })
            .collect();
        let pf = crate::projects_file::ProjectsFile { projects: entries };
        pf.save().map_err(|e| {
            CapabilityError::InitializationFailed(format!("projects.kdl 書き込み失敗: {}", e))
        })
    }

    /// vpバイナリを検索
    fn find_vp_binary() -> Option<PathBuf> {
        // 1. current_exe()（最も確実）
        if let Ok(exe) = std::env::current_exe()
            && exe.exists()
        {
            return Some(exe);
        }

        // 2. ~/.cargo/bin/vp
        if let Some(home) = dirs::home_dir() {
            let cargo_path = home.join(".cargo/bin/vp");
            if cargo_path.exists() {
                return Some(cargo_path);
            }
        }

        // 3. /usr/local/bin/vp
        let usr_local = PathBuf::from("/usr/local/bin/vp");
        if usr_local.exists() {
            return Some(usr_local);
        }

        // 4. PATH経由
        if let Ok(output) = std::process::Command::new("which").arg("vp").output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }

        None
    }

    /// プロジェクト名から正規化パスキーを解決
    ///
    /// `projects` HashMap を検索して name が一致するエントリのキー（正規化パス）を返す。
    /// 公開 API（start_process 等）が project_name を受け取り、内部キーに変換するために使用。
    async fn resolve_key_by_name(&self, project_name: &str) -> Option<String> {
        let projects = self.projects.read().await;
        projects
            .iter()
            .find(|(_, info)| info.name == project_name)
            .map(|(key, _)| key.clone())
    }

    /// プロジェクト一覧を取得（project_order の順序で返す）
    pub async fn list_projects(&self) -> Vec<ProjectInfo> {
        let projects = self.projects.read().await;
        let order = self.project_order.read().await;
        order
            .iter()
            .filter_map(|key| projects.get(key).cloned())
            .collect()
    }

    /// 稼働中Process一覧を取得
    pub async fn list_running_processes(&self) -> Vec<RunningProcess> {
        let procs = self.running_processes.read().await;
        procs.values().cloned().collect()
    }

    /// projects を projects.kdl と同期する（VP-188: projects.kdl 経由、 VP-189: 双方向同期）。
    ///
    /// projects.kdl にある project は in-memory に追加、 projects.kdl から消えた
    /// project は in-memory からも除去する。 後者は VP-189 の ghost project cleanup
    /// (= `vp sync` / 起動時 sync の projects.kdl 書き換え) を daemon の in-memory
    /// 状態に伝播させるための双方向同期。
    ///
    /// ただし **running process を持つ project は projects.kdl から消えていても残す**
    /// ── 稼働中 SP の取りこぼし防止 (安全側)。 ghost project は dir 消失で SP が
    /// 起動不可なので、 通常は running と ghost が両立しない。
    pub async fn reload_config(&self) {
        let Ok(config) = Config::load() else {
            return;
        };

        // running process の key を先に取得 (projects/order の write lock との入れ子回避)。
        let running: std::collections::HashSet<String> = {
            let procs = self.running_processes.read().await;
            procs.keys().cloned().collect()
        };

        let mut projects = self.projects.write().await;
        let mut order = self.project_order.write().await;

        // projects.kdl 由来の key 集合 (= 除去判定の基準)。
        let kdl_keys: std::collections::HashSet<String> = config
            .projects
            .iter()
            .map(|p| normalize_path_key(&PathBuf::from(&p.path)))
            .collect();

        // add: projects.kdl の各 project を in-memory に反映 (未登録なら追加)。
        for project in &config.projects {
            let key = normalize_path_key(&PathBuf::from(&project.path));
            projects.entry(key.clone()).or_insert_with(|| ProjectInfo {
                name: project.name.clone(),
                path: project.path.clone().into(),
                process_status: ProcessStatus::Stopped,
                port: project.port,
                enabled: project.enabled,
                slot: project.slot,
            });
            if !order.contains(&key) {
                order.push(key);
            }
        }

        // remove: projects.kdl から消えた entry を in-memory からも除去。
        // ただし running process を持つ key は残す (稼働中 SP を取りこぼさない)。
        projects.retain(|key, _| kdl_keys.contains(key) || running.contains(key));
        order.retain(|key| projects.contains_key(key));

        tracing::info!("Config reloaded: {} projects", projects.len());
    }

    /// プロジェクトを追加（+ projects.kdl に永続化、 VP-188）
    pub async fn add_project(&self, name: &str, path: &str) -> CapabilityResult<ProjectInfo> {
        // 名前バリデーション
        if name.trim().is_empty() {
            return Err(CapabilityError::Other(
                "Project name cannot be empty".to_string(),
            ));
        }

        // パスの存在・ディレクトリ確認
        let pb = PathBuf::from(path);
        if !pb.is_dir() {
            return Err(CapabilityError::Other(format!(
                "Path is not a directory: {}",
                path
            )));
        }

        let key = normalize_path_key(&pb);

        let info = ProjectInfo {
            name: name.to_string(),
            path: path.into(),
            process_status: ProcessStatus::Stopped,
            port: None,
            enabled: true,
            slot: None, // 新規 project は slot 未割当 (= SP 初回起動時に resolve が割当)
        };

        {
            let mut projects = self.projects.write().await;
            if projects.contains_key(&key) {
                return Err(CapabilityError::Other(format!(
                    "Project already exists: {}",
                    path
                )));
            }
            projects.insert(key.clone(), info.clone());
        }
        // 順序リストに末尾追加
        self.project_order.write().await.push(key.clone());

        // VP-188: projects.kdl に永続化
        self.persist_projects().await?;

        Ok(info)
    }

    /// プロジェクトを削除（+ projects.kdl に永続化、 VP-188）
    pub async fn remove_project(&self, path: &str) -> CapabilityResult<()> {
        let key = normalize_path_key(&PathBuf::from(path));

        // 稼働中なら停止を先にする必要がある
        {
            let procs = self.running_processes.read().await;
            if procs.contains_key(&key) {
                return Err(CapabilityError::Other(
                    "Cannot remove running project. Stop it first.".to_string(),
                ));
            }
        }

        {
            let mut projects = self.projects.write().await;
            if projects.remove(&key).is_none() {
                return Err(CapabilityError::Other(format!(
                    "Project not found: {}",
                    path
                )));
            }
        }
        // 順序リストからも削除
        self.project_order.write().await.retain(|k| k != &key);

        // VP-188: projects.kdl に永続化
        self.persist_projects().await?;

        Ok(())
    }

    /// プロジェクト名を変更（+ projects.kdl に永続化、 VP-188）
    pub async fn rename_project(&self, path: &str, new_name: &str) -> CapabilityResult<()> {
        if new_name.trim().is_empty() {
            return Err(CapabilityError::Other(
                "Project name cannot be empty".to_string(),
            ));
        }

        let key = normalize_path_key(&PathBuf::from(path));

        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.name = new_name.to_string();
            } else {
                return Err(CapabilityError::Other(format!(
                    "Project not found: {}",
                    path
                )));
            }
        }

        // VP-188: projects.kdl に永続化
        self.persist_projects().await?;

        Ok(())
    }

    /// プロジェクトの enabled/disabled を切り替え（+ projects.kdl に永続化）
    pub async fn set_project_enabled(&self, path: &str, enabled: bool) -> CapabilityResult<()> {
        let key = normalize_path_key(&PathBuf::from(path));

        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.enabled = enabled;
            } else {
                return Err(CapabilityError::Other(format!(
                    "Project not found: {}",
                    path
                )));
            }
        }

        // VP-188: projects.kdl に永続化
        self.persist_projects().await?;
        tracing::info!("Project enabled={}: {}", enabled, path);

        Ok(())
    }

    /// プロジェクトの並び順を更新（+ projects.kdl に永続化、 VP-188）
    pub async fn reorder_projects(&self, paths: &[String]) -> CapabilityResult<()> {
        // raw paths を正規化して HashMap キーと一致させる
        let normalized: Vec<String> = paths
            .iter()
            .map(|p| normalize_path_key(&PathBuf::from(p)))
            .collect();
        // 順序リストを更新
        *self.project_order.write().await = normalized.clone();

        // VP-188: projects.kdl に永続化
        self.persist_projects().await?;

        Ok(())
    }

    /// Processを起動
    /// VP-133 MVP: 指定 path に対して live SP が存在するか port range scan で確認。
    ///
    /// `running_processes` registry を bypass し、 `PORT_RANGE_START..=END` を直接 GET
    /// `/api/health` で query、 response の `project_dir` を `normalize_path_key` で正規化して
    /// `project_path` と match する SP を返す。
    ///
    /// **用途**: `start_process` で false positive 切断検知後の auto-spawn 重複を防ぐ
    /// dedup check。 registry が誤って空になっても、 ports は実 SP の listen 状態を反映する
    /// ので、 port scan が source of truth として機能する。
    ///
    /// **同 logic は `refresh_process_status` Phase 2 (line ~1045) でも使われているが、
    /// あちらは ghost detection / 自動 register の higher-level loop**。 本 helper は
    /// 「ある path に SP が live か」 の単純 query に絞り、 caller が分岐判断する形。
    async fn find_running_sp_at_path(
        &self,
        project_path: &std::path::Path,
    ) -> Option<RunningProcess> {
        let target_key = normalize_path_key(project_path);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .ok()?;

        for port in crate::cli::PORT_RANGE_START..=crate::cli::PORT_RANGE_END {
            let url = format!("http://[::1]:{}/api/health", port);
            let Ok(resp) = client.get(&url).send().await else {
                continue;
            };
            if !resp.status().is_success() {
                continue;
            }
            let Ok(json) = resp.json::<serde_json::Value>().await else {
                continue;
            };

            let project_dir = json
                .get("project_dir")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if project_dir.is_empty() {
                continue;
            }

            let key = normalize_path_key(std::path::Path::new(project_dir));
            if key != target_key {
                continue;
            }

            // path match — 既存 SP を return
            let pid = json.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let project_name = std::path::Path::new(project_dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            return Some(RunningProcess {
                project_name,
                port,
                pid,
                project_path: project_path.to_path_buf(),
                tmux_session: None,
            });
        }
        None
    }

    pub async fn start_process(&self, project_name: &str) -> CapabilityResult<RunningProcess> {
        let vp_path = self.vp_binary_path.clone().ok_or_else(|| {
            CapabilityError::InitializationFailed("vp binary not found".to_string())
        })?;

        // 名前→パスキー解決（見つからなければ config を再読み込みして再試行）
        let key = match self.resolve_key_by_name(project_name).await {
            Some(k) => k,
            None => {
                self.reload_config().await;
                self.resolve_key_by_name(project_name)
                    .await
                    .ok_or_else(|| {
                        CapabilityError::Other(format!("Project not found: {}", project_name))
                    })?
            }
        };

        let project = {
            let projects = self.projects.read().await;
            projects.get(&key).cloned()
        }
        .ok_or_else(|| CapabilityError::Other(format!("Project not found: {}", project_name)))?;

        // 既に起動中かチェック
        {
            let procs = self.running_processes.read().await;
            if procs.contains_key(&key) {
                return Err(CapabilityError::Other(format!(
                    "Process already running for project: {}",
                    project_name
                )));
            }
        }

        // VP-133 MVP: dedup port scan check ─ false positive 切断検知 (= QUIC heartbeat 一時失敗
        // 等で running_processes registry が誤って空になる) 後の auto-spawn を防ぐ。 registry
        // bypass で port 直 scan + path match を確認、 既存 SP 発見なら spawn skip + 再 register。
        //
        // 旧挙動 (= dedup check 不在) では、 false positive で registry 空 → start_process →
        // 旧 SP alive のまま新 port で spawn → multi-port 並走 → Health monitor が 30 秒毎に
        // ghost detect → 互殺 ping-pong cycle が永続化していた (VP-133 root cause)。
        if let Some(existing) = self.find_running_sp_at_path(&project.path).await {
            tracing::info!(
                "start_process: dedup check で既存 SP 発見 → spawn skip + re-register \
                 (project={}, port={}, pid={})",
                project_name,
                existing.port,
                existing.pid
            );
            {
                let mut projects = self.projects.write().await;
                if let Some(p) = projects.get_mut(&key) {
                    p.process_status = ProcessStatus::Running;
                }
            }
            {
                let mut procs = self.running_processes.write().await;
                procs.insert(key.clone(), existing.clone());
            }
            return Ok(existing);
        }

        // 状態を Starting に
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Starting;
            }
        }

        // VP-165 PR-5b: TheWorld が port allocation の authority。
        // - `sp_port_for_project` で slot ベースの port を解決 (新規割当なら config 永続)
        // - `vp sp start -p <port>` で port を明示渡し
        // - `wait_for_health(port, &path)` で `/api/health` の project_dir 一致を確認
        // - 外部衝突 (別 project SP / 非 VP process) なら 1 回きり auto-reassign + retry
        //
        // 旧 (PR-5 まで): `vp sp start -C <path>` (-p 無し) → 子の resolve_port が slot 解決 →
        // TheWorld が `wait_for_process_port` で range scan で discover、 だった。 PR-5b で
        // TheWorld が port を明示所有する形に整理。
        let project_path_str = project.path.to_string_lossy().to_string();
        let max_attempts = 2; // 初回 + auto-reassign 後 1 回
        let mut attempt = 0;
        let (port, pid) = loop {
            attempt += 1;

            // port 解決 (slot ベース、 新規割当なら config 永続)。 失敗時は find_available_port に fallback
            let port = match crate::resolve::sp_port_for_project(project_name) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        "VP-165: sp_port_for_project('{}') 失敗: {} → find_available_port にフォールバック",
                        project_name,
                        e
                    );
                    crate::resolve::find_available_port().ok_or_else(|| {
                        CapabilityError::Other(format!(
                            "VP-165: port 解決失敗かつ空き port もなし ({})",
                            e
                        ))
                    })?
                }
            };

            // vp sp start を子プロセスとして実行 (-p で port 明示)
            let mut cmd = Command::new(&vp_path);
            cmd.args([
                "sp",
                "start",
                "-C",
                &project_path_str,
                "-p",
                &port.to_string(),
            ]);
            cmd.current_dir(&project.path);
            let child = cmd
                .spawn()
                .map_err(|e| CapabilityError::Other(format!("Failed to start vp: {}", e)))?;
            let pid = child.id().unwrap_or(0);

            // health 確認 (port 既知なので range scan 不要)
            let health = self
                .wait_for_health(
                    port,
                    &project.path,
                    std::time::Duration::from_millis(800),
                    std::time::Duration::from_millis(500),
                    std::time::Duration::from_secs(10),
                )
                .await;

            match health {
                HealthCheckResult::Ours => break (port, pid),
                HealthCheckResult::WrongProject(actual) => {
                    if attempt >= max_attempts {
                        return Err(CapabilityError::Other(format!(
                            "VP-165: port {} は別 project SP ({}) が占有、 auto-reassign 後も解消せず",
                            port, actual
                        )));
                    }
                    let new_port = self.auto_reassign_slot(project_name, port).await?;
                    tracing::info!(
                        "VP-165 retry: project '{}' を新 port {} で再 spawn",
                        project_name,
                        new_port
                    );
                    // child は port bind に失敗してすぐ exit するはず。 念のため kill は
                    // しない (vp sp start 側の collision check が bail で抜ける)。
                    continue;
                }
                HealthCheckResult::Occupied => {
                    if attempt >= max_attempts {
                        return Err(CapabilityError::Other(format!(
                            "VP-165: port {} が外部 process に占有、 auto-reassign 後も解消せず",
                            port
                        )));
                    }
                    let new_port = self.auto_reassign_slot(project_name, port).await?;
                    tracing::info!(
                        "VP-165 retry: project '{}' を新 port {} で再 spawn (旧 {} は外部占有)",
                        project_name,
                        new_port,
                        port
                    );
                    continue;
                }
                HealthCheckResult::Timeout => {
                    return Err(CapabilityError::Other(format!(
                        "VP-165: SP startup timeout (port={}, project='{}')",
                        port, project_name
                    )));
                }
            }
        };

        let running_process = RunningProcess {
            project_name: project_name.to_string(),
            port,
            pid,
            project_path: project.path.clone(),
            tmux_session: None,
        };

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Running;
            }
        }

        {
            let mut procs = self.running_processes.write().await;
            procs.insert(key.clone(), running_process.clone());
        }

        // DB に書き込み（正規化パスで保存）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db
                .upsert_process(&key, project_name, port, pid, "running", None)
                .await
        {
            tracing::warn!("DB process 登録失敗: {}", e);
        }

        tracing::info!(
            project = project_name,
            port = port,
            pid = pid,
            "Process started"
        );

        Ok(running_process)
    }

    /// Processを停止
    pub async fn stop_process(&self, project_name: &str) -> CapabilityResult<()> {
        let key = self
            .resolve_key_by_name(project_name)
            .await
            .ok_or_else(|| {
                CapabilityError::Other(format!("Project not found: {}", project_name))
            })?;

        let running = {
            let procs = self.running_processes.read().await;
            procs.get(&key).cloned()
        };

        let running = running.ok_or_else(|| {
            CapabilityError::Other(format!("No running Process for project: {}", project_name))
        })?;

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Stopping;
            }
        }

        // POST /api/shutdown を送信
        let client = reqwest::Client::new();
        let url = format!("http://[::1]:{}/api/shutdown", running.port);

        if let Err(e) = client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            tracing::warn!(
                "shutdown リクエスト失敗 '{}' (port={}): {}",
                project_name,
                running.port,
                e
            );
        }

        // ロック順序統一: projects → running_processes
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Stopped;
            }
        }
        {
            let mut procs = self.running_processes.write().await;
            procs.remove(&key);
        }

        // DB から削除（正規化パスで削除）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db.delete_process(&key).await
        {
            tracing::warn!("DB process 削除失敗: {}", e);
        }

        tracing::info!(project = project_name, "Process stopped");

        Ok(())
    }

    /// Phase 5-C: SP を restart する。 stop → 短い grace period → start を atomic に chain。
    /// stop が「No running Process」 なら start のみ実行 (= ensure-running 的な挙動)。
    pub async fn restart_process(&self, project_name: &str) -> CapabilityResult<RunningProcess> {
        // stop が失敗しても start を試みる (= dead な project でも restart で起こす UX)
        match self.stop_process(project_name).await {
            Ok(()) => {
                tracing::info!(project = project_name, "Process stopped (for restart)");
                // grace period: shutdown signal の伝播 + port release を待つ
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                tracing::info!(
                    project = project_name,
                    "stop_process during restart failed (continuing to start): {}",
                    e
                );
            }
        }
        self.start_process(project_name).await
    }

    /// PointViewを開く
    pub async fn open_pointview(&self, project_name: &str) -> CapabilityResult<()> {
        let key = self.resolve_key_by_name(project_name).await;

        // Processが起動していなければ起動
        let running = if let Some(ref key) = key {
            let procs = self.running_processes.read().await;
            procs.get(key).cloned()
        } else {
            None
        };

        let running = match running {
            Some(s) => s,
            None => self.start_process(project_name).await?,
        };

        // POST /api/canvas/open を送信（将来的にはWebSocketで）
        let client = reqwest::Client::new();
        let url = format!("http://[::1]:{}/api/canvas/open", running.port);

        client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to open PointView: {}", e)))?;

        Ok(())
    }

    /// 外部 Process の自己登録（Process 起動時に呼ばれる）
    pub async fn register_external_process(&self, port: u16, project_dir: &str, pid: u32) {
        let key = normalize_path_key(std::path::Path::new(project_dir));
        let name = {
            let projects = self.projects.read().await;
            projects.get(&key).map(|p| p.name.clone())
        }
        .unwrap_or_else(|| {
            std::path::Path::new(project_dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        let mut process = RunningProcess {
            project_name: name.clone(),
            port,
            pid,
            project_path: project_dir.into(),
            tmux_session: None,
        };

        // プロジェクト状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Running;
            }
        }

        let mut procs = self.running_processes.write().await;
        // 既存の tmux_session を保持（QUIC 登録済みのセッション名を HTTP で上書きしない）
        if let Some(existing) = procs.get(&key)
            && process.tmux_session.is_none()
        {
            process.tmux_session = existing.tmux_session.clone();
        }
        procs.insert(key.clone(), process.clone());

        // DB に書き込み（正規化パスで保存）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db
                .upsert_process(
                    &key,
                    &name,
                    port,
                    pid,
                    "running",
                    process.tmux_session.as_deref(),
                )
                .await
        {
            tracing::warn!("DB process 登録失敗: {}", e);
        }

        tracing::info!(
            "Process 登録: port={}, dir={}, key={}",
            port,
            project_dir,
            key
        );
    }

    /// 外部 Process の登録解除（Process 停止時に呼ばれる）
    pub async fn unregister_external_process(&self, port: u16) {
        // Read-then-Act: まず read でキーを特定 → 解放 → 個別に write
        let key = {
            let procs = self.running_processes.read().await;
            procs
                .iter()
                .find(|(_, p)| p.port == port)
                .map(|(k, _)| k.clone())
        };

        if let Some(key) = key {
            // projects → running_processes の順で write（他の箇所と統一）
            {
                let mut projects = self.projects.write().await;
                if let Some(p) = projects.get_mut(&key) {
                    p.process_status = ProcessStatus::Stopped;
                }
            }
            {
                let mut procs = self.running_processes.write().await;
                procs.remove(&key);
            }

            // DB から削除（正規化パスで削除）
            if let Some(ref db) = self.vpdb
                && let Err(e) = db.delete_process(&key).await
            {
                tracing::warn!("DB process 登録解除失敗: {}", e);
            }

            tracing::info!("Process 登録解除: port={}, key={}", port, key);
        }
    }

    /// ポートスキャンでProcessを見つける
    ///
    /// `/api/health` の `project_dir` を `normalize_path_key` で正規化して比較し、
    /// symlink / trailing slash 等の path variation を吸収する。 VP-134 で
    /// `find_running_sp_at_path` (VP-133) と symmetry 復元。
    async fn find_process_port(&self, project_path: &std::path::Path) -> Option<u16> {
        let target_key = normalize_path_key(project_path);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .ok()?;

        for port in crate::cli::PORT_RANGE_START..=crate::cli::PORT_RANGE_END {
            let url = format!("http://[::1]:{}/api/health", port);
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
                && let Ok(json) = resp.json::<serde_json::Value>().await
                && let Some(dir) = json.get("project_dir").and_then(|v| v.as_str())
                && normalize_path_key(std::path::Path::new(dir)) == target_key
            {
                return Some(port);
            }
        }

        None
    }

    /// VP-165 PR-5b: 既知 port の `/api/health` を poll し、`project_dir` 一致を確認する。
    ///
    /// 旧 `wait_for_process_port` は range scan (33000-33024 を全部 GET して path 一致を探す)
    /// だったが、PR-5b で `start_process` が `-p <port>` で port を明示渡しするようになり、
    /// 既知 port の health を直 poll すれば足りる。さらに「health が別 project の dir を返す」
    /// = 別 project の SP が同 port にいる（外部衝突）ケースも区別できるようになり、
    /// auto-reassign の trigger になる。
    ///
    /// - `initial_delay` ~800ms: SP が axum listen + `/api/health` 準備完了するまでの最低時間
    /// - `poll_interval` ~500ms: retry 間隔
    /// - `total_timeout` ~10s: 諦めるまでの total
    async fn wait_for_health(
        &self,
        port: u16,
        expected_path: &std::path::Path,
        initial_delay: std::time::Duration,
        poll_interval: std::time::Duration,
        total_timeout: std::time::Duration,
    ) -> HealthCheckResult {
        let start = std::time::Instant::now();
        tokio::time::sleep(initial_delay).await;

        let url = format!("http://[::1]:{}/api/health", port);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let expected_normalized = Config::normalize_path(expected_path);

        loop {
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
                && let Ok(json) = resp.json::<serde_json::Value>().await
                && let Some(actual_dir) = json.get("project_dir").and_then(|v| v.as_str())
            {
                let actual_normalized = Config::normalize_path(std::path::Path::new(actual_dir));
                if actual_normalized == expected_normalized {
                    tracing::info!(
                        "SP startup health verified in {}ms (port={}, project_path={})",
                        start.elapsed().as_millis(),
                        port,
                        expected_path.display()
                    );
                    return HealthCheckResult::Ours;
                }
                // 別 project の SP が同 port にいる = 外部衝突 (auto-reassign trigger)
                tracing::warn!(
                    "Health 不一致: port={} expected={} actual={} ({}ms 経過)",
                    port,
                    expected_normalized,
                    actual_normalized,
                    start.elapsed().as_millis()
                );
                return HealthCheckResult::WrongProject(actual_normalized);
            }
            if start.elapsed() >= total_timeout {
                // timeout: 何かが port を握ってるか (Occupied) / 誰も応答しないか (Timeout)
                let occupied = std::net::TcpStream::connect_timeout(
                    &format!("[::1]:{}", port).parse().unwrap(),
                    std::time::Duration::from_millis(200),
                )
                .is_ok();
                tracing::warn!(
                    "SP startup health timeout after {}ms (port={}, occupied={})",
                    start.elapsed().as_millis(),
                    port,
                    occupied
                );
                return if occupied {
                    HealthCheckResult::Occupied
                } else {
                    HealthCheckResult::Timeout
                };
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// VP-165 PR-5b: 外部衝突時の slot 自動再割当 (1 回きり、 config 永続)
    ///
    /// `wait_for_health` が `WrongProject` / `Occupied` を返した時に呼ぶ。
    /// 旧 slot を解放 → 「現 slot でなく、他 project に未割当で、port が listening でない」
    /// slot を探して force-assign → config save。これで「外部衝突という実イベントに対して、
    /// その 1 project だけ 1 回きり別 slot に退避 + 永続」が実現する (config 編集による
    /// cascading shift とは別物 = bounded migration)。
    ///
    /// 25 slot 全部塞がってる極端な場合は Err で、 caller (`start_process`) が retry を諦める。
    async fn auto_reassign_slot(
        &self,
        project_name: &str,
        occupied_port: u16,
    ) -> CapabilityResult<u16> {
        let mut config = Config::load().map_err(|e| {
            CapabilityError::Other(format!("VP-165 reassign: config load 失敗: {}", e))
        })?;

        let layout = config.port_layout();
        let max_projects = layout.max_projects;
        let used = config.used_slots();
        let current_slot = config.resolve_slot_by_name(project_name);

        // 候補: 現 slot でない & 他 project に未割当 & port が listening でない
        let new_slot = (0..max_projects).find(|s| {
            Some(*s) != current_slot
                && !used.contains(s)
                && std::net::TcpStream::connect_timeout(
                    &format!("[::1]:{}", crate::cli::PORT_RANGE_START + s)
                        .parse()
                        .unwrap(),
                    std::time::Duration::from_millis(100),
                )
                .is_err()
        });

        let new_slot = new_slot.ok_or_else(|| {
            CapabilityError::Other(format!(
                "VP-165 auto-reassign: 空き slot なし (max_projects={}, occupied port={})",
                max_projects, occupied_port
            ))
        })?;

        // 旧 slot を解放してから新 slot を force-assign (ensure_slot は preferred が used 中だと
        // err なので、 unassign → ensure の順)
        if current_slot.is_some() {
            let _ = config.unassign_slot(project_name);
        }
        config
            .ensure_slot(project_name, Some(new_slot))
            .map_err(|e| {
                CapabilityError::Other(format!(
                    "VP-165 reassign: slot {} assign 失敗: {}",
                    new_slot, e
                ))
            })?;
        // VP-188: slot 永続化先は projects.kdl (config.toml ではない)。
        config.persist_projects_kdl().map_err(|e| {
            CapabilityError::Other(format!("VP-165 reassign: projects.kdl save 失敗: {}", e))
        })?;

        let new_port = crate::cli::PORT_RANGE_START + new_slot;
        tracing::warn!(
            "VP-165 auto-reassign: project '{}' slot {:?} → {}, port {} → {} (config 永続化済み)",
            project_name,
            current_slot,
            new_slot,
            occupied_port,
            new_port
        );
        Ok(new_port)
    }

    /// 全 Process の状態を更新（PID liveness check + ポートスキャン Reconciliation）
    ///
    /// 1. PID liveness check: 登録済み Process のゴースト除去
    /// 2. ポートスキャン Reconciliation: 未登録 SP の自動追加
    ///
    /// Push（QUIC 自己登録）が主パス、Pull（ポートスキャン）が安全網。
    /// どちらかが壊れてももう一方がカバーし、システムが正常状態に収束する。
    pub async fn refresh_process_status(&self) -> CapabilityResult<()> {
        let mut dead_names: Vec<String> = Vec::new();

        // ── Phase 1: PID liveness check（ゴースト除去）──
        {
            let procs = self.running_processes.read().await;
            for (name, proc) in procs.iter() {
                if proc.pid > 0 && !is_pid_alive(proc.pid) {
                    dead_names.push(name.clone());
                }
            }
        }

        if !dead_names.is_empty() {
            let mut procs = self.running_processes.write().await;
            for name in &dead_names {
                if let Some(removed) = procs.remove(name) {
                    tracing::info!(
                        "Reconcile: PID {} 死亡 → '{}' 除去 (port={})",
                        removed.pid,
                        name,
                        removed.port
                    );
                    // DB からも削除
                    if let Some(ref db) = self.vpdb
                        && let Err(e) = db.delete_process(name).await
                    {
                        tracing::warn!("DB process 削除失敗 (PID死亡): {}", e);
                    }
                }
            }
        }

        // ── Phase 2: ポートスキャン Reconciliation（未登録 SP の自動追加 + ゴースト除去）──
        //
        // 1プロジェクト1プロセスが原則。同名プロジェクトが複数ポートで見つかったら
        // 既に登録済みの方を優先し、ゴースト（古い方）は shutdown を送って停止する。
        // ループ中に発見した SP も tracked に追加して、同パスの2つ目をゴースト判定する。
        let mut tracked: HashMap<String, RunningProcess> = {
            let procs = self.running_processes.read().await;
            procs.clone()
        };
        let mut tracked_ports: std::collections::HashSet<u16> =
            tracked.values().map(|p| p.port).collect();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("reqwest Client 構築失敗");

        for port in crate::cli::PORT_RANGE_START..=crate::cli::PORT_RANGE_END {
            if tracked_ports.contains(&port) {
                continue; // 既に登録済みポート
            }

            let url = format!("http://[::1]:{}/api/health", port);
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
                && let Ok(json) = resp.json::<serde_json::Value>().await
            {
                let project_dir = json
                    .get("project_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let pid = json.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                if project_dir.is_empty() {
                    continue;
                }

                let key = normalize_path_key(std::path::Path::new(&project_dir));

                // プロジェクト名を解決（config の名前を優先）
                let project_name = {
                    let projects = self.projects.read().await;
                    projects.get(&key).map(|p| p.name.clone())
                }
                .unwrap_or_else(|| {
                    std::path::Path::new(&project_dir)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string()
                });

                // 同パスのプロジェクトが既に登録済みかチェック
                if let Some(existing) = tracked.get(&key) {
                    // 既に登録済み → このポートはゴースト。shutdown を送って停止
                    tracing::info!(
                        "Reconcile: ゴースト検出 '{}' (port={}, pid={}) — 既に port={} で稼働中 → shutdown",
                        project_name,
                        port,
                        pid,
                        existing.port
                    );
                    let shutdown_url = format!("http://[::1]:{}/api/shutdown", port);
                    let _ = client.post(&shutdown_url).send().await;
                    continue;
                }

                let process = RunningProcess {
                    project_name: project_name.clone(),
                    port,
                    pid,
                    project_path: project_dir.into(),
                    tmux_session: None,
                };

                tracing::info!(
                    "Reconcile: 未登録 SP 発見 → '{}' 追加 (port={}, pid={})",
                    project_name,
                    port,
                    pid
                );

                // ロック順序統一: projects → running_processes
                {
                    let mut projects = self.projects.write().await;
                    if let Some(p) = projects.get_mut(&key) {
                        p.process_status = ProcessStatus::Running;
                    }
                }
                {
                    let mut procs = self.running_processes.write().await;
                    procs.insert(key.clone(), process.clone());
                }
                // DB にも書き込み（正規化パス）
                if let Some(ref db) = self.vpdb
                    && let Err(e) = db
                        .upsert_process(&key, &project_name, port, pid, "running", None)
                        .await
                {
                    tracing::warn!("DB process 登録失敗 (Reconcile): {}", e);
                }
                // tracked を更新して後続ポートのゴースト検出に使う
                tracked.insert(key, process);
                tracked_ports.insert(port);
            }
        }

        // ── Phase 3: プロジェクト状態を最終同期 ──
        // running_processes と projects は同じパスキーなので直接比較可能
        let running_keys: std::collections::HashSet<String> = {
            let running = self.running_processes.read().await;
            running.keys().cloned().collect()
        };
        {
            let mut projects = self.projects.write().await;
            for (key, info) in projects.iter_mut() {
                info.process_status = if running_keys.contains(key) {
                    ProcessStatus::Running
                } else {
                    ProcessStatus::Stopped
                };
            }
        }

        Ok(())
    }

    /// 起動時設定の復帰: TheWorld 起動時に `enabled` な project の SP を自動起動する。
    ///
    /// daemon restart 後に working set を復元する (VP-207)。 TheWorld 起動時に
    /// バックグラウンドタスクとして 1 回だけ spawn される。
    /// 1. 5 秒待機 — QUIC 自己登録 + 初期スキャンが `running_processes` を埋める猶予
    /// 2. `refresh_process_status` で稼働中 SP をポートスキャン把握
    /// 3. `enabled == true` かつ未稼働の project を収集
    /// 4. 各 project を `start_process` で起動 (300ms ずらして burst 回避)
    ///
    /// 検出漏れがあっても `vp sp start` 側の collision check が bail するので
    /// 二重起動は安全。 lock 規律は `run_health_monitor` を踏襲する。
    pub async fn autostart_enabled_projects(world: Arc<RwLock<Self>>) {
        // QUIC 自己登録 + 初期スキャンが running_processes を埋める猶予。
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        // 稼働中 SP をポートスキャンで把握（read ガードは即解放）。
        {
            let w = world.read().await;
            if let Err(e) = w.refresh_process_status().await {
                tracing::warn!("autostart: 初期スキャン失敗: {}", e);
            }
        }

        // enabled かつ未稼働の project 名を収集。
        let targets: Vec<String> = {
            let w = world.read().await;
            let projects = w.projects.read().await;
            let running = w.running_processes.read().await;
            projects
                .values()
                .filter(|p| p.enabled)
                .filter(|p| !running.contains_key(&normalize_path_key(&p.path)))
                .map(|p| p.name.clone())
                .collect()
        };

        if targets.is_empty() {
            tracing::info!("autostart: 起動対象なし（全 enabled project が稼働中）");
            return;
        }
        tracing::info!(
            "autostart: {} project の SP を起動: {:?}",
            targets.len(),
            targets
        );

        // start_process は内部で sleep + ポートスキャンするため、read ガードを
        // 保持せず clone した cap で呼ぶ（run_health_monitor と同じ規律）。
        for name in &targets {
            let world_cap = {
                let w = world.read().await;
                w.clone()
            };
            match world_cap.start_process(name).await {
                Ok(p) => tracing::info!("autostart: '{}' 起動成功（port {}）", name, p.port),
                Err(e) => tracing::warn!("autostart: '{}' 起動失敗: {}", name, e),
            }
            // burst を避けて少しずらす。
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }

    /// ヘルスモニター: 定期的に PID 生存確認 + クラッシュ検知 + 自動再起動
    ///
    /// TheWorld 起動時にバックグラウンドタスクとして spawn される。
    /// 30秒間隔で以下を実行:
    /// 1. PID liveness check（QUIC 切断漏れのゴースト除去）
    /// 2. 前回稼働中だった Process が消えていたらクラッシュ検知 → 自動再起動
    pub async fn run_health_monitor(
        world: Arc<RwLock<Self>>,
        shutdown_token: tokio_util::sync::CancellationToken,
    ) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        // 最初の tick は即座に発火するのでスキップ
        interval.tick().await;

        // クラッシュ検知用: 連続して不在のカウント（1回の失敗では再起動しない）
        let mut missing_count: HashMap<String, u32> = HashMap::new();

        tracing::info!("Health monitor 起動（30秒間隔）");

        loop {
            tokio::select! {
                _ = interval.tick() => {},
                _ = shutdown_token.cancelled() => {
                    tracing::info!("Health monitor 停止");
                    return;
                }
            }

            // ── 読み取りフェーズ（ロックを短時間で解放）──
            let (current, restart_targets) = {
                let world = world.read().await;

                // 1. PID liveness check（QUIC 切断漏れのゴースト除去）
                if let Err(e) = world.refresh_process_status().await {
                    tracing::warn!("Health check: 状態更新失敗: {}", e);
                    continue;
                }

                // 2. クラッシュ検知判定
                let current = world.running_processes.read().await.clone();
                let previous = world.previously_running.read().await.clone();

                // 復帰した Process のカウントをリセット
                for name in current.keys() {
                    missing_count.remove(name);
                }

                // (path_key, project_name, port) — start_process には project_name を渡す
                let mut targets: Vec<(String, String, u16)> = Vec::new();
                for (path_key, prev_proc) in &previous {
                    if !current.contains_key(path_key) {
                        let count = missing_count.entry(path_key.clone()).or_insert(0);
                        *count += 1;

                        if *count < 2 {
                            tracing::debug!(
                                "Health check: Process '{}' が不在（{}/2回目、次回再確認）",
                                prev_proc.project_name,
                                count
                            );
                            continue;
                        }

                        tracing::warn!(
                            "Health check: Process '{}' (port {}) がクラッシュを検知（2回連続不在）",
                            prev_proc.project_name,
                            prev_proc.port
                        );
                        targets.push((
                            path_key.clone(),
                            prev_proc.project_name.clone(),
                            prev_proc.port,
                        ));
                    }
                }

                (current, targets)
            };
            // ── ここで world の read ガードが解放される ──

            // previously_running を更新（read ガード外で write ロック取得）
            {
                let world = world.read().await;
                *world.previously_running.write().await = current.clone();
            }

            // ── 書き込みフェーズ（再起動が必要な場合のみ）──
            // start_process は内部でスリープ + ポートスキャンがあるため、
            // read ガードを長時間保持しないよう clone して解放する
            for (path_key, project_name, _port) in &restart_targets {
                tracing::info!("Health check: Process '{}' を自動再起動中...", project_name);
                let world_cap = {
                    let w = world.read().await;
                    w.clone()
                };
                match world_cap.start_process(project_name).await {
                    Ok(new_proc) => {
                        tracing::info!(
                            "Health check: Process '{}' 再起動成功 (port {})",
                            project_name,
                            new_proc.port
                        );
                        missing_count.remove(path_key);
                        crate::notify::post_process_changed(new_proc.port, "restarted");
                    }
                    Err(e) => {
                        tracing::error!(
                            "Health check: Process '{}' 再起動失敗: {}",
                            project_name,
                            e
                        );
                    }
                }
            }

            let _ = &current; // current のライフタイムを明示（コンパイラ最適化防止用ではなく意図表示）
        }
    }

    /// VP-129: lane root を watch して wing dir 削除を SP DELETE に bridge する FSEvents watcher。
    ///
    /// **「folder = Lane 空間」 axiom の物理実装**。 user が Finder / `rm -rf` で wing dir を
    /// 削除した時、 OS の file system event (Mac → FSEvents、 Linux → inotify) → notify crate
    /// → 本 watcher が path → project 解決 → SP `DELETE /api/lanes` 自動発火、 sidebar /
    /// tmux / PtySlot が cascade で同期 cleanup される。
    ///
    /// D10 Reconciliation arch の **3rd path 拡張**: Push (QUIC heartbeat) + Pull (port scan) +
    /// **FSEvents (本 method)** の 3-trigger model 完成。
    ///
    /// ## project-local lane refactor PR 4c
    ///
    /// PR 1 で wing 配置が `<repo>/.vp/lanes/<name>` に移行したのを受けて、 旧 logic
    /// (= 単一 legacy global path watch + `<repo>-<name>` prefix parsing) を撤去し、
    /// `config.projects` の **各 project の `.vp/lanes/` を `Vec<watch>` で N path 同時監視**
    /// する形に書き直した。 dirname 解析は撤去、 [`resolve_lane_event`] で完全 path-based
    /// resolution。
    ///
    /// MVP scope (= 別 ticket で safety net 追加候補):
    /// - 動的 project add/remove: scope 外 (= startup snapshot で固定、 後 PR で hot-reload 検討)
    /// - self-loop 防止: scope 外 (= SP 経由削除も Remove event 発火、 二重 DELETE 走るが SP 側
    ///   404 で no-op、 log noise 許容)
    /// - spawn race: scope 外 (= 既存 spawn semaphore + atomic LanePool insert で吸収)
    /// - 詳細 EventKind 区別: Remove(_) 全 variant accept (= Mac FSEvents は RemoveKind 区別が薄い)
    pub async fn run_lane_watcher(
        world: Arc<RwLock<Self>>,
        shutdown_token: tokio_util::sync::CancellationToken,
    ) {
        use notify::{EventKind, RecursiveMode, Watcher};

        // 起動時 snapshot: config.projects から `.vp/lanes/` path map を build。
        // 動的 project 追加は本 PR scope 外 (= TODO: project register event を購読して
        // path_map を hot-reload)。
        let path_map = Self::build_lane_watch_path_map(&world).await;
        if path_map.is_empty() {
            tracing::info!("lane watcher: 監視対象 project なし (skip 起動)");
            return;
        }

        // notify event は std::sync::mpsc 風 closure callback で来る。 async loop で処理する
        // ため tokio mpsc に bridge (file_watcher.rs:379 と同型 pattern)。
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();
        let mut watcher =
            match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("lane watcher: recommended_watcher 構築失敗 (skip): {}", e);
                    return;
                }
            };

        // 各 project の `.vp/lanes/` を watch。 dir 不在は best-effort で create
        // (= wing がまだ作られてない project でも先に watch を arm する)。
        let mut watched_count = 0usize;
        for (watch_path, (project_name, _)) in &path_map {
            if !watch_path.exists()
                && let Err(e) = std::fs::create_dir_all(watch_path)
            {
                tracing::warn!(
                    "lane watcher: dir create 失敗 (skip) project={} path={}: {}",
                    project_name,
                    watch_path.display(),
                    e
                );
                continue;
            }
            if let Err(e) = watcher.watch(watch_path, RecursiveMode::NonRecursive) {
                tracing::warn!(
                    "lane watcher: watch 開始失敗 (skip) project={} path={}: {}",
                    project_name,
                    watch_path.display(),
                    e
                );
                continue;
            }
            watched_count += 1;
            tracing::info!(
                "lane watcher: project={} path={} 監視開始",
                project_name,
                watch_path.display()
            );
        }
        if watched_count == 0 {
            tracing::warn!("lane watcher: 全 project で watch 失敗、 watcher 起動 abort");
            return;
        }
        tracing::info!(
            "lane watcher 起動完了 (project={} 件、 mode=NonRecursive、 trigger=Remove → SP DELETE)",
            watched_count
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest Client 構築失敗");

        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    tracing::info!("lane watcher: shutdown signal、 停止");
                    break;
                }
                event_opt = rx.recv() => {
                    let Some(event) = event_opt else { break }; // channel closed
                    if !matches!(event.kind, EventKind::Remove(_)) {
                        continue;
                    }
                    Self::handle_lane_remove_event(&world, &client, &path_map, &event).await;
                }
            }
        }

        drop(watcher); // 明示 drop で watching 停止 (scope 終端でも自動だが意図表示)
        tracing::info!("lane watcher 終了");
    }

    /// `config.projects` から `<repo>/.vp/lanes/` path → (project_name, project_path) の
    /// HashMap を build する。 起動 snapshot 用 (= 動的更新は scope 外)。
    async fn build_lane_watch_path_map(
        world: &Arc<RwLock<Self>>,
    ) -> std::collections::HashMap<std::path::PathBuf, (String, String)> {
        let mut map = std::collections::HashMap::new();
        let world_read = world.read().await;
        let Some(config) = world_read.config.as_ref() else {
            return map;
        };
        for proj in &config.projects {
            let project_root = std::path::PathBuf::from(&proj.path);
            let lanes_dir = project_root.join(".vp").join("lanes");
            map.insert(lanes_dir, (proj.name.clone(), proj.path.clone()));
        }
        map
    }

    /// VP-129: Remove event 1 件を処理。 path → project 解決 → SP DELETE call。
    /// `run_lane_watcher` の inner、 各 path を独立処理。
    async fn handle_lane_remove_event(
        world: &Arc<RwLock<Self>>,
        client: &reqwest::Client,
        path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
        event: &notify::Event,
    ) {
        for path in &event.paths {
            let Some((project_name, project_path, wing_name)) = resolve_lane_event(path, path_map)
            else {
                continue;
            };

            // SP port 取得 (= running_processes registry)。 `project_path` は config の
            // String 型で持たれているので Path 変換してから normalize する。
            let port = {
                let world_read = world.read().await;
                let procs = world_read.running_processes.read().await;
                let key = normalize_path_key(std::path::Path::new(&project_path));
                procs.get(&key).map(|p| p.port)
            };
            let Some(port) = port else {
                tracing::debug!(
                    "lane watcher: SP not running for project={} (skip) wing={}",
                    project_name,
                    wing_name
                );
                continue;
            };

            // SP DELETE /api/lanes (cleanup=false、 dir は既に gone)。 self-loop case
            // (= SP 経由で削除されて dir が消えた → watcher が Remove 検知 → 本 DELETE 発火)
            // は SP 側で 404 (Lane not found) 返却、 log debug 落ち。
            // address は新 wing form (`<project>/wing/<name>`、 SP 側 parse_address は legacy
            // `worker` も alias で受理)。
            let address = format!("{}/wing/{}", project_name, wing_name);
            let address_enc = address.replace('/', "%2F");
            let url = format!(
                "http://[::1]:{}/api/lanes?address={}&cleanup=false",
                port, address_enc
            );
            tracing::info!(
                "lane watcher: dir removed → SP DELETE 発火 (project={}, wing={}, port={})",
                project_name,
                wing_name,
                port
            );
            match client.delete(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    tracing::info!("lane watcher: SP DELETE 成功 ({})", address);
                }
                Ok(r) => {
                    tracing::debug!(
                        "lane watcher: SP DELETE non-success (likely self-loop or already deleted): status={}, address={}",
                        r.status(),
                        address
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "lane watcher: SP DELETE 失敗 (port={}, address={}): {}",
                        port,
                        address,
                        e
                    );
                }
            }
        }
    }
}

/// lane Remove event 1 path を解決する純粋関数。 `path_map` (= `<.vp/lanes path>` → `(project_name,
/// project_path)`) から parent match で project を逆引きし、 path の file_name を wing 名として
/// 返す。
///
/// 戻り値: `Some((project_name, project_path, wing_name))` if 完全 match。 そうでなければ `None`。
/// - dotfile / 空 wing 名は skip (= `.git` 内ファイル等の伝播除外)
/// - path_map に登録されてない project 配下の path は skip
/// - I/O なしの pure fn (= test しやすい、 mock 不要)
fn resolve_lane_event(
    path: &std::path::Path,
    path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
) -> Option<(String, String, String)> {
    let parent = path.parent()?;
    let (project_name, project_path) = path_map.get(parent)?;
    let wing_name = path.file_name()?.to_str()?.to_string();
    if wing_name.is_empty() || wing_name.starts_with('.') {
        return None;
    }
    Some((project_name.clone(), project_path.clone(), wing_name))
}

impl Default for ProcessManagerCapability {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Capability for ProcessManagerCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "world-capability",
            env!("CARGO_PKG_VERSION"),
            "Process World - 複数のProject Processを統括管理",
        )
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
        if self.state != CapabilityState::Uninitialized {
            return Err(CapabilityError::AlreadyInitialized);
        }

        self.state = CapabilityState::Initializing;

        // vpバイナリを検索
        self.vp_binary_path = Self::find_vp_binary();
        if self.vp_binary_path.is_none() {
            tracing::warn!("vp binary not found in PATH");
        }

        // 設定を読み込み
        if let Err(e) = self.load_config().await {
            tracing::warn!("Failed to load config: {}", e);
        }

        // 初期状態チェック（PID liveness — SP は QUIC registry で自己登録する）
        if let Err(e) = self.refresh_process_status().await {
            tracing::warn!("Failed to refresh process status: {}", e);
        }

        self.state = CapabilityState::Idle;

        let project_count = self.projects.read().await.len();
        tracing::info!(
            projects = project_count,
            "ProcessManagerCapability initialized"
        );

        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        self.state = CapabilityState::Stopped;
        tracing::info!("ProcessManagerCapability shutdown");
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["process.*".to_string()]
    }

    async fn handle_event(
        &mut self,
        _event: &CapabilityEvent,
        _ctx: &CapabilityContext,
    ) -> CapabilityResult<()> {
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_world_capability_new() {
        let cap = ProcessManagerCapability::new();
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
    }

    // --- resolve_lane_event (project-local lane refactor PR 4c) ---

    fn make_path_map(
        entries: &[(&str, &str, &str)],
    ) -> std::collections::HashMap<std::path::PathBuf, (String, String)> {
        let mut m = std::collections::HashMap::new();
        for (lanes_dir, project_name, project_path) in entries {
            m.insert(
                std::path::PathBuf::from(lanes_dir),
                (project_name.to_string(), project_path.to_string()),
            );
        }
        m
    }

    #[test]
    fn resolve_lane_event_happy_path() {
        let map = make_path_map(&[(
            "/Users/makoto/repos/creo-memories/.vp/lanes",
            "creo-memories",
            "/Users/makoto/repos/creo-memories",
        )]);
        let path = std::path::Path::new(
            "/Users/makoto/repos/creo-memories/.vp/lanes/or-integration",
        );
        let resolved = resolve_lane_event(path, &map);
        assert_eq!(
            resolved,
            Some((
                "creo-memories".to_string(),
                "/Users/makoto/repos/creo-memories".to_string(),
                "or-integration".to_string(),
            ))
        );
    }

    #[test]
    fn resolve_lane_event_unknown_parent_returns_none() {
        let map = make_path_map(&[(
            "/Users/makoto/repos/creo-memories/.vp/lanes",
            "creo-memories",
            "/Users/makoto/repos/creo-memories",
        )]);
        // 知らない project 配下の path
        let path = std::path::Path::new("/Users/makoto/repos/other-repo/.vp/lanes/foo");
        assert_eq!(resolve_lane_event(path, &map), None);
    }

    #[test]
    fn resolve_lane_event_skips_dotfile_wing_name() {
        // `.git` や `.DS_Store` の Remove event (lane dir 内部からの伝播) を skip。
        // NonRecursive watch で arrive する可能性は低いが防御で。
        let map = make_path_map(&[(
            "/repo/.vp/lanes",
            "repo",
            "/repo",
        )]);
        let path = std::path::Path::new("/repo/.vp/lanes/.DS_Store");
        assert_eq!(resolve_lane_event(path, &map), None);
    }

    #[test]
    fn resolve_lane_event_skips_when_no_parent() {
        // ルート `/` は parent なし → None
        let map = make_path_map(&[("/repo/.vp/lanes", "repo", "/repo")]);
        let path = std::path::Path::new("/");
        assert_eq!(resolve_lane_event(path, &map), None);
    }

    #[test]
    fn resolve_lane_event_multiple_projects_match_correct_one() {
        let map = make_path_map(&[
            ("/repo-a/.vp/lanes", "repo-a", "/repo-a"),
            ("/repo-b/.vp/lanes", "repo-b", "/repo-b"),
        ]);
        let path_b = std::path::Path::new("/repo-b/.vp/lanes/wing-x");
        let resolved = resolve_lane_event(path_b, &map);
        assert_eq!(
            resolved,
            Some(("repo-b".to_string(), "/repo-b".to_string(), "wing-x".to_string()))
        );
    }

    #[tokio::test]
    async fn test_lane_registry_ref_returns_shared_arc() {
        // Phase 1b: lane_registry_ref で取得した Arc が DaemonState 共有用に使える
        // 同じ cap から複数回 lane_registry_ref() を呼んでも内部 HashMap は共有される
        let cap = ProcessManagerCapability::new();
        let lr1 = cap.lane_registry_ref();
        assert!(
            lr1.read().await.is_empty(),
            "新規 cap の lane_registry は empty"
        );

        // Arc 共有確認: 1 つのハンドル経由で書き込んで、もう 1 つで読める
        lr1.write()
            .await
            .insert("/tmp/test".to_string(), Vec::new());

        let lr2 = cap.lane_registry_ref();
        assert_eq!(lr2.read().await.len(), 1);
        assert!(lr2.read().await.contains_key("/tmp/test"));

        // 削除も共有
        lr1.write().await.remove("/tmp/test");
        assert!(lr2.read().await.is_empty());
    }

    #[test]
    fn test_process_status_serialize() {
        let status = ProcessStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn test_normalize_path_key_consistency() {
        // 同じパスの異なる表現が同じキーになることを確認
        let key1 = normalize_path_key(&PathBuf::from("/tmp/test-project"));
        let key2 = normalize_path_key(&PathBuf::from("/tmp/test-project/"));
        // 末尾スラッシュの正規化は Config::normalize_path に依存
        assert!(!key1.is_empty());
        assert!(!key2.is_empty());
    }

    #[test]
    fn test_project_info_port_serialization() {
        // port が Some のとき JSON に含まれることを確認
        let info = ProjectInfo {
            name: "test".to_string(),
            path: "/tmp/test".into(),
            process_status: ProcessStatus::Stopped,
            port: Some(33005),
            enabled: true,
            slot: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("33005"));

        // port が None のとき JSON に含まれないことを確認（skip_serializing_if）
        let info_no_port = ProjectInfo {
            name: "test".to_string(),
            path: "/tmp/test".into(),
            process_status: ProcessStatus::Stopped,
            port: None,
            enabled: true,
            slot: None,
        };
        let json_no_port = serde_json::to_string(&info_no_port).unwrap();
        assert!(!json_no_port.contains("port"));
    }

    // --- CRUD テスト（async） ---

    /// テスト用ヘルパー: 空の ProcessManagerCapability を作成
    fn make_test_cap() -> ProcessManagerCapability {
        ProcessManagerCapability::new()
    }

    #[tokio::test]
    async fn test_add_project_success() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        let result = cap.add_project("test-project", &path).await;
        assert!(result.is_ok());

        let info = result.unwrap();
        assert_eq!(info.name, "test-project");
        assert_eq!(info.process_status, ProcessStatus::Stopped);
        assert_eq!(info.port, None);
    }

    #[tokio::test]
    async fn test_add_project_duplicate_path() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_project("first", &path).await.unwrap();
        let result = cap.add_project("second", &path).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_add_project_empty_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        let result = cap.add_project("", &path).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_add_project_whitespace_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        let result = cap.add_project("   ", &path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_project_nonexistent_path() {
        let cap = make_test_cap();
        let result = cap
            .add_project("ghost", "/nonexistent/path/that/does/not/exist")
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a directory"));
    }

    #[tokio::test]
    async fn test_remove_project_success() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_project("removable", &path).await.unwrap();
        let result = cap.remove_project(&path).await;
        assert!(result.is_ok());

        // 削除後は一覧に含まれない
        let projects = cap.list_projects().await;
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn test_remove_project_not_found() {
        let cap = make_test_cap();
        let result = cap.remove_project("/nonexistent").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_rename_project_success() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_project("old-name", &path).await.unwrap();
        let result = cap.rename_project(&path, "new-name").await;
        assert!(result.is_ok());

        let projects = cap.list_projects().await;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "new-name");
    }

    #[tokio::test]
    async fn test_rename_project_empty_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_project("existing", &path).await.unwrap();
        let result = cap.rename_project(&path, "").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_rename_project_not_found() {
        let cap = make_test_cap();
        let result = cap.rename_project("/nonexistent", "new").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_key_by_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_project("findme", &path).await.unwrap();

        let found = cap.resolve_key_by_name("findme").await;
        assert!(found.is_some());

        let not_found = cap.resolve_key_by_name("nothere").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_list_projects_empty() {
        let cap = make_test_cap();
        let projects = cap.list_projects().await;
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn test_list_running_processes_empty() {
        let cap = make_test_cap();
        let procs = cap.list_running_processes().await;
        assert!(procs.is_empty());
    }
}
