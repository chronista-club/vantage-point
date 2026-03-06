//! Process Manager Capability - Process プロセス管理
//!
//! 複数のProject Processを管理するCapability。
//! メニューバーアプリ（Swift）からREST API経由で操作される。
//!
//! ## 役割
//!
//! - Project Processのライフサイクル管理（起動・停止・監視）
//! - Bonjour経由でのProcess発見
//! - REST API提供
//!
//! ## 使用例
//!
//! ```ignore
//! let mut manager = ProcessManagerCapability::new();
//! manager.initialize(&ctx).await?;
//!
//! // プロジェクト一覧取得
//! let projects = conductor.list_projects().await;
//!
//! // Process起動
//! conductor.start_process("my-project").await?;
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
    /// Bonjour発見か
    pub discovered_via_bonjour: bool,
}

/// Conductor Capability
pub struct ProcessManagerCapability {
    /// 現在の状態
    state: CapabilityState,
    /// 登録プロジェクト一覧
    projects: Arc<RwLock<HashMap<String, ProjectInfo>>>,
    /// 稼働中Process一覧
    running_processes: Arc<RwLock<HashMap<String, RunningProcess>>>,
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
            config: None,
            vp_binary_path: None,
        }
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
            projects.insert(
                project.name.clone(),
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
        // 1. ~/.cargo/bin/vp
        if let Some(home) = dirs::home_dir() {
            let cargo_path = home.join(".cargo/bin/vp");
            if cargo_path.exists() {
                return Some(cargo_path);
            }
        }

        // 2. /usr/local/bin/vp
        let usr_local = PathBuf::from("/usr/local/bin/vp");
        if usr_local.exists() {
            return Some(usr_local);
        }

        // 3. PATH経由
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

    /// Processを起動
    pub async fn start_process(&self, project_name: &str) -> CapabilityResult<RunningProcess> {
        let vp_path = self.vp_binary_path.clone().ok_or_else(|| {
            CapabilityError::InitializationFailed("vp binary not found".to_string())
        })?;

        // プロジェクト情報取得
        let project = {
            let projects = self.projects.read().await;
            projects.get(project_name).cloned()
        };

        let project = project.ok_or_else(|| {
            CapabilityError::Other(format!("Project not found: {}", project_name))
        })?;

        // 既に起動中かチェック
        {
            let procs = self.running_processes.read().await;
            if procs.contains_key(project_name) {
                return Err(CapabilityError::Other(format!(
                    "Process already running for project: {}",
                    project_name
                )));
            }
        }

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(project_name) {
                p.process_status = ProcessStatus::Starting;
            }
        }

        // vp start を実行
        let mut cmd = Command::new(&vp_path);
        cmd.arg("start");
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
            discovered_via_bonjour: false,
        };

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(project_name) {
                p.process_status = ProcessStatus::Running;
            }
        }

        {
            let mut procs = self.running_processes.write().await;
            procs.insert(project_name.to_string(), running_process.clone());
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
        let running = {
            let procs = self.running_processes.read().await;
            procs.get(project_name).cloned()
        };

        let running = running.ok_or_else(|| {
            CapabilityError::Other(format!("No running Process for project: {}", project_name))
        })?;

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(project_name) {
                p.process_status = ProcessStatus::Stopping;
            }
        }

        // POST /api/shutdown を送信
        let client = reqwest::Client::new();
        let url = format!("http://localhost:{}/api/shutdown", running.port);

        let _ = client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;

        // 稼働中リストから削除
        {
            let mut procs = self.running_processes.write().await;
            procs.remove(project_name);
        }

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(project_name) {
                p.process_status = ProcessStatus::Stopped;
            }
        }

        tracing::info!(project = project_name, "Process stopped");

        Ok(())
    }

    /// PointViewを開く
    pub async fn open_pointview(&self, project_name: &str) -> CapabilityResult<()> {
        // Processが起動していなければ起動
        let running = {
            let procs = self.running_processes.read().await;
            procs.get(project_name).cloned()
        };

        let running = match running {
            Some(s) => s,
            None => self.start_process(project_name).await?,
        };

        // POST /api/pointview を送信（将来的にはWebSocketで）
        let client = reqwest::Client::new();
        let url = format!("http://localhost:{}/api/pointview", running.port);

        client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to open PointView: {}", e)))?;

        Ok(())
    }

    /// ポートスキャンでProcessを見つける
    async fn find_process_port(&self, project_path: &PathBuf) -> Option<u16> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .ok()?;

        for port in 33000..=33010 {
            let url = format!("http://localhost:{}/api/health", port);
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
                && let Ok(json) = resp.json::<serde_json::Value>().await
                && let Some(dir) = json.get("project_dir").and_then(|v| v.as_str())
                && PathBuf::from(dir) == *project_path
            {
                return Some(port);
            }
        }

        None
    }

    /// 全Processの状態を更新（ヘルスチェック）
    pub async fn refresh_process_status(&self) -> CapabilityResult<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .map_err(|e| CapabilityError::Other(e.to_string()))?;

        let mut discovered: HashMap<String, RunningProcess> = HashMap::new();

        // ポートスキャン
        for port in 33000..=33010 {
            let url = format!("http://localhost:{}/api/health", port);
            if let Ok(resp) = client.get(&url).send().await
                && resp.status().is_success()
                && let Ok(json) = resp.json::<serde_json::Value>().await
            {
                let project_dir = json
                    .get("project_dir")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
                let pid = json.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                if let Some(project_path) = project_dir {
                    // プロジェクト名を探す
                    let projects = self.projects.read().await;
                    for (name, info) in projects.iter() {
                        if info.path == project_path {
                            discovered.insert(
                                name.clone(),
                                RunningProcess {
                                    project_name: name.clone(),
                                    port,
                                    pid,
                                    project_path: project_path.clone(),
                                    discovered_via_bonjour: false,
                                },
                            );
                            break;
                        }
                    }
                }
            }
        }

        // 稼働中リストを更新
        {
            let mut procs = self.running_processes.write().await;
            *procs = discovered.clone();
        }

        // プロジェクト状態を更新
        {
            let mut projects = self.projects.write().await;
            for (name, info) in projects.iter_mut() {
                info.process_status = if discovered.contains_key(name) {
                    ProcessStatus::Running
                } else {
                    ProcessStatus::Stopped
                };
            }
        }

        Ok(())
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
            "conductor-capability",
            env!("CARGO_PKG_VERSION"),
            "Process Conductor - 複数のProject Processを指揮・管理",
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

        // 初期状態をスキャン
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
        vec!["process.*".to_string(), "bonjour.*".to_string()]
    }

    async fn handle_event(
        &mut self,
        event: &CapabilityEvent,
        _ctx: &CapabilityContext,
    ) -> CapabilityResult<()> {
        // Bonjour発見イベントを処理
        if event.event_type == "bonjour.advertised" {
            // 新しいProcessが発見されたら状態を更新
            let _ = self.refresh_process_status().await;
        }

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
    fn test_conductor_capability_new() {
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
