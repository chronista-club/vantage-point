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

/// PID が生存しているか確認（kill(pid, 0) でシグナルを送らずチェック）
fn is_pid_alive(pid: u32) -> bool {
    if let Ok(pid_i32) = i32::try_from(pid) {
        // SAFETY: signal 0 はプロセスに何もしない。存在確認のみ。
        unsafe { libc::kill(pid_i32, 0) == 0 }
    } else {
        false
    }
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

/// Conductor Capability
#[derive(Clone)]
pub struct ProcessManagerCapability {
    /// 現在の状態
    state: CapabilityState,
    /// 登録プロジェクト一覧（キー: 正規化パス）
    projects: Arc<RwLock<HashMap<String, ProjectInfo>>>,
    /// 稼働中Process一覧（キー: 正規化パス）
    running_processes: Arc<RwLock<HashMap<String, RunningProcess>>>,
    /// 前回のヘルスチェックで稼働中だった Process（クラッシュ検知用）
    previously_running: Arc<RwLock<HashMap<String, RunningProcess>>>,
    /// 設定
    config: Option<Config>,
    /// vpバイナリパス
    vp_binary_path: Option<PathBuf>,
}

impl ProcessManagerCapability {
    /// 新しいProcessManagerCapabilityを作成
    pub fn new() -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            projects: Arc::new(RwLock::new(HashMap::new())),
            running_processes: Arc::new(RwLock::new(HashMap::new())),
            previously_running: Arc::new(RwLock::new(HashMap::new())),
            config: None,
            vp_binary_path: None,
        }
    }

    /// running_processes の共有参照を取得（DaemonState と共有するため）
    pub fn running_processes_ref(&self) -> Arc<RwLock<HashMap<String, RunningProcess>>> {
        self.running_processes.clone()
    }

    /// projects の共有参照を取得（DaemonState と共有するため）
    pub fn projects_ref(&self) -> Arc<RwLock<HashMap<String, ProjectInfo>>> {
        self.projects.clone()
    }

    /// 設定を読み込み
    pub async fn load_config(&mut self) -> CapabilityResult<()> {
        let config = Config::load().map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to load config: {}", e))
        })?;

        // プロジェクト一覧を更新
        let mut projects = self.projects.write().await;
        projects.clear();

        for project in &config.projects {
            let key = normalize_path_key(&PathBuf::from(&project.path));
            projects.insert(
                key,
                ProjectInfo {
                    name: project.name.clone(),
                    path: project.path.clone().into(),
                    process_status: ProcessStatus::Stopped,
                },
            );
        }

        self.config = Some(config);
        Ok(())
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

    /// プロジェクト一覧を取得
    pub async fn list_projects(&self) -> Vec<ProjectInfo> {
        let projects = self.projects.read().await;
        projects.values().cloned().collect()
    }

    /// 稼働中Process一覧を取得
    pub async fn list_running_processes(&self) -> Vec<RunningProcess> {
        let procs = self.running_processes.read().await;
        procs.values().cloned().collect()
    }

    /// config.toml を再読み込みして projects を更新（新規プロジェクトの動的追加対応）
    pub async fn reload_config(&self) {
        if let Ok(config) = Config::load() {
            let mut projects = self.projects.write().await;
            for project in &config.projects {
                let key = normalize_path_key(&PathBuf::from(&project.path));
                // 既存エントリは上書きしない（稼働状態を保持）
                projects.entry(key).or_insert_with(|| ProjectInfo {
                    name: project.name.clone(),
                    path: project.path.clone().into(),
                    process_status: ProcessStatus::Stopped,
                });
            }
            tracing::info!("Config reloaded: {} projects", projects.len());
        }
    }

    /// プロジェクトを追加（+ config.toml に永続化）
    pub async fn add_project(&self, name: &str, path: &str) -> CapabilityResult<ProjectInfo> {
        let key = normalize_path_key(&PathBuf::from(path));

        let info = ProjectInfo {
            name: name.to_string(),
            path: path.into(),
            process_status: ProcessStatus::Stopped,
        };

        {
            let mut projects = self.projects.write().await;
            if projects.contains_key(&key) {
                return Err(CapabilityError::Other(format!(
                    "Project already exists: {}",
                    path
                )));
            }
            projects.insert(key, info.clone());
        }

        self.persist_projects().await;
        Ok(info)
    }

    /// プロジェクトを削除（+ config.toml に永続化）
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

        self.persist_projects().await;
        Ok(())
    }

    /// プロジェクト名を変更（+ config.toml に永続化）
    pub async fn rename_project(&self, path: &str, new_name: &str) -> CapabilityResult<()> {
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

        self.persist_projects().await;
        Ok(())
    }

    /// プロジェクトの並び順を更新（+ config.toml に永続化）
    ///
    /// paths の順序で config.toml に書き出す。
    pub async fn reorder_projects(&self, paths: &[String]) -> CapabilityResult<()> {
        // 並び順は config.toml の `[[projects]]` 順で管理
        // HashMap は順序を持たないため、永続化時に paths の順で書き出す
        self.persist_projects_ordered(paths).await;
        Ok(())
    }

    /// projects HashMap を config.toml に永続化
    async fn persist_projects(&self) {
        let projects = self.projects.read().await;
        let ordered: Vec<String> = projects.keys().cloned().collect();
        drop(projects);
        self.persist_projects_ordered(&ordered).await;
    }

    /// 指定順序で config.toml に永続化
    async fn persist_projects_ordered(&self, order: &[String]) {
        let projects = self.projects.read().await;

        let mut config = Config::load().unwrap_or_default();
        config.projects = order
            .iter()
            .filter_map(|key| {
                projects.get(key).map(|info| crate::config::ProjectConfig {
                    name: info.name.clone(),
                    path: info.path.to_string_lossy().to_string(),
                    port: None,
                })
            })
            .collect();

        // order に含まれないプロジェクトも末尾に追加
        let order_set: std::collections::HashSet<&String> = order.iter().collect();
        for (key, info) in projects.iter() {
            if !order_set.contains(key) {
                config.projects.push(crate::config::ProjectConfig {
                    name: info.name.clone(),
                    path: info.path.to_string_lossy().to_string(),
                    port: None,
                });
            }
        }

        if let Err(e) = config.save() {
            tracing::error!("config.toml 永続化失敗: {}", e);
        } else {
            tracing::info!("config.toml 永続化完了: {} projects", config.projects.len());
        }
    }

    /// Processを起動
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

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Starting;
            }
        }

        // vp start を実行
        let mut cmd = Command::new(&vp_path);
        cmd.args(["start", "--headless"]);
        cmd.current_dir(&project.path);

        // バックグラウンドで起動
        let child = cmd
            .spawn()
            .map_err(|e| CapabilityError::Other(format!("Failed to start vp: {}", e)))?;

        let pid = child.id().unwrap_or(0);

        // 少し待ってからヘルスチェック
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

        // ポートをスキャンして見つける
        let port = self.find_process_port(&project.path).await.ok_or_else(|| {
            CapabilityError::Other("Failed to find Process port after startup".to_string())
        })?;

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

        let _ = client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;

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

        tracing::info!(project = project_name, "Process stopped");

        Ok(())
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
        if let Some(existing) = procs.get(&key) {
            if process.tmux_session.is_none() {
                process.tmux_session = existing.tmux_session.clone();
            }
        }
        procs.insert(key.clone(), process);
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
            tracing::info!("Process 登録解除: port={}, key={}", port, key);
        }
    }

    /// ポートスキャンでProcessを見つける
    async fn find_process_port(&self, project_path: &std::path::Path) -> Option<u16> {
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
                && std::path::Path::new(dir) == project_path
            {
                return Some(port);
            }
        }

        None
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

    #[test]
    fn test_process_status_serialize() {
        let status = ProcessStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"running\"");
    }
}
