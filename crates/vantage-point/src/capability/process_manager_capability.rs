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
    vpdb: Option<vp_db::SharedVpDb>,
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
    pub fn set_vpdb(&mut self, vpdb: vp_db::SharedVpDb) {
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
    /// DB が接続済みなら DB → HashMap 同期。
    /// DB が未接続なら config.toml → HashMap。
    /// 初回起動時（DB 空）は config.toml → DB マイグレーションも実行。
    pub async fn load_config(&mut self) -> CapabilityResult<()> {
        let config = Config::load().map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to load config: {}", e))
        })?;

        // DB があれば DB から読む + config.toml からマイグレーション
        if let Some(ref db) = self.vpdb {
            // DB の既存プロジェクトを取得
            let db_projects = db.list_projects().await.unwrap_or_default();

            if db_projects.is_empty() && !config.projects.is_empty() {
                // 初回マイグレーション: config.toml → DB
                tracing::info!(
                    "config.toml → DB マイグレーション: {} projects",
                    config.projects.len()
                );
                for (i, project) in config.projects.iter().enumerate() {
                    // 正規化パスで DB に保存（add_project と統一）
                    let normalized = normalize_path_key(&PathBuf::from(&project.path));
                    if let Err(e) = db
                        .upsert_project(&project.name, &normalized, i as i64)
                        .await
                    {
                        tracing::warn!("DB マイグレーション失敗 ({}): {}", project.name, e);
                    }
                }
            }

            // DB → HashMap 同期
            let db_projects = db.list_projects().await.unwrap_or_default();
            let mut projects = self.projects.write().await;
            let mut order = self.project_order.write().await;
            projects.clear();
            order.clear();

            for row in &db_projects {
                let name = row["name"].as_str().unwrap_or("").to_string();
                let path = row["path"].as_str().unwrap_or("").to_string();
                let key = normalize_path_key(&PathBuf::from(&path));
                order.push(key.clone());
                projects.insert(
                    key,
                    ProjectInfo {
                        name,
                        path: path.into(),
                        process_status: ProcessStatus::Stopped,
                        port: None, // DB には port を持たない（動的割当）
                        enabled: true,
                    },
                );
            }
        } else {
            // DB 未接続: config.toml から読む（従来通り）
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
                        port: project.port,
                        enabled: project.enabled,
                    },
                );
            }
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

    /// config.toml を再読み込みして projects を更新（新規プロジェクトの動的追加対応）
    pub async fn reload_config(&self) {
        if let Ok(config) = Config::load() {
            let mut projects = self.projects.write().await;
            let mut order = self.project_order.write().await;
            for project in &config.projects {
                let key = normalize_path_key(&PathBuf::from(&project.path));
                if projects
                    .entry(key.clone())
                    .or_insert_with(|| ProjectInfo {
                        name: project.name.clone(),
                        path: project.path.clone().into(),
                        process_status: ProcessStatus::Stopped,
                        port: project.port,
                        enabled: project.enabled,
                    })
                    .name
                    == project.name
                {
                    // 新規追加の場合のみ order に追加
                    if !order.contains(&key) {
                        order.push(key);
                    }
                }
            }
            tracing::info!("Config reloaded: {} projects", projects.len());
        }
    }

    /// プロジェクトを追加（+ DB / config.toml に永続化）
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
        };

        let sort_order = {
            let mut projects = self.projects.write().await;
            if projects.contains_key(&key) {
                return Err(CapabilityError::Other(format!(
                    "Project already exists: {}",
                    path
                )));
            }
            projects.insert(key.clone(), info.clone());
            projects.len() as i64 - 1
        };
        // 順序リストに末尾追加
        self.project_order.write().await.push(key.clone());

        // DB に書き込み（正規化パスで保存）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db.upsert_project(name, &key, sort_order).await
        {
            tracing::warn!("DB project 追加失敗: {}", e);
        }

        // DB 未接続時は config.toml にフォールバック
        self.persist_to_config_fallback().await;

        Ok(info)
    }

    /// プロジェクトを削除（+ DB / config.toml に永続化）
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

        // DB から削除（正規化パスで削除）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db.delete_project(&key).await
        {
            tracing::warn!("DB project 削除失敗: {}", e);
        }

        // DB 未接続時は config.toml にフォールバック
        self.persist_to_config_fallback().await;

        Ok(())
    }

    /// プロジェクト名を変更（+ DB / config.toml に永続化）
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

        // DB を更新（正規化パスで更新）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db.update_project_name(&key, new_name).await
        {
            tracing::warn!("DB project 名前変更失敗: {}", e);
        }

        // DB 未接続時は config.toml にフォールバック
        self.persist_to_config_fallback().await;

        Ok(())
    }

    /// プロジェクトの enabled/disabled を切り替え（+ config.toml に永続化）
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

        self.persist_to_config_fallback().await;
        tracing::info!("Project enabled={}: {}", enabled, path);

        Ok(())
    }

    /// プロジェクトの並び順を更新（+ DB / config.toml に永続化）
    pub async fn reorder_projects(&self, paths: &[String]) -> CapabilityResult<()> {
        // raw paths を正規化して HashMap キーと一致させる
        let normalized: Vec<String> = paths
            .iter()
            .map(|p| normalize_path_key(&PathBuf::from(p)))
            .collect();
        // 順序リストを更新
        *self.project_order.write().await = normalized.clone();

        // DB を更新（正規化パスで更新）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db.reorder_projects(&normalized).await
        {
            tracing::warn!("DB project 並び替え失敗: {}", e);
        }

        // DB 未接続時は config.toml にフォールバック
        self.persist_to_config_fallback().await;

        Ok(())
    }

    /// DB が未接続の場合に config.toml に永続化するフォールバック（project_order の順序で書き出す）
    #[cfg(not(test))]
    async fn persist_to_config_fallback(&self) {
        if self.vpdb.is_some() {
            // DB 接続中は DB が source of truth なのでスキップ
            return;
        }

        let order = self.project_order.read().await.clone();
        let projects = self.projects.read().await;

        let mut config = Config::load().unwrap_or_default();
        config.projects = order
            .iter()
            .filter_map(|key| {
                projects.get(key).map(|info| {
                    // 既存 config の slot を継承 (port management Phase 1)
                    let slot = config
                        .projects
                        .iter()
                        .find(|p| p.name == info.name)
                        .and_then(|p| p.slot);
                    crate::config::ProjectConfig {
                        name: info.name.clone(),
                        path: info.path.to_string_lossy().to_string(),
                        port: info.port,
                        enabled: info.enabled,
                        slot,
                    }
                })
            })
            .collect();

        // order に含まれないプロジェクトも末尾に追加
        let order_set: std::collections::HashSet<&String> = order.iter().collect();
        for (key, info) in projects.iter() {
            if !order_set.contains(key) {
                let slot = config
                    .projects
                    .iter()
                    .find(|p| p.name == info.name)
                    .and_then(|p| p.slot);
                config.projects.push(crate::config::ProjectConfig {
                    name: info.name.clone(),
                    path: info.path.to_string_lossy().to_string(),
                    port: info.port,
                    enabled: info.enabled,
                    slot,
                });
            }
        }

        if let Err(e) = config.save() {
            tracing::error!("config.toml 永続化失敗: {}", e);
        } else {
            tracing::info!("config.toml 永続化完了: {} projects", config.projects.len());
        }
    }

    /// テスト環境では config.toml を書き換えない（データ破壊防止）
    #[cfg(test)]
    async fn persist_to_config_fallback(&self) {
        // no-op: テスト時は本番の config.toml に触れない
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

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Starting;
            }
        }

        // vp sp start を子プロセスとして実行
        let project_path_str = project.path.to_string_lossy();
        let mut cmd = Command::new(&vp_path);
        cmd.args(["sp", "start", "-C", &project_path_str]);
        cmd.current_dir(&project.path);

        // バックグラウンドで起動
        let child = cmd
            .spawn()
            .map_err(|e| CapabilityError::Other(format!("Failed to start vp: {}", e)))?;

        let pid = child.id().unwrap_or(0);

        // ポートが listen ready になるまで polling で待つ。
        //
        // 旧実装は固定 sleep(1500ms) + 1-shot scan で、 cold start (SurrealDB lock retry /
        // dyld load 等) で SP の `/api/health` が 1.5s 直後にちょうど ready になる場合に
        // refused/timeout を取りこぼして即 fail していた (PR #228 後の dogfood で
        // unison-kdl / object-records が連続失敗)。
        //
        // 新実装: 800ms 待 → scan → miss なら 500ms backoff で max 10s 再試行。
        // 起動が早い case (~1s) では旧 1500ms 固定より速く抜ける、 遅い case (~3-5s) でも
        // 確実に catch。 (I-a 対症 fix、 2026-04-30、 user 提案 (I-b) concurrency 化は別 sprint)。
        let port = self
            .wait_for_process_port(
                &project.path,
                std::time::Duration::from_millis(800),
                std::time::Duration::from_millis(500),
                std::time::Duration::from_secs(10),
            )
            .await
            .ok_or_else(|| {
                CapabilityError::Other(
                    "Failed to find Process port within 10s startup timeout".to_string(),
                )
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

    /// SP startup port が listen ready になるまで polling で待つ。
    ///
    /// `start_process` の補助。 旧固定 sleep(1500ms) + 1-shot scan が cold start で
    /// 取りこぼしていた問題への対症 fix ((I-a) 2026-04-30)。
    ///
    /// - `initial_delay`: 最初の scan までの待機。 SP が axum listen + `/api/health`
    ///   route 準備完了する最低時間 (典型 ~800ms)。
    /// - `poll_interval`: miss 時の retry 間隔 (典型 500ms)。
    /// - `total_timeout`: 諦めるまでの total 時間 (典型 10s)。 timeout 超で `None`。
    ///
    /// 計測 log: 解決時に `tracing::info!` で elapsed ms を出す。 dogfood で cold start
    /// 分布を観察して将来 (I-b) concurrency 化の N 値決定に使う想定。
    async fn wait_for_process_port(
        &self,
        project_path: &std::path::Path,
        initial_delay: std::time::Duration,
        poll_interval: std::time::Duration,
        total_timeout: std::time::Duration,
    ) -> Option<u16> {
        let start = std::time::Instant::now();
        tokio::time::sleep(initial_delay).await;

        loop {
            if let Some(port) = self.find_process_port(project_path).await {
                tracing::info!(
                    "SP startup port resolved in {}ms (project_path={})",
                    start.elapsed().as_millis(),
                    project_path.display()
                );
                return Some(port);
            }
            if start.elapsed() >= total_timeout {
                tracing::warn!(
                    "SP startup port resolution timeout after {}ms (project_path={})",
                    start.elapsed().as_millis(),
                    project_path.display()
                );
                return None;
            }
            tokio::time::sleep(poll_interval).await;
        }
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

    /// VP-129 MVP: ccws root を watch して worker dir 削除を SP DELETE に bridge する FSEvents watcher。
    ///
    /// **「folder = Lane 空間」 axiom の物理実装**。 user が Finder / `rm -rf` で worker dir を
    /// 削除した時、 OS の file system event (Mac → FSEvents、 Linux → inotify) → notify crate
    /// → 本 watcher が dirname を parse → project 解決 → SP `DELETE /api/lanes` 自動発火、
    /// sidebar / tmux / PtySlot が cascade で同期 cleanup される。
    ///
    /// D10 Reconciliation arch の **3rd path 拡張**: Push (QUIC heartbeat) + Pull (port scan) +
    /// **FSEvents (本 method)** の 3-trigger model 完成。
    ///
    /// MVP scope (= 別 ticket で safety net 追加候補):
    /// - self-loop 防止: scope 外 (= SP 経由削除も Remove event 発火、 二重 DELETE 走るが SP 側
    ///   404 で no-op、 log noise 許容)
    /// - spawn race: scope 外 (= 既存 spawn semaphore + atomic LanePool insert で吸収)
    /// - 詳細 EventKind 区別: Remove(_) 全 variant accept (= Mac FSEvents は RemoveKind 区別が薄い)
    pub async fn run_ccws_watcher(
        world: Arc<RwLock<Self>>,
        shutdown_token: tokio_util::sync::CancellationToken,
    ) {
        use notify::{EventKind, RecursiveMode, Watcher};

        // workers_dir 解決 (= ~/.local/share/ccws/)。 不在なら作成 (= 後の worker spawn でも必要)。
        let workers_dir = match crate::ccws::config::workers_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("ccws watcher: workers_dir 解決失敗 (skip): {}", e);
                return;
            }
        };
        if !workers_dir.exists()
            && let Err(e) = std::fs::create_dir_all(&workers_dir)
        {
            tracing::warn!(
                "ccws watcher: workers_dir create 失敗 (skip、 path={}): {}",
                workers_dir.display(),
                e
            );
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
                    tracing::warn!("ccws watcher: recommended_watcher 構築失敗 (skip): {}", e);
                    return;
                }
            };

        if let Err(e) = watcher.watch(&workers_dir, RecursiveMode::NonRecursive) {
            tracing::warn!(
                "ccws watcher: watch 開始失敗 (path={}, err={})",
                workers_dir.display(),
                e
            );
            return;
        }

        tracing::info!(
            "ccws watcher 起動 (path={}、 mode=NonRecursive、 trigger=Remove → SP DELETE)",
            workers_dir.display()
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest Client 構築失敗");

        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    tracing::info!("ccws watcher: shutdown signal、 停止");
                    break;
                }
                event_opt = rx.recv() => {
                    let Some(event) = event_opt else { break }; // channel closed
                    if !matches!(event.kind, EventKind::Remove(_)) {
                        continue;
                    }
                    Self::handle_ccws_remove_event(&world, &client, &workers_dir, &event).await;
                }
            }
        }

        drop(watcher); // 明示 drop で watching 停止 (scope 終端でも自動だが意図表示)
        tracing::info!("ccws watcher 終了");
    }

    /// VP-129 MVP: Remove event 1 件を処理。 dirname → project 解決 → SP DELETE call。
    /// `run_ccws_watcher` の inner、 各 path を独立処理。
    async fn handle_ccws_remove_event(
        world: &Arc<RwLock<Self>>,
        client: &reqwest::Client,
        workers_dir: &std::path::Path,
        event: &notify::Event,
    ) {
        for path in &event.paths {
            // workers_dir 直下の dir のみ対象 (= 子孫 file の Remove は無関係)
            let Some(parent) = path.parent() else {
                continue;
            };
            if parent != workers_dir {
                continue;
            }
            let Some(dirname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            // project 解決 = config.projects に対する longest prefix match
            // (例: "vantage-point-keystage" で project name が "vantage" / "vantage-point" の
            //  両方 match した場合、 longest = "vantage-point" を採用)
            let resolved = {
                let world_read = world.read().await;
                let Some(config) = world_read.config.as_ref() else {
                    continue;
                };
                let parent_proj = config
                    .projects
                    .iter()
                    .filter(|p| dirname.starts_with(&format!("{}-", p.name)))
                    .max_by_key(|p| p.name.len());
                match parent_proj {
                    Some(p) => Some((p.name.clone(), p.path.clone())),
                    None => {
                        tracing::debug!(
                            "ccws watcher: parent project 解決失敗 (skip) dirname={}",
                            dirname
                        );
                        None
                    }
                }
            };
            let Some((project_name, project_path)) = resolved else {
                continue;
            };
            let worker_name = match dirname.strip_prefix(&format!("{}-", project_name)) {
                Some(w) if !w.is_empty() => w.to_string(),
                _ => continue,
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
                    "ccws watcher: SP not running for project={} (skip) worker={}",
                    project_name,
                    worker_name
                );
                continue;
            };

            // SP DELETE /api/lanes (cleanup=false、 dir は既に gone)。 self-loop case
            // (= SP 経由で削除されて dir が消えた → watcher が Remove 検知 → 本 DELETE 発火)
            // は SP 側で 404 (Lane not found) 返却、 log debug 落ち。
            let address = format!("{}/worker/{}", project_name, worker_name);
            let address_enc = address.replace('/', "%2F");
            let url = format!(
                "http://[::1]:{}/api/lanes?address={}&cleanup=false",
                port, address_enc
            );
            tracing::info!(
                "ccws watcher: dir removed → SP DELETE 発火 (project={}, worker={}, port={})",
                project_name,
                worker_name,
                port
            );
            match client.delete(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    tracing::info!("ccws watcher: SP DELETE 成功 ({})", address);
                }
                Ok(r) => {
                    tracing::debug!(
                        "ccws watcher: SP DELETE non-success (likely self-loop or already deleted): status={}, address={}",
                        r.status(),
                        address
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "ccws watcher: SP DELETE 失敗 (port={}, address={}): {}",
                        port,
                        address,
                        e
                    );
                }
            }
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
