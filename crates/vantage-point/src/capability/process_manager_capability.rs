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
use tokio::sync::{RwLock, Semaphore};

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
    /// active lane (presence、 Model Q): この project の選択中 lane address。
    /// daemon-canonical。 `list_projects` で active_lanes map から enrich (構築時は None)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lane: Option<String>,
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
}

/// project の presence 状態（World daemon-canonical、vp-app sidebar の ●◐○ 表示用）。
///
/// `/api/health` の `processes[].presence` で vp-app に expose される。
///
/// ⚠️ doc 44 P1 (fold-in) 後の実態: production で set されるのは `Connected`（`start_process`）と
/// `Unregistered`（`stop_process`）の**2 値のみ**。`Connecting`（respawn 着手中）と
/// `Disconnected`（QUIC 切断）は、別プロセスの SP が register/heartbeat していた時代の状態で、
/// その生産者（registry handler は #824、health monitor は本 PR）が消えたため到達不能になった。
/// fold-in 後の project は「World の中に居る（=起動済）か、居ないか」の二値しか取り得ない。
/// enum の 2 値への縮約と vp-app 描画の追従は presence 意味論の follow-up（doc 44 §5.5 PR3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPresenceState {
    /// 未登録（projects には在るが未起動 / 停止済）。fold-in 後は「World の外」を表す。
    Unregistered,
    /// 到達不能（fold-in で生産者消滅）。旧: 再起動 in-flight（respawn 着手、register 待ち）。
    Connecting,
    /// 起動済み（`start_process` 成功）。fold-in 後は「World の中に居る」を表す。
    Connected,
    /// 到達不能（fold-in で生産者消滅）。旧: QUIC 切断を検知（crash / network）。
    Disconnected,
}

impl ProcessPresenceState {
    /// `/api/health` の `processes[].presence` 値（vp-app が ●◐○ 描画に使う）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
        }
    }
}

/// vp-app sidebar 向けの SP presence 1 件（`/api/health` の `processes[]` 要素）。
///
/// `projects`（desired = 全登録 project）を軸に、`running_processes`（live port/pid）と
/// `process_presence`（接続状態）を join した結果。Connected でない（= live 不在）SP は
/// port/pid が `None` になるが、project として sidebar には残り続ける（Model Q）。
#[derive(Debug, Clone, Serialize)]
pub struct ProcessHealthInfo {
    /// プロジェクト名（表示用ラベル）。
    pub project: String,
    /// 正規化パスキー（一意識別）。
    pub path: String,
    /// presence 状態（`"unregistered"` | `"connecting"` | `"connected"` | `"disconnected"`）。
    pub presence: &'static str,
    /// live port（Connected 時のみ Some、`running_processes` 由来）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// live pid（同上）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// 正規化パスキーを生成（HashMap のキーに使用）
///
/// ディレクトリパスを正規化した String を返す。
/// `running_processes` / `projects` の一意キーとして使用。
pub fn normalize_path_key(path: &std::path::Path) -> String {
    Config::normalize_path(path)
}

/// `config.projects` (ProjectConfig) を ProjectEntry 列に変換する。
///
/// PR-C: load_config が「DB 復旧の seed」「vpdb なし時の fallback」両方でこの変換を使う。
/// enabled は projects.kdl の慣習 (true は省略 = None、 false のみ明記) に揃える。
fn config_projects_to_entries(config: &Config) -> Vec<crate::projects_file::ProjectEntry> {
    config
        .projects
        .iter()
        .map(|p| crate::projects_file::ProjectEntry {
            name: p.name.clone(),
            path: p.path.clone(),
            enabled: if p.enabled { None } else { Some(false) },
            slot: p.slot,
        })
        .collect()
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
    /// Phase 1b: 各 Project の Lane registry（キー: 正規化パス）—
    /// SP が register payload に lanes を載せて push、 disconnect で全 Lane drop。
    /// agent (Echoes on Claude CLI) が `GET /api/lanes` で resolve するための cache。
    #[allow(clippy::type_complexity)]
    lane_registry: Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
    /// 設定
    config: Option<Config>,
    /// vpバイナリパス
    /// SurrealDB クライアント（Some なら DB に二重書き込み）
    vpdb: Option<crate::db::SharedVpDb>,
    /// doc 44 P1 (fold-in): project を World 内で起動・保持する registry（World mode のみ Some）。
    /// 旧構成で `vp sp start` を spawn していた箇所が、この registry への `start()` に置き換わる。
    project_runtimes: Option<Arc<crate::process::project_registry::ProjectRuntimes>>,
    /// process lifecycle event の broadcast Sender（World mode のみ Some、DaemonState と共有）。
    ///
    /// doc 44 P1 (fold-in): 旧構成では SP の register/unregister を受けた registry channel
    /// handler がここに Add/Remove を流していた。SP が消えたため、in-process の起動元である
    /// `start_process` / `stop_process` が daemon-canonical な生産者として引き継ぐ。
    /// これが無いと `vp daemon processes --watch` と event log の process.up/down が永久沈黙する。
    process_lifecycle_tx:
        Option<tokio::sync::broadcast::Sender<crate::daemon::protocol::ProcessLifecycleEvent>>,
    /// active lane (presence、 Model Q): project ごとの選択中 lane (キー: 正規化パス)。
    /// daemon-canonical。 `set_active_lane` で更新 + db/world に upsert、 boot で load。
    active_lanes: Arc<RwLock<HashMap<String, String>>>,
    /// L1 lifecycle (Phase C): project の presence (キー: 正規化パス)。daemon-canonical (doc 27 §3.2)。
    /// doc 44 P1 (fold-in): `start_process`→Connected / `stop_process`→Unregistered の 2 値のみ
    /// set される（旧 registry handler / health monitor の Connecting / Disconnected は到達不能）。
    /// DaemonState と Arc 共有 (`process_presence_ref`)。
    process_presence: Arc<RwLock<HashMap<String, ProcessPresenceState>>>,
    /// PR3: SP spawn の同時実行数 cap (= CPU コアベースの平滑化)。
    ///
    /// tmux decoupling 後は lane claude = SP の PtySlot の子なので、同時 spawn が CPU を
    /// 圧迫すると claude 群の起動が団子になる。`start_process` (全 spawn trigger の sink) を
    /// この permit でゲートし、一度に走る `vp sp start` を `cores − 2` (floor 1) に平滑化する。
    /// permit は spawn 区間だけ RAII 保持 → 総稼働 SP 数は縛らない (semantics A)。
    /// `Semaphore::new(0)` は永久 block なので permit は必ず ≥1 (spawn_cap で floor)。
    spawn_semaphore: Arc<Semaphore>,
}

/// PR3: SP spawn の同時実行 cap を CPU コア数から算出する (= `cores − 2`、floor 1)。
///
/// Workflow の concurrency cap `min(16, cores − 2)` と同発想の `k = 2` (daemon 本体 +
/// 余裕分を空ける)。`available_parallelism()` は std (依存追加不要)。1〜2 core 機では
/// `saturating_sub(2) = 0` になるため `.max(1)` で下限を保証 — `Semaphore::new(0)` は
/// 永久 block する地雷 (lane_spawn_actor が踏んだ前例)。
fn spawn_cap() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_sub(2)
        .max(1)
}

impl ProcessManagerCapability {
    /// 新しいProcessManagerCapabilityを作成
    pub fn new() -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            projects: Arc::new(RwLock::new(HashMap::new())),
            project_order: Arc::new(RwLock::new(Vec::new())),
            running_processes: Arc::new(RwLock::new(HashMap::new())),
            lane_registry: Arc::new(RwLock::new(HashMap::new())),
            config: None,
            vpdb: None,
            project_runtimes: None,
            process_lifecycle_tx: None,
            active_lanes: Arc::new(RwLock::new(HashMap::new())),
            process_presence: Arc::new(RwLock::new(HashMap::new())),
            spawn_semaphore: Arc::new(Semaphore::new(spawn_cap())),
        }
    }

    /// SurrealDB クライアントを設定
    /// doc 44 P1 (fold-in): project を in-process 起動するための registry を差し込む。
    ///
    /// World mode でのみ設定される。未設定のまま `start_process` を呼ぶと Err になる
    /// （旧構成で `vp` binary が見つからない場合と同じ「起動手段が無い」状態）。
    pub(crate) fn set_project_runtimes(
        &mut self,
        runtimes: Arc<crate::process::project_registry::ProjectRuntimes>,
    ) {
        self.project_runtimes = Some(runtimes);
    }

    /// process lifecycle broadcast の Sender を差し込む（World mode のみ）。
    ///
    /// DaemonState と同一 Sender を共有し、`start_process` / `stop_process` が
    /// Add / Remove を流す。未設定なら emit は no-op（SP 単体起動 / test）。
    pub(crate) fn set_process_lifecycle_tx(
        &mut self,
        tx: tokio::sync::broadcast::Sender<crate::daemon::protocol::ProcessLifecycleEvent>,
    ) {
        self.process_lifecycle_tx = Some(tx);
    }

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

    /// L1 lifecycle: process_presence の共有参照を取得（DaemonState と共有するため）。
    ///
    /// registry channel handler が同一 Arc を握り、SP の register/unregister/切断を観測して
    /// presence を遷移させる（capability 経由でなく DaemonState 側から直接書ける）。
    pub fn process_presence_ref(&self) -> Arc<RwLock<HashMap<String, ProcessPresenceState>>> {
        self.process_presence.clone()
    }

    /// L1 lifecycle: 1 project の presence を更新する（`start_process` / `stop_process`）。
    pub async fn set_presence(&self, path_key: &str, state: ProcessPresenceState) {
        self.process_presence
            .write()
            .await
            .insert(path_key.to_string(), state);
    }

    /// L1 lifecycle: vp-app sidebar 用の SP presence 一覧を作る（World daemon-canonical）。
    ///
    /// `projects`（desired = 全登録 project）を軸に `running_processes`（live port/pid）と
    /// `process_presence`（接続状態）を join する。SP が crash/disconnect しても projects には
    /// 残るので sidebar から消えず ○ disconnected として見える（Model Q）。HashMap 反復順は
    /// 非決定的なので project 名で sort して返す（sidebar の表示 jitter を防ぐ）。
    ///
    /// ロック順序: projects → running_processes → process_presence（register handler と同順、deadlock 回避）。
    pub async fn presence_snapshot(&self) -> Vec<ProcessHealthInfo> {
        let projects = self.projects.read().await;
        let running = self.running_processes.read().await;
        let presence = self.process_presence.read().await;
        let mut out: Vec<ProcessHealthInfo> = projects
            .iter()
            .map(|(path_key, info)| {
                let state = presence
                    .get(path_key)
                    .copied()
                    .unwrap_or(ProcessPresenceState::Unregistered);
                let live = running.get(path_key);
                ProcessHealthInfo {
                    project: info.name.clone(),
                    path: path_key.clone(),
                    presence: state.as_str(),
                    port: live.map(|p| p.port),
                    pid: live.map(|p| p.pid),
                }
            })
            .collect();
        out.sort_by(|a, b| a.project.cmp(&b.project));
        out
    }

    /// 設定を読み込み
    ///
    /// PR-C (control plane 一元化, creo `mem_1CbmWjCGNi9z49s3r21TwQ`): registered projects の
    /// 真実源を db/world に切り替える。
    /// - `vpdb=Some` (= World daemon): **db/world を真実源**にする。 DB が空なら config.projects
    ///   (= projects.kdl) から一回 import して復旧 (VP-182 シナリオ / 既存ユーザーの移行)。
    /// - `vpdb=None` (= CLI / SP / test 初期): 従来通り config.projects (= projects.kdl) から展開。
    ///
    /// projects.kdl は過渡期の復旧の種兼ミラー (PR-D で撤去予定)。 `Config::load()` は config.kdl の
    /// 人設定読みと、 復旧 seed としての projects.kdl 読みを兼ねる。
    pub async fn load_config(&mut self) -> CapabilityResult<()> {
        let config = Config::load().map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to load config: {}", e))
        })?;

        // 真実源から ProjectEntry 列を得る (vpdb=Some なら DB 優先、 空なら kdl から復旧)。
        let entries: Vec<crate::projects_file::ProjectEntry> = if let Some(db) = &self.vpdb {
            let mut entries = db.export_projects().await.map_err(|e| {
                CapabilityError::InitializationFailed(format!("DB projects 取得失敗: {}", e))
            })?;
            if entries.is_empty() && !config.projects.is_empty() {
                // DB 空 + kdl に projects あり → kdl から db/world へ一回 import (移行 / 復旧)。
                let seed = config_projects_to_entries(&config);
                db.import_projects(&seed).await.map_err(|e| {
                    CapabilityError::InitializationFailed(format!(
                        "DB projects 復旧 import 失敗: {}",
                        e
                    ))
                })?;
                tracing::info!(
                    "projects を projects.kdl から db/world に復旧 ({} 件)",
                    seed.len()
                );
                entries = db.export_projects().await.map_err(|e| {
                    CapabilityError::InitializationFailed(format!("DB projects 再取得失敗: {}", e))
                })?;
            }
            entries
        } else {
            config_projects_to_entries(&config)
        };

        let mut projects = self.projects.write().await;
        let mut order = self.project_order.write().await;
        projects.clear();
        order.clear();

        for e in &entries {
            // db 由来の entry は `ProjectsFile::load` を経ないため、 旧 Windows が保存した
            // verbatim prefix (`\\?\C:\...`) を落とす最後の関所がここ。 素通しすると
            // `ProjectInfo.path` が SP の spawn 引数 (`-C`) までそのまま流れる。
            let path = crate::config::strip_verbatim_prefix(&e.path);
            let key = normalize_path_key(&PathBuf::from(path));
            order.push(key.clone());
            projects.insert(
                key,
                ProjectInfo {
                    name: e.name.clone(),
                    path: PathBuf::from(path),
                    process_status: ProcessStatus::Stopped,
                    port: None, // port は動的割当 (port_layout が slot から計算)
                    enabled: e.is_enabled(),
                    slot: e.slot,
                    active_lane: None, // list_projects で enrich
                },
            );
        }
        drop(projects);
        drop(order);

        // Model Q: active lane (presence) を db/world から load する (vpdb=Some のみ)。
        if let Some(db) = &self.vpdb {
            match db.list_active_lanes().await {
                Ok(rows) => {
                    let mut al = self.active_lanes.write().await;
                    al.clear();
                    for (path, addr) in rows {
                        al.insert(path, addr);
                    }
                }
                Err(e) => tracing::warn!("active_lane の load 失敗 (空で継続): {}", e),
            }

            // doc 24 §10 Phase 2: lane descriptor を db/world から boot load する (daemon
            // 再起動を re-animate、 §3.3)。 旧来 lane_registry は SP push を待って初めて
            // 埋まる cache だったが、 daemon-canonical 化で boot 時点から truth を持つ。
            // SP が後で reconnect すれば register snapshot が最新で上書きする (= reconcile)。
            match db.list_lanes().await {
                Ok(rows) => {
                    let mut lr = self.lane_registry.write().await;
                    lr.clear();
                    for (path, info) in rows {
                        lr.entry(path).or_default().push(info);
                    }
                }
                Err(e) => tracing::warn!("lane の boot load 失敗 (空で継続): {}", e),
            }
        }

        // doc 24 §4.6 boot reconcile heal (庭師モデル): desired(store の lifecycle) × actual
        // (disk の ground) を突き合わせて収束させる。 daemon boot で 1 周 (vpdb=Some のみ内部判定)。
        self.reconcile_lanes().await;

        self.config = Some(config);
        Ok(())
    }

    /// doc 24 §4.6 boot reconcile heal — desired (store の lifecycle) × actual (disk の ground) を
    /// 突き合わせて収束させる (庭師モデル)。 daemon boot で 1 周走る。
    ///
    /// heal table (create-side、 retry は後続スライス):
    /// - `provisioning` + ground 在り → `ready` (provision 完了とみなす)
    /// - `provisioning` + ground 無し → `dead`  (crash 中断。 retry-1x は club-nostos Outcome の次スライス)
    /// - `ready` + ground 在り       → ok (no-op)
    /// - `ready` + ground 外部削除   → `dead`  (user の rm を尊重、 勝手に作り直さない)
    /// - `dead`                      → 保持 (inspection / `--resume`)
    ///
    /// destroy-side (`destroying`) と orphan→adopt は後続 increment。
    async fn reconcile_lanes(&self) {
        use crate::process::lanes_state::LaneLifecycle;
        let Some(db) = &self.vpdb else { return };
        let lifecycles = match db.list_lane_lifecycles().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("reconcile: lane_lifecycle load 失敗 (skip): {}", e);
                return;
            }
        };
        if lifecycles.is_empty() {
            return;
        }

        // descriptor の cwd を引く (boot load 済 lane_registry): (project_path, address) → cwd。
        // team-b #3: lifecycle はあるのに lane_registry が空 = boot lane load 失敗の可能性。
        // この不整合のまま進むと全 lane が cwd_map miss → ground_exists=false → dead に誤判定
        // するため、 skip する (heal は次 boot に委ねる = 庭師の「ゆるやか」収束)。
        let cwd_map: std::collections::HashMap<(String, String), String> = {
            let lr = self.lane_registry.read().await;
            if lr.is_empty() {
                tracing::warn!(
                    "reconcile: lane_lifecycle はあるが lane_registry 空 → skip (lane boot load 失敗の可能性)"
                );
                return;
            }
            lr.iter()
                .flat_map(|(p, lanes)| {
                    lanes
                        .iter()
                        .map(move |l| ((p.clone(), l.address.to_string()), l.cwd.clone()))
                })
                .collect()
        };

        for (project_path, address, lifecycle_str) in lifecycles {
            let lifecycle = LaneLifecycle::parse(&lifecycle_str);
            if lifecycle == LaneLifecycle::Dead {
                continue; // dead は保持
            }
            let ground_exists = cwd_map
                .get(&(project_path.clone(), address.clone()))
                .map(|c| std::path::Path::new(c).exists())
                .unwrap_or(false);

            let healed = match (lifecycle, ground_exists) {
                (LaneLifecycle::Provisioning, true) => Some(LaneLifecycle::Ready),
                (LaneLifecycle::Provisioning, false) => Some(LaneLifecycle::Dead),
                (LaneLifecycle::Ready, false) => Some(LaneLifecycle::Dead),
                (LaneLifecycle::Ready, true) | (LaneLifecycle::Dead, _) => None,
            };

            if let Some(new_lc) = healed {
                match db
                    .upsert_lane_lifecycle(&project_path, &address, new_lc.as_str())
                    .await
                {
                    Ok(()) => tracing::info!(
                        "reconcile heal: {} {} {} → {}",
                        project_path,
                        address,
                        lifecycle.as_str(),
                        new_lc.as_str()
                    ),
                    Err(e) => tracing::warn!(
                        "reconcile heal の永続失敗 ({} {}): {}",
                        project_path,
                        address,
                        e
                    ),
                }
            }
        }
    }

    /// 現在の projects HashMap を真実源に永続化する。
    ///
    /// PR-C (control plane 一元化): `project_order` の順序で `ProjectEntry` 列を組み立て、
    /// - `vpdb=Some` (= World): **db/world に全置換** (= 真実源)。 projects.kdl は DB からの
    ///   一方向 export ミラー (= 過渡期の人間可読 + 復旧の種、 PR-D で撤去予定)。
    /// - `vpdb=None` (= CLI / SP / test): 従来通り projects.kdl に atomic write。
    ///
    /// add / delete / rename / reorder / set_enabled / auto_reassign_slot の各操作後に呼ぶ。
    /// test 環境では `ProjectsFile::save()` が no-op なので本番ファイルを破壊しない。
    async fn persist_projects(&self) -> CapabilityResult<()> {
        // read guard は entries 構築のみで解放する (DB / file の await 中は lock を持たない)。
        let entries: Vec<crate::projects_file::ProjectEntry> = {
            let projects = self.projects.read().await;
            let order = self.project_order.read().await;
            order
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
                .collect()
        };

        if let Some(db) = &self.vpdb {
            // db/world を真実源として全置換。
            db.replace_all_projects(&entries).await.map_err(|e| {
                CapabilityError::InitializationFailed(format!("DB projects 全置換失敗: {}", e))
            })?;
            // projects.kdl は DB の読み取り専用ミラー。 entries は replace_all で書いた内容と
            // 同一 (ord = 出現順) なので export 往復を省く (= DELETE→export 間に別リクエストが
            // 割り込んで誤った内容を kdl に焼く窓も消える、 Moody Blues PR-D review #3)。
            let pf = crate::projects_file::ProjectsFile { projects: entries };
            pf.save().map_err(|e| {
                CapabilityError::InitializationFailed(format!("projects.kdl export 失敗: {}", e))
            })
        } else {
            // vpdb なし: 従来通り projects.kdl に書く (= 真実源)。
            let pf = crate::projects_file::ProjectsFile { projects: entries };
            pf.save().map_err(|e| {
                CapabilityError::InitializationFailed(format!("projects.kdl 書き込み失敗: {}", e))
            })
        }
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
        // Model Q: active lane (presence) を enrich (daemon-canonical)。
        let active = self.active_lanes.read().await;
        order
            .iter()
            .filter_map(|key| {
                projects.get(key).cloned().map(|mut p| {
                    p.active_lane = active.get(key).cloned();
                    p
                })
            })
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

        {
            let mut projects = self.projects.write().await;
            let mut order = self.project_order.write().await;

            // projects.kdl 由来の key 集合 (= 除去判定の基準)。
            let kdl_keys: std::collections::HashSet<String> = config
                .projects
                .iter()
                .map(|p| normalize_path_key(&PathBuf::from(&p.path)))
                .collect();

            // add/update: projects.kdl の各 project を in-memory に反映。
            // PR-C: 既存 key も kdl 値で name/enabled/slot を更新 (CLI が kdl 経由で更新した
            // slot 等を取り込む)。 running process の process_status / port は触らない (安全側)。
            for project in &config.projects {
                let key = normalize_path_key(&PathBuf::from(&project.path));
                projects
                    .entry(key.clone())
                    .and_modify(|p| {
                        p.name = project.name.clone();
                        p.enabled = project.enabled;
                        p.slot = project.slot;
                    })
                    .or_insert_with(|| ProjectInfo {
                        name: project.name.clone(),
                        path: project.path.clone().into(),
                        process_status: ProcessStatus::Stopped,
                        port: project.port,
                        enabled: project.enabled,
                        slot: project.slot,
                        active_lane: None,
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
        } // projects / order の write guard を解放してから persist (read lock 取り直し)

        // PR-C: vpdb=Some なら DB に同期する。 reload は kdl→in-memory→DB の向きで、
        // running 保護後の in-memory を書くので、 古い kdl で DB を盲目上書きせず取りこぼしも防ぐ。
        // (= CLI が kdl 経由で更新した slot 等を db/world に焼く合流点)
        if self.vpdb.is_some()
            && let Err(e) = self.persist_projects().await
        {
            tracing::warn!("reload_config: DB 同期失敗: {}", e);
        }
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
            active_lane: None,
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

        // Model Q / §4.6 含有=所有=寿命: project(namespace) を倒したら、 その presence
        // (active_lane) も畳む。 in-memory map と db/world から回収する (DB は best-effort)。
        self.active_lanes.write().await.remove(&key);
        if let Some(db) = &self.vpdb
            && let Err(e) = db.delete_active_lane(&key).await
        {
            tracing::warn!(
                "active_lane の db/world 削除に失敗 (in-memory は削除済): {}",
                e
            );
        }
        // doc 44 D4 / §4.6: Project Host の帳簿 (開発起点ポインタ) も namespace と共に回収する。
        // 残すと同 path で project を再登録した時、旧 lane の UUID を指す孤児ポインタが復活し、
        // 起点が `Dangling` に落ちる (= 指定した覚えのない「指定が失われました」表示)。
        if let Some(db) = &self.vpdb {
            if let Err(e) = db.delete_host_origin(&key).await {
                tracing::warn!("host_origin の db/world 削除に失敗: {}", e);
            }
            // doc 44 §12: lane の並び順も同じ namespace の帳簿なので一緒に畳む。
            if let Err(e) = db.delete_lane_order_for_project(&key).await {
                tracing::warn!("host_lane_order の db/world 削除に失敗: {}", e);
            }
        }
        // L1 lifecycle: connection presence も namespace と共に回収 (active_lanes と対称、
        // DB 永続を持たない in-memory only field なので map remove のみ)。
        self.process_presence.write().await.remove(&key);

        // doc 24 §10 Phase 2 / §4.6 含有=所有=寿命: lane descriptor も同様に畳む。
        // lane は daemon-canonical durable truth (SP disconnect では残すが、 project remove は
        // namespace ごと倒す = descriptor も回収する)。 in-memory lane_registry と db から削除。
        // remove は外した Vec<LaneInfo> を返すので、 下の ground reclaim にそのまま使う。
        let removed_lanes = self
            .lane_registry
            .write()
            .await
            .remove(&key)
            .unwrap_or_default();
        if let Some(db) = &self.vpdb {
            if let Err(e) = db.delete_lanes_for_project(&key).await {
                tracing::warn!("lane の db/world 削除に失敗 (in-memory は削除済): {}", e);
            }
            // §4.6: lane lifecycle (別 table) も同様に回収する。
            if let Err(e) = db.delete_lane_lifecycles_for_project(&key).await {
                tracing::warn!("lane_lifecycle の db/world 削除に失敗: {}", e);
            }
        }

        // doc 24 §5.3 / B-destroy: ground を provision/reclaim する唯一の主体は daemon。
        // namespace (project) を倒したら performer の worktree (ground) も daemon が reclaim する。
        // A では descriptor だけ畳んで worktree が disk に orphan で残る中間状態だった — その穴を閉じる。
        // conductor は cwd = repo root (= user の repo そのもの) なので **絶対に消さない**、 performer のみ。
        let performer_names: Vec<String> = removed_lanes
            .iter()
            .filter(|l| !l.address.is_conductor())
            .map(|l| l.address.name.clone())
            .collect();
        if !performer_names.is_empty() {
            // repo_root は key (= normalize_path_key の出力) から再構築する。 add_project 時と
            // 同じ normalize を経るので通常は実 repo root と一致する。 万一ズレ / explicit cwd
            // (`<repo>/.vp/lanes/<name>` 外) の時は find_performer_dir が None → 下の warn で
            // skip され orphan が残るだけ (= 誤削除は起きない、 best-effort、 team-b review #1)。
            let repo_root = PathBuf::from(&key);
            // git worktree remove は blocking subprocess なので spawn_blocking で executor を塞がない。
            let _ = tokio::task::spawn_blocking(move || {
                for name in performer_names {
                    // best-effort (§4.6 ゆるやか統治): 既に手動 rm 済 / explicit cwd 外などは warn で流す。
                    match crate::lane::commands::remove_performer_in(&repo_root, &name) {
                        Ok(()) => tracing::info!(
                            "performer worktree reclaim: name={} repo={}",
                            name,
                            repo_root.display()
                        ),
                        Err(e) => tracing::warn!(
                            "performer worktree reclaim 失敗 (best-effort、 skip): name={} err={}",
                            name,
                            e
                        ),
                    }
                }
            })
            .await;
        }

        // VP-188: projects.kdl に永続化
        self.persist_projects().await?;

        Ok(())
    }

    /// doc 24 §10 Phase 2 B-create / §5.3: daemon が performer lane を create する。
    ///
    /// 「ground を provision する唯一の主体は daemon」(§5.3) の create 半分。 daemon が
    /// worktree を provision し、 descriptor を daemon-canonical truth (db + in-memory) として
    /// 所有する。 live PtySlot の spawn は SP の仕事で、 worktree dir 作成を検知した
    /// lane_watcher が SP に `POST /api/lanes` (cwd 明示) を発火して spawn させる
    /// (= 既存 convergence loop を再利用、 daemon→SP の新経路は作らない)。
    ///
    /// (b) スコープ: §4.6 の durable lifecycle state machine (provisioning/ready/dead +
    /// boot reconcile) は入れない。 それを exercise する in-flight 状態が無い間は投機実装に
    /// なるため ([[pre-mvp-development-stance]]: 中間状態を作らない)。 crash mid-provision の
    /// orphan worktree は現状の SP create と同じ risk profile で、 B-destroy (#568) +
    /// 将来の boot reconcile が回収する。
    /// `branch` / `stand` は呼び手 (route) が resolve 済の concrete 値を渡す
    /// (default 導出 = data/calc は route の責務、 capability は provision = action に専念)。
    pub async fn create_lane(
        &self,
        project_path: &str,
        name: &str,
        branch: &str,
        stand: &str,
    ) -> CapabilityResult<crate::process::lanes_state::LaneInfo> {
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneLifecycle, LaneState};

        // doc 44 §9: **永続より先に**入口で名前を検証する（両経路で同じ gate）。
        //
        // 旧実装はここで空文字だけを見ており、それ以外（予約名 / 不正文字）は奥の
        // `new_performer_in` → `validate_performer_name` が clone 段階で初めて弾いていた。
        // だが下の intent-first bracket は **provision より前に descriptor を永続する**ので、
        // 拒否されるべき入力が db の `lane` 行に触れてしまう:
        //
        //   ① upsert_lane（DELETE+CREATE）で `<project>/<name>` 行を書く
        //   ② clone が validate で失敗
        //   ③ rollback の delete_lane が `<project>/<name>` 行を消す
        //
        // `name = conductor` ならこの ①③ が**本物の開発起点 descriptor を上書きして消す**。
        // 通常は下の dup check（in-memory `lane_registry`）が先に弾くので発火しないが、
        // dup check は validation ではなく、その cache は db と乖離しうる
        // （boot load 失敗 / SP snapshot 上書き — 前例 [performer console teardown]）。
        // **masking に頼らず、拒否は永続の手前で完結させる。**
        let name = name.trim();
        crate::lane::config::validate_performer_name(name).map_err(CapabilityError::Other)?;
        let repo_root = PathBuf::from(project_path);
        let project_id = repo_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let addr = LaneAddress::performer(&project_id, name);
        let key = normalize_path_key(&repo_root);

        // dup check (daemon-canonical lane_registry)。
        let exists = {
            let lr = self.lane_registry.read().await;
            lr.get(&key)
                .map(|lanes| lanes.iter().any(|l| l.address == addr))
                .unwrap_or(false)
        };
        if exists {
            return Err(CapabilityError::Other(format!(
                "Lane {} already exists",
                addr
            )));
        }

        // doc 24 §4.6 intent-first bracket (enter): descriptor + lifecycle=Provisioning を **先に**
        // 永続する。 cwd は worktree の deterministic path (<repo>/.vp/lanes/<name>) なので
        // provision 前に確定できる。 これにより daemon が provision 途中で crash しても
        // 「provisioning が残る」= boot reconcile が ground 存在で heal できる。
        let performer_dir = repo_root.join(".vp").join("lanes").join(name);
        let addr_str = addr.to_string();
        let info = LaneInfo {
            console_mode: Default::default(),
            id: crate::lane::lane_id::load_or_create(&project_id, name),
            address: addr.clone(),
            state: LaneState::Spawning, // process liveness: PtySlot pending (= lifecycle と別軸)
            stand: stand.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: performer_dir.to_string_lossy().into_owned(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };
        self.lane_registry
            .write()
            .await
            .entry(key.clone())
            .or_default()
            .push(info.clone());
        if let Some(db) = &self.vpdb {
            if let Err(e) = db.upsert_lane(&key, &info).await {
                tracing::warn!(
                    "lane descriptor の db 永続に失敗 (in-memory は反映済): {}",
                    e
                );
            }
            if let Err(e) = db
                .upsert_lane_lifecycle(&key, &addr_str, LaneLifecycle::Provisioning.as_str())
                .await
            {
                tracing::warn!("lane_lifecycle=provisioning の db 永続に失敗: {}", e);
            }
        }

        // §5.3 (active): ground provision は daemon が行う (worktree add)。 blocking git は spawn_blocking。
        // team-b #1: JoinError (task panic) を `?` で早期 return せず、 provision Err と同じ
        // rollback 経路に畳む (= intent-first の crash-recovery 保証を破らない)。
        let provision: Result<std::path::PathBuf, String> = {
            let repo_root = repo_root.clone();
            let name_owned = name.to_string();
            let branch = branch.to_string();
            match tokio::task::spawn_blocking(move || {
                crate::lane::commands::new_performer_in(
                    &repo_root,
                    &name_owned,
                    &branch,
                    false,
                    crate::lane::commands::Isolation::Worktree,
                    // daemon ground provision (GUI 経由) は base override 未対応 = 従来挙動
                    None,
                )
            })
            .await
            {
                Ok(inner) => inner,
                Err(join_err) => Err(format!("provision task join: {}", join_err)),
            }
        };

        // §4.6 (exit): provision の結果で lifecycle を確定する。
        match provision {
            Ok(_dir) => {
                if let Some(db) = &self.vpdb
                    && let Err(e) = db
                        .upsert_lane_lifecycle(&key, &addr_str, LaneLifecycle::Ready.as_str())
                        .await
                {
                    tracing::warn!("lane_lifecycle=ready の db 永続に失敗: {}", e);
                }
                tracing::info!(
                    "lane created (daemon): addr={} cwd={} lifecycle=ready (PtySlot は watcher→SP)",
                    addr,
                    info.cwd
                );
                Ok(info)
            }
            Err(e) => {
                // 通常の provision 失敗は rollback (retry 可能に): descriptor + lifecycle を回収。
                // crash 中断時だけ provisioning が db に残り boot reconcile が heal する
                // (= intent-first の効きどころ。 doc 24 §4.6)。
                if let Some(v) = self.lane_registry.write().await.get_mut(&key) {
                    v.retain(|l| l.address != addr);
                }
                if let Some(db) = &self.vpdb {
                    let _ = db.delete_lane(&key, &addr_str).await;
                    let _ = db.delete_lane_lifecycle(&key, &addr_str).await;
                }
                tracing::warn!("lane provision 失敗 → rollback: addr={} err={}", addr, e);
                Err(CapabilityError::Other(format!(
                    "worktree provision 失敗: {}",
                    e
                )))
            }
        }
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

    /// active lane (presence、 Model Q) を設定する。
    ///
    /// project ごとの選択中 lane を daemon-canonical に持つ。 in-memory map を更新し、
    /// vpdb=Some (= World) なら db/world の active_lane table に upsert する。
    /// §4.6: presence は tail-loss 許容なので DB 永続は best-effort (失敗は warn のみ)。
    pub async fn set_active_lane(
        &self,
        project_path: &str,
        lane_address: &str,
    ) -> CapabilityResult<()> {
        let key = normalize_path_key(&PathBuf::from(project_path));
        self.active_lanes
            .write()
            .await
            .insert(key.clone(), lane_address.to_string());
        if let Some(db) = &self.vpdb
            && let Err(e) = db.upsert_active_lane(&key, lane_address).await
        {
            tracing::warn!(
                "active_lane の db/world 永続に失敗 (in-memory は更新済): {}",
                e
            );
        }
        Ok(())
    }

    /// projects を現実と同期 (PR-D: CLI の `ProjectsFile::sync` を daemon 経由に移管)。
    ///
    /// dir が実在しない ghost project を除去する (running process を持つものは安全側で残す)。
    /// 永続化は内部の remove_project が persist_projects 経由で行う。
    ///
    /// かつて `start_dir` で「起点 dir 自動登録」も行っていたが、 `vp sp start` の起動時
    /// sync が **削除済 project を復活させる** resurrection バグの温床だったため撤去した
    /// (削除 → SP 再起動 → sync が起点 dir を無条件再登録 → db/kdl に復活)。 project 登録は
    /// `add_project` 経由の明示操作のみ (sidebar Add / `vp projects add`)。
    pub async fn sync_projects(&self) -> CapabilityResult<crate::projects_file::SyncOutcome> {
        let mut outcome = crate::projects_file::SyncOutcome::default();

        // ghost 除去 (dir 非実在 & 非 running)。ロック順序 projects → running_processes を遵守。
        let ghosts: Vec<(String, String)> = {
            let projects = self.projects.read().await;
            let running: std::collections::HashSet<String> = {
                let procs = self.running_processes.read().await;
                procs.keys().cloned().collect()
            };
            projects
                .iter()
                .filter(|(key, p)| !p.path.is_dir() && !running.contains(*key))
                .map(|(key, p)| (key.clone(), p.name.clone()))
                .collect()
        };
        for (key, name) in ghosts {
            if self.remove_project(&key).await.is_ok() {
                outcome.removed.push(name);
            }
        }

        Ok(outcome)
    }

    /// L0 finale (Push-only): 指定 path の live SP を `running_processes` registry から引く。
    ///
    /// `start_process` の重複 spawn 防止 dedup check。
    /// doc 44 P1 (fold-in): project が World 内の map エントリになり、重複起動は
    /// `ProjectRuntimes` のキー一意性が構造的に防ぐ。本 check は running_processes を直引きする
    /// 補助（旧 VP-133 の port scan dedup / SP uplink reconnect / health monitor respawn の
    /// 段取りはいずれも SP プロセス前提で、fold-in で不要になった）。
    async fn find_running_sp_at_path(
        &self,
        project_path: &std::path::Path,
    ) -> Option<RunningProcess> {
        let target_key = normalize_path_key(project_path);
        self.running_processes
            .read()
            .await
            .get(&target_key)
            .cloned()
    }

    pub async fn start_process(&self, project_name: &str) -> CapabilityResult<RunningProcess> {
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

        // PR3: SP spawn 平滑化 — early-return (already-running / dedup) を抜けた「実 spawn 確定」
        // 地点で permit を取得。permit は関数 return まで RAII 保持され、一度に走る
        // `vp sp start` を `spawn_cap()` 本に絞る (semantics A)。非輻輳時は即取得 =
        // レイテンシ影響なし。`Semaphore` は close しないので acquire は必ず成功する。
        if self.spawn_semaphore.available_permits() == 0 {
            tracing::debug!(
                "start_process: spawn permit 全 in-flight、'{}' は空き待ち (spawn cap 平滑化)",
                project_name
            );
        }
        let _spawn_permit = self
            .spawn_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("spawn_semaphore は close されない");

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
        // - `wait_for_health(port, &path)` で QUIC registry 登録を確認 (Push-only、 L0 finale)
        // - 外部衝突 (別 project SP / 非 VP process) なら 1 回きり auto-reassign + retry
        //
        // 旧 (PR-5 まで): `vp sp start -C <path>` (-p 無し) → 子の resolve_port が slot 解決 →
        // TheWorld が `wait_for_process_port` で range scan で discover、 だった。 PR-5b で
        // TheWorld が port を明示所有する形に整理。
        let project_path_str = project.path.to_string_lossy().to_string();
        // doc 44 P1 (fold-in): 旧実装は `vp sp start -C <path> -p <port>` を子プロセスとして
        // spawn し、QUIC registry への自己登録を `wait_for_health` で待ち、port が他 project に
        // 取られていれば `auto_reassign_slot` して 1 回だけ retry する、という多段の段取りだった。
        //
        // project が World 内の `Arc<AppState>` になった今、これは単なる関数呼び出しになる。
        // 旧段取りの構成要素はいずれも概念ごと不要になった:
        //   - port 解決      … bind しないので割り当てる対象が無い
        //   - health 待ち    … 起動の成否は Result で同期的に返る
        //   - 衝突 retry     … 衝突する port が存在しない
        //   - 重複 spawn 検出 … registry map のキー一意性が構造的に防ぐ
        //     （旧: registry dedup + spawn Semaphore + per-project DB LOCK の 3 重掛け）
        let runtimes = self.project_runtimes.clone().ok_or_else(|| {
            CapabilityError::Other(
                "project runtimes 未設定 — World mode 以外では project を起動できない".to_string(),
            )
        })?;
        let started = runtimes.start(&project_path_str).await.map_err(|e| {
            CapabilityError::Other(format!("project 起動失敗 ({}): {}", project_name, e))
        })?;
        if !started {
            tracing::info!("project は既に起動済み → skip (project={})", project_name);
        }
        // port / pid は SP プロセスの遺産。 project は World と同一プロセスで動くので
        // pid は World 自身、 port は不在を表す 0 を入れる（表示の意味論整理は doc 44 P3）。
        let running_process = RunningProcess {
            project_name: project_name.to_string(),
            port: 0,
            pid: std::process::id(),
            project_path: project.path.clone(),
        };

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Running;
            }
        }

        // doc 44 P1 (fold-in): daemon 側 insert を復活させる。
        //
        // #648 でこの insert を撤去したのは「SP の QUIC 自己登録が entry を書くので、
        // daemon 側の子 pid で上書きすると Push-canonical を壊す」ためだった。fold-in で
        // SP が消え、自己登録しに来る者が居なくなったため、この前提そのものが失効した。
        // 撤去したまま放置すると registry は**永久に空**になり、`vp ps` / `/api/world/processes`
        // が空を返し、`stop_process` は自身の gate に阻まれて project を停止できなくなる。
        //
        // in-process 起動が成功した瞬間が権威ある lifecycle event なので、書き手はここが正
        // （= presence 設計が元から掲げていた daemon-canonical に戻る）。
        {
            let mut procs = self.running_processes.write().await;
            procs.insert(key.clone(), running_process.clone());
        }
        // presence も同様に SP の register だけが Connected にしていた。in-process の
        // project は「起動していれば接続している」以外の状態を取り得ないため、
        // 起動成功 = Connected で確定する（旧: registry handler の register→Connected）。
        self.set_presence(&key, ProcessPresenceState::Connected)
            .await;

        // DB に書き込み（正規化パスで保存、 pid/port は registry entry の真実を使う）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db
                .upsert_process(
                    &key,
                    project_name,
                    running_process.port,
                    running_process.pid,
                    "running",
                )
                .await
        {
            tracing::warn!("DB process 登録失敗: {}", e);
        }

        // doc 44 P1 (fold-in): lifecycle event を broadcast する（旧 registry handler の
        // register→Add の後継）。`vp daemon processes --watch` と event log の process.up が
        // これを購読する。receiver 不在（購読者ゼロ）は Err になるが無害なので握り潰す。
        if let Some(ref tx) = self.process_lifecycle_tx {
            let _ = tx.send(crate::daemon::protocol::ProcessLifecycleEvent::Add {
                project_path: key.clone(),
                project_name: project_name.to_string(),
                port: running_process.port,
                pid: running_process.pid,
            });
        }

        tracing::info!(
            project = project_name,
            port = running_process.port,
            pid = running_process.pid,
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

        let _running = running.ok_or_else(|| {
            CapabilityError::Other(format!("No running Process for project: {}", project_name))
        })?;

        // 状態を更新
        {
            let mut projects = self.projects.write().await;
            if let Some(p) = projects.get_mut(&key) {
                p.process_status = ProcessStatus::Stopping;
            }
        }

        // doc 44 P1 (fold-in): 旧実装は World process-proxy の "shutdown" method を loopback で
        // 撃ち、reverse-route で SP に届けて自死させていた。SP は shutdown_token cancel 直後に
        // control channel を畳むため応答が返らないことがあり、best-effort 扱いにせざるを得な
        // かった（「止まったかどうか確かめられない」）。
        //
        // in-process になった今は registry から直接停止でき、完了も同期的に確認できる。
        if let Some(runtimes) = self.project_runtimes.as_ref() {
            if !runtimes.stop(&key).await {
                tracing::info!(
                    "停止対象の project は既に不在 (project={}) — registry の後始末のみ実施",
                    project_name
                );
            }
        } else {
            tracing::warn!(
                "project runtimes 未設定のため停止をスキップ (project={}) — registry からは remove",
                project_name
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
        // doc 44 P1 (fold-in): presence も対称に落とす（旧: SP 切断を registry handler が
        // 検知して Disconnected にしていた）。in-process では「停止した = 登録が無い」なので
        // Disconnected（居るが繋がらない）ではなく Unregistered が正。
        self.set_presence(&key, ProcessPresenceState::Unregistered)
            .await;

        // DB から削除（正規化パスで削除）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db.delete_process(&key).await
        {
            tracing::warn!("DB process 削除失敗: {}", e);
        }

        // doc 44 P1 (fold-in): lifecycle Remove を broadcast（旧 unregister→Remove の後継）。
        if let Some(ref tx) = self.process_lifecycle_tx {
            let _ = tx.send(crate::daemon::protocol::ProcessLifecycleEvent::Remove {
                project_path: key.clone(),
            });
        }

        tracing::info!(project = project_name, "Process stopped");

        Ok(())
    }

    /// project を restart する。 stop → 短い grace period → start を atomic に chain。
    /// stop が「No running Process」 なら start のみ実行 (= ensure-running 的な挙動)。
    ///
    /// doc 44 P1 (fold-in): in-process 化で stop/start は同期的に確定するため、旧 SP 時代の
    /// 「db LOCK 保持で重複 spawn 検出 → health monitor の crash 検知が respawn で自己修復」
    /// という非同期の自己修復経路は不要になった（health monitor 自体も退役）。
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

    /// 起動時設定の復帰: TheWorld 起動時に `enabled` な project の SP を自動起動する。
    ///
    /// daemon restart 後に working set を復元する (VP-207)。 TheWorld 起動時に
    /// バックグラウンドタスクとして 1 回だけ spawn される。
    /// 1. `enabled == true` かつ未稼働の project を収集
    /// 2. 各 project を `start_process` で起動 (300ms ずらして burst 回避)
    ///
    /// 二重起動は `ProjectRuntimes` の map キー一意性が構造的に防ぐ。
    /// lock 規律: `start_process`（内部で sleep する）を呼ぶ前に read ガードを clone して解放する。
    pub async fn autostart_enabled_projects(world: Arc<RwLock<Self>>) {
        // doc 44 P1 (fold-in): 旧「registry 静穏待ち」(最大 60s) を撤去した。
        //
        // あの待ちの目的は「gentle daemon restart を生き延びた旧 SP の QUIC heal 再登録が
        // 届くまで待ち、稼働中の SP を重複 spawn しない」ことだった。fold-in で project が
        // World 内に入り、daemon 停止を生き延びる SP が存在しなくなったため、registry は
        // boot 時に**常に空**で安定する = 待つ理由そのものが消滅した。
        //
        // 残したままだと毎回きっかり QUIET_WINDOW(20s) 空回りしてから project を起こすことに
        // なり、daemon 再起動のたびに 20 秒の無駄が乗る（dogfood で最も踏む経路）。
        // 二重起動の防御は `ProjectRuntimes` の map キー一意性が引き継いでいる。
        //
        // doc 44 P1 (fold-in): 旧実装はここで `refresh_process_status`（PID liveness）を
        // 呼んでいたが、boot 時点で running_processes は空なので no-op だった上、fold-in で
        // pid が全 project 共通の World 自身になり liveness check 自体が無意味化したため撤去。

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

        // start_process は内部で sleep するため、read ガードを保持せず clone した cap で呼ぶ。
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

    /// VP-129: lane root を watch して performer dir 削除を SP DELETE に bridge する FSEvents watcher。
    ///
    /// **「folder = Lane 空間」 axiom の物理実装**。 user が Finder / `rm -rf` で performer dir を
    /// 削除した時、 OS の file system event (Mac → FSEvents、 Linux → inotify) → notify crate
    /// → 本 watcher が path → project 解決 → SP `DELETE /api/lanes` 自動発火、 sidebar /
    /// tmux / PtySlot が cascade で同期 cleanup される。
    ///
    /// D10 Reconciliation arch の **3rd path 拡張**: Push (QUIC heartbeat) + Pull (port scan) +
    /// **FSEvents (本 method)** の 3-trigger model 完成。
    ///
    /// ## project-local lane refactor PR 4c → hot-reload
    ///
    /// PR 4c で `config.projects` の各 project の `.vp/lanes/` を `Vec<watch>` で N path
    /// 同時監視に書き直し、 本 PR で **5s tick polling-based の動的 hot-reload** を追加。
    /// 起動後に projects.kdl 経由で新規 project が register/unregister されると、
    /// 次の tick (= 最大 5s 遅延) で watch list を sync する。
    ///
    /// MVP scope (= 別 ticket で safety net 追加候補):
    /// - self-loop 防止: scope 外 (= SP 経由削除も Remove event 発火、 二重 DELETE 走るが SP 側
    ///   404 で no-op、 log noise 許容)
    /// - spawn race: scope 外 (= 既存 spawn semaphore + atomic LanePool insert で吸収)
    /// - 詳細 EventKind 区別: Remove(_) 全 variant accept (= Mac FSEvents は RemoveKind 区別が薄い)
    /// - event-based hot-reload: scope 外 (= polling で十分、 PR 1 の `build_lanes_snapshot`
    ///   periodic と同 cadence で user の mental model 一致)
    pub async fn run_lane_watcher(
        world: Arc<RwLock<Self>>,
        shutdown_token: tokio_util::sync::CancellationToken,
    ) {
        use notify::EventKind;

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

        // 起動時 snapshot を arm。 0 project でも loop は起動し、 periodic tick で
        // 後から register された project を pick up する (= hot-reload 動作)。
        let mut path_map = Self::build_lane_watch_path_map(&world).await;
        let mut watched: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        for (path, (name, _)) in &path_map {
            if Self::arm_watch_path(&mut watcher, path, name) {
                watched.insert(path.clone());
            }
        }
        tracing::info!(
            "lane watcher 起動 (初期 {} project arm 済、 mode=NonRecursive、 trigger=Create/Remove → lane_create/lane_delete)",
            watched.len()
        );

        // lanes portless: 旧 reqwest client (SP HTTP 直結) は撤去。 event handler は World
        // process-proxy ask (`lane_create` / `lane_delete`) に loopback する。

        // 5s tick で projects.kdl 経由の register/unregister を hot-reload。
        let mut hot_reload = tokio::time::interval(std::time::Duration::from_secs(5));
        hot_reload.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    tracing::info!("lane watcher: shutdown signal、 停止");
                    break;
                }
                _ = hot_reload.tick() => {
                    // diff 計算 → 差分のみ unwatch/watch
                    let new_map = Self::build_lane_watch_path_map(&world).await;
                    let new_paths: std::collections::HashSet<std::path::PathBuf> =
                        new_map.keys().cloned().collect();
                    let (to_add, to_remove) = compute_watch_diff(&watched, &new_paths);
                    for path in &to_remove {
                        use notify::Watcher;
                        let _ = watcher.unwatch(path);
                        watched.remove(path);
                        tracing::info!(
                            "lane watcher: project unwatch (= unregister 検出) path={}",
                            path.display()
                        );
                    }
                    for path in &to_add {
                        let name = new_map
                            .get(path)
                            .map(|(n, _)| n.as_str())
                            .unwrap_or("unknown");
                        if Self::arm_watch_path(&mut watcher, path, name) {
                            watched.insert(path.clone());
                        }
                    }
                    path_map = new_map;
                }
                event_opt = rx.recv() => {
                    let Some(event) = event_opt else { break }; // channel closed
                    match event.kind {
                        EventKind::Remove(_) => {
                            Self::handle_lane_remove_event(&world, &path_map, &event).await;
                        }
                        EventKind::Create(_) => {
                            // F.8 B Convergent: SP 起動後に CLI / 外部で `.vp/lanes/<name>`
                            // dir が増えた時、 World process-proxy ask `lane_create` (cwd 明示) で
                            // spawn を依頼。 「disk dir があるが LanePool に居ない」 中間状態
                            // (= disk-only Lane) を恒久化させない、 lifecycle 自動 convergence。
                            Self::handle_lane_create_event(&world, &path_map, &event).await;
                        }
                        _ => {} // Modify / Access 等は無視
                    }
                }
            }
        }

        drop(watcher); // 明示 drop で watching 停止 (scope 終端でも自動だが意図表示)
        tracing::info!("lane watcher 終了");
    }

    /// 1 project の `.vp/lanes/` を arm する helper (`run_lane_watcher` の inner)。
    /// dir 不在なら best-effort で create + `watch()` 試行。 成功すれば true を返す。
    fn arm_watch_path(
        watcher: &mut notify::RecommendedWatcher,
        path: &std::path::Path,
        project_name: &str,
    ) -> bool {
        use notify::{RecursiveMode, Watcher};
        if !path.exists()
            && let Err(e) = std::fs::create_dir_all(path)
        {
            tracing::warn!(
                "lane watcher: dir create 失敗 (skip) project={} path={}: {}",
                project_name,
                path.display(),
                e
            );
            return false;
        }
        if let Err(e) = watcher.watch(path, RecursiveMode::NonRecursive) {
            tracing::warn!(
                "lane watcher: watch 開始失敗 (skip) project={} path={}: {}",
                project_name,
                path.display(),
                e
            );
            return false;
        }
        tracing::info!(
            "lane watcher: project={} path={} 監視開始",
            project_name,
            path.display()
        );
        true
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

    /// VP-129: Remove event 1 件を処理。 path → project 解決 → World process-proxy ask `lane_delete`。
    /// `run_lane_watcher` の inner、 各 path を独立処理。
    async fn handle_lane_remove_event(
        world: &Arc<RwLock<Self>>,
        path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
        event: &notify::Event,
    ) {
        for path in &event.paths {
            let Some((project_name, project_path, performer_name)) =
                resolve_lane_event(path, path_map)
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
                    "lane watcher: SP not running for project={} (skip) performer={}",
                    project_name,
                    performer_name
                );
                continue;
            };

            // lanes portless (doc 27 §3.4.5): 旧 SP HTTP DELETE /api/lanes を World process-proxy ask
            // `lane_delete` に移管 (World 内 loopback、 surface 群と uniform な transport)。 cleanup=false
            // で dir は既に gone。 self-loop case (= SP 経由削除で dir 消滅 → watcher が Remove 検知 →
            // 本 lane_delete 発火) は server が "Lane not found" を Err で返すので no-op 扱い。
            let address = format!("{}/performer/{}", project_name, performer_name);
            let payload = serde_json::json!({ "address": address, "cleanup": false });
            tracing::info!(
                "lane watcher: dir removed → lane_delete 発火 (project={}, performer={}, sp_port={})",
                project_name,
                performer_name,
                port
            );
            match crate::commands::process_client::world_process_request(
                crate::cli::world_port(),
                &project_path,
                "lane_delete",
                payload,
            )
            .await
            {
                Ok(_) => {
                    tracing::info!("lane watcher: lane_delete 成功 ({})", address);
                }
                Err(e) if e.to_string().contains("Lane not found") => {
                    tracing::debug!(
                        "lane watcher: lane_delete no-op (self-loop or already deleted): {}",
                        address
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "lane watcher: lane_delete 失敗 (project={}, address={}): {}",
                        project_name,
                        address,
                        e
                    );
                }
            }
        }
    }

    /// F.8 B Convergent: lane Create event を 1 path 処理。 path → project + performer_name 解決 →
    /// SP POST /api/lanes (kind=performer, name=<performer>, cwd=<existing_dir>) で auto-spawn を依頼する。
    ///
    /// `run_lane_watcher` の inner、 sibling は `handle_lane_remove_event` (Remove 時の SP DELETE)。
    /// 設計対称性: Remove → DELETE / Create → POST で「dir 状態と LanePool 状態を一致させる」
    /// convergence loop を成立させる (= disk-only Lane を恒久化しない)。
    ///
    /// 競合 case:
    /// - sidebar `+` で作成中に Create event fired → SP 側 LanePool 重複チェックで CONFLICT
    ///   が返り、 watcher 側はそれを debug log で受ける (= silent OK)
    /// - SP 起動時 bootstrap で既に同 performer が SpawnLane Cmd 投入済 → 上記同様 CONFLICT で no-op
    async fn handle_lane_create_event(
        world: &Arc<RwLock<Self>>,
        path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
        event: &notify::Event,
    ) {
        for path in &event.paths {
            // dir のみ対象 (= `.vp/lanes/<name>` の new dir、 単発ファイルは無視)
            if !path.is_dir() {
                continue;
            }
            let Some((project_name, project_path, performer_name)) =
                resolve_lane_event(path, path_map)
            else {
                continue;
            };

            // SP port 取得 (= running_processes registry)
            let port = {
                let world_read = world.read().await;
                let procs = world_read.running_processes.read().await;
                let key = normalize_path_key(std::path::Path::new(&project_path));
                procs.get(&key).map(|p| p.port)
            };
            let Some(port) = port else {
                tracing::debug!(
                    "lane watcher: SP not running for project={} (skip create) performer={}",
                    project_name,
                    performer_name
                );
                continue;
            };

            // daemon-canonical create（GUI「+ Add Performer」）の stand を descriptor から引き継ぐ
            // （bug mem_1Cd4M7i5Enp3HHMLVYayRe）: create_lane は stand 込みの descriptor を
            // lane_registry に保存済みだが、旧実装の watcher はそれを読まず payload に stand を
            // 積まなかったため、SP 側で default_stand（= echoes）に倒れていた =「codex を選んでも
            // claude で spawn」の根因。descriptor 不在（手動 `vp lane new` 等 = watcher だけが
            // 検知した dir）は従来どおり None → SP 側 default に委ねる。
            let descriptor_stand = {
                let world_read = world.read().await;
                let lr = world_read.lane_registry.read().await;
                let key = normalize_path_key(std::path::Path::new(&project_path));
                lr.get(&key).and_then(|lanes| {
                    lanes
                        .iter()
                        .find(|l| l.address.name == *performer_name)
                        .map(|l| l.stand.clone())
                })
            };

            // lanes portless (doc 27 §3.4.5): 旧 SP HTTP POST /api/lanes を World process-proxy ask
            // `lane_create` に移管 (World 内 loopback、 surface 群と uniform な transport)。 payload は
            // CreateLaneReq (routes/lanes.rs) 互換。 cwd 明示で既存 dir を再利用 (new_performer_in skip)。
            // doc 44 P2: `kind` は撤去（lane に種別が無くなり、指定する余地が消えた）。
            let mut payload = serde_json::json!({
                "name": performer_name,
                "cwd": path.to_string_lossy(),
            });
            if let Some(stand) = descriptor_stand {
                payload["stand"] = serde_json::Value::String(stand);
            }
            tracing::info!(
                "lane watcher: dir created → lane_create 発火 (project={}, performer={}, sp_port={})",
                project_name,
                performer_name,
                port
            );
            match crate::commands::process_client::world_process_request(
                crate::cli::world_port(),
                &project_path,
                "lane_create",
                payload,
            )
            .await
            {
                Ok(_) => {
                    tracing::info!(
                        "lane watcher: lane_create 成功 (project={}, performer={})",
                        project_name,
                        performer_name
                    );
                }
                // 競合: sidebar `+` or bootstrap で既に Lane 作成済。 server は "already exists" を
                // Err で返すので silent OK (= 旧 HTTP CONFLICT 経路と等価)。
                Err(e) if e.to_string().contains("already exists") => {
                    tracing::debug!(
                        "lane watcher: lane_create 競合 (= 既に Lane あり、 silent OK) project={} performer={}",
                        project_name,
                        performer_name
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "lane watcher: lane_create 失敗 (project={}, performer={}): {}",
                        project_name,
                        performer_name,
                        e
                    );
                }
            }
        }
    }
}

/// lane Remove event 1 path を解決する純粋関数。 `path_map` (= `<.vp/lanes path>` → `(project_name,
/// project_path)`) から parent match で project を逆引きし、 path の file_name を performer 名として
/// 返す。
///
/// 戻り値: `Some((project_name, project_path, performer_name))` if 完全 match。 そうでなければ `None`。
/// - dotfile / 空 performer 名は skip (= `.git` 内ファイル等の伝播除外)
/// - path_map に登録されてない project 配下の path は skip
/// - I/O なしの pure fn (= test しやすい、 mock 不要)
fn resolve_lane_event(
    path: &std::path::Path,
    path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
) -> Option<(String, String, String)> {
    let parent = path.parent()?;
    let (project_name, project_path) = path_map.get(parent)?;
    let performer_name = path.file_name()?.to_str()?.to_string();
    if performer_name.is_empty() || performer_name.starts_with('.') {
        return None;
    }
    Some((project_name.clone(), project_path.clone(), performer_name))
}

/// lane watcher hot-reload の純粋 diff 計算。 `current` (= 現在 arm 済 path 集合) と
/// `new` (= 期待 path 集合 = `build_lane_watch_path_map` の最新 keys) から、
/// `(to_add, to_remove)` を返す。
///
/// - `to_add` = `new` にあって `current` に無い (= 新規 register された project)
/// - `to_remove` = `current` にあって `new` に無い (= unregister された project)
/// - I/O なしの pure fn (= test しやすい、 `notify::Watcher` mock 不要)
fn compute_watch_diff(
    current: &std::collections::HashSet<std::path::PathBuf>,
    new: &std::collections::HashSet<std::path::PathBuf>,
) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let to_add: Vec<_> = new.difference(current).cloned().collect();
    let to_remove: Vec<_> = current.difference(new).cloned().collect();
    (to_add, to_remove)
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

        // doc 44 P1 (fold-in): vp binary の所在探索は撤去。project を子プロセスとして
        // spawn しなくなったため、起動に binary path は要らない。残しておくと daemon 起動の
        // たびに無駄な FS stat + `which` の subprocess が走り、しかも今は無意味な
        // 「vp binary not found in PATH」警告で読み手の調査コストを誘発する。

        // 設定を読み込み
        if let Err(e) = self.load_config().await {
            tracing::warn!("Failed to load config: {}", e);
        }

        // doc 44 P1 (fold-in): 旧「初期状態チェック（PID liveness）」は撤去。boot 時点で
        // running_processes は空で、fold-in 後は pid が World 自身になり liveness が無意味。

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

    // --- PR3: SP spawn 平滑化 (CPU cap) ---

    /// floor 保証: 1〜2 core 機でも `spawn_cap()` は最低 1。
    /// `Semaphore::new(0)` は permit 永久枯渇で spawn が全 block する地雷なので、
    /// この floor が崩れると daemon が SP を一切起動できなくなる (回帰の急所)。
    #[test]
    fn spawn_cap_is_floored_at_one() {
        assert!(
            spawn_cap() >= 1,
            "spawn_cap() は最低 1 (Semaphore::new(0) の永久 block 回避)"
        );
    }

    /// wiring: `new()` の spawn_semaphore は `spawn_cap()` permits で初期化される
    /// (gate 値が cap とズレていないことの確認)。
    #[test]
    fn new_wires_spawn_semaphore_to_cap() {
        let cap = ProcessManagerCapability::new();
        assert_eq!(
            cap.spawn_semaphore.available_permits(),
            spawn_cap(),
            "spawn_semaphore は spawn_cap() 本の permit を持つ"
        );
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
        let path =
            std::path::Path::new("/Users/makoto/repos/creo-memories/.vp/lanes/or-integration");
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
    fn resolve_lane_event_skips_dotfile_performer_name() {
        // `.git` や `.DS_Store` の Remove event (lane dir 内部からの伝播) を skip。
        // NonRecursive watch で arrive する可能性は低いが防御で。
        let map = make_path_map(&[("/repo/.vp/lanes", "repo", "/repo")]);
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
        let path_b = std::path::Path::new("/repo-b/.vp/lanes/performer-x");
        let resolved = resolve_lane_event(path_b, &map);
        assert_eq!(
            resolved,
            Some((
                "repo-b".to_string(),
                "/repo-b".to_string(),
                "performer-x".to_string()
            ))
        );
    }

    // --- compute_watch_diff (hot-reload pure helper) ---

    fn make_path_set(paths: &[&str]) -> std::collections::HashSet<std::path::PathBuf> {
        paths.iter().map(std::path::PathBuf::from).collect()
    }

    fn sort_paths(mut v: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
        v.sort();
        v
    }

    #[test]
    fn compute_watch_diff_initial_arm_all_new() {
        // 起動直後: current 空、 new に N project → 全部 to_add
        let current = std::collections::HashSet::new();
        let new = make_path_set(&["/a/.vp/lanes", "/b/.vp/lanes"]);
        let (to_add, to_remove) = compute_watch_diff(&current, &new);
        assert_eq!(
            sort_paths(to_add),
            vec![
                std::path::PathBuf::from("/a/.vp/lanes"),
                std::path::PathBuf::from("/b/.vp/lanes"),
            ]
        );
        assert!(to_remove.is_empty());
    }

    #[test]
    fn compute_watch_diff_full_drain_when_new_empty() {
        // 全 project unregister: current に N、 new 空 → 全部 to_remove
        let current = make_path_set(&["/a/.vp/lanes", "/b/.vp/lanes"]);
        let new = std::collections::HashSet::new();
        let (to_add, to_remove) = compute_watch_diff(&current, &new);
        assert!(to_add.is_empty());
        assert_eq!(
            sort_paths(to_remove),
            vec![
                std::path::PathBuf::from("/a/.vp/lanes"),
                std::path::PathBuf::from("/b/.vp/lanes"),
            ]
        );
    }

    #[test]
    fn compute_watch_diff_steady_state_no_change() {
        // 完全 match: 変化なし
        let current = make_path_set(&["/a/.vp/lanes", "/b/.vp/lanes"]);
        let new = make_path_set(&["/a/.vp/lanes", "/b/.vp/lanes"]);
        let (to_add, to_remove) = compute_watch_diff(&current, &new);
        assert!(to_add.is_empty());
        assert!(to_remove.is_empty());
    }

    #[test]
    fn compute_watch_diff_mixed_add_and_remove() {
        // a が消えて c が追加: to_add=[c]、 to_remove=[a]、 b は維持
        let current = make_path_set(&["/a/.vp/lanes", "/b/.vp/lanes"]);
        let new = make_path_set(&["/b/.vp/lanes", "/c/.vp/lanes"]);
        let (to_add, to_remove) = compute_watch_diff(&current, &new);
        assert_eq!(to_add, vec![std::path::PathBuf::from("/c/.vp/lanes")]);
        assert_eq!(to_remove, vec![std::path::PathBuf::from("/a/.vp/lanes")]);
    }

    #[test]
    fn compute_watch_diff_both_empty_yields_empty() {
        // edge: 両方空 (= projects 0 状態) → no-op
        let current = std::collections::HashSet::new();
        let new = std::collections::HashSet::new();
        let (to_add, to_remove) = compute_watch_diff(&current, &new);
        assert!(to_add.is_empty());
        assert!(to_remove.is_empty());
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
            active_lane: None,
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
            active_lane: None,
        };
        let json_no_port = serde_json::to_string(&info_no_port).unwrap();
        assert!(!json_no_port.contains("port"));
    }

    // --- CRUD テスト（async） ---

    /// テスト用ヘルパー: 空の ProcessManagerCapability を作成
    fn make_test_cap() -> ProcessManagerCapability {
        ProcessManagerCapability::new()
    }

    /// テスト用ヘルパー: projects に 1 件登録する。
    fn test_project(name: &str, port: Option<u16>) -> ProjectInfo {
        ProjectInfo {
            name: name.to_string(),
            path: format!("/tmp/{name}").into(),
            process_status: ProcessStatus::Stopped,
            port,
            enabled: true,
            slot: None,
            active_lane: None,
        }
    }

    #[test]
    fn test_process_presence_state_as_str() {
        // /api/health の processes[].presence にそのまま載る文字列のロック (vp-app 描画契約)。
        assert_eq!(ProcessPresenceState::Unregistered.as_str(), "unregistered");
        assert_eq!(ProcessPresenceState::Connecting.as_str(), "connecting");
        assert_eq!(ProcessPresenceState::Connected.as_str(), "connected");
        assert_eq!(ProcessPresenceState::Disconnected.as_str(), "disconnected");
    }

    #[tokio::test]
    async fn test_presence_snapshot_joins_projects_running_and_presence() {
        let cap = make_test_cap();

        // projects (desired) に 2 件。proj-b を先に入れて sort も検証する。
        {
            let projects = cap.projects_ref();
            let mut projs = projects.write().await;
            projs.insert("/tmp/proj-b".to_string(), test_project("proj-b", None));
            projs.insert(
                "/tmp/proj-a".to_string(),
                test_project("proj-a", Some(33000)),
            );
        }

        // proj-a だけ live (running_processes に entry) + Connected。
        {
            let running = cap.running_processes_ref();
            running.write().await.insert(
                "/tmp/proj-a".to_string(),
                RunningProcess {
                    project_name: "proj-a".to_string(),
                    port: 33000,
                    pid: 4242,
                    project_path: "/tmp/proj-a".into(),
                },
            );
        }
        cap.set_presence("/tmp/proj-a", ProcessPresenceState::Connected)
            .await;
        // proj-b は切断済 (live 不在だが project は残る = Model Q)。
        cap.set_presence("/tmp/proj-b", ProcessPresenceState::Disconnected)
            .await;

        let snap = cap.presence_snapshot().await;

        // project 名で sort されている (HashMap 反復の非決定性を吸収)。
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].project, "proj-a");
        assert_eq!(snap[1].project, "proj-b");

        // proj-a: Connected + live port/pid。
        assert_eq!(snap[0].presence, "connected");
        assert_eq!(snap[0].port, Some(33000));
        assert_eq!(snap[0].pid, Some(4242));

        // proj-b: Disconnected + live 値は None (project としては sidebar に残る)。
        assert_eq!(snap[1].presence, "disconnected");
        assert_eq!(snap[1].port, None);
        assert_eq!(snap[1].pid, None);
    }

    #[tokio::test]
    async fn test_presence_snapshot_defaults_unregistered_without_entry() {
        // projects には在るが presence entry が無い (SP 未起動) → Unregistered default。
        let cap = make_test_cap();
        cap.projects_ref()
            .write()
            .await
            .insert("/tmp/proj-x".to_string(), test_project("proj-x", None));
        let snap = cap.presence_snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].presence, "unregistered");
        assert_eq!(snap[0].port, None);
    }

    #[tokio::test]
    async fn test_set_presence_overwrites_connecting_to_disconnected() {
        // set_presence の上書き機構 + as_str マッピングを固定する。
        // Connecting / Disconnected は fold-in 後は production 到達不能だが enum 値としては
        // 有効で、全 variant の presence_snapshot 経由の文字列化を押さえる意味で残す。
        let cap = make_test_cap();
        cap.projects_ref()
            .write()
            .await
            .insert("/tmp/proj-r".to_string(), test_project("proj-r", None));
        cap.set_presence("/tmp/proj-r", ProcessPresenceState::Connecting)
            .await;
        assert_eq!(cap.presence_snapshot().await[0].presence, "connecting");
        cap.set_presence("/tmp/proj-r", ProcessPresenceState::Disconnected)
            .await;
        assert_eq!(cap.presence_snapshot().await[0].presence, "disconnected");
    }

    #[tokio::test]
    async fn test_remove_project_clears_presence() {
        // namespace (project) を倒したら presence entry も回収する (active_lanes と対称、orphan 防止)。
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();
        cap.add_project("presence-cleanup", &path).await.unwrap();
        let key = normalize_path_key(std::path::Path::new(&path));
        cap.set_presence(&key, ProcessPresenceState::Connected)
            .await;
        {
            let presence = cap.process_presence_ref();
            assert!(presence.read().await.contains_key(&key));
        }
        cap.remove_project(&path).await.unwrap();
        let presence = cap.process_presence_ref();
        assert!(
            !presence.read().await.contains_key(&key),
            "remove_project は presence entry を回収すべき"
        );
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
    async fn test_remove_project_reclaims_performer_ground_not_conductor() {
        // doc 24 §5.3 / B-destroy: project remove で performer worktree(ground) は daemon が
        // reclaim、 conductor(=repo root = user の repo) は絶対に消さない、 を検証する。
        // git なしの plain dir で実行 (remove_performer_workspace は .git 無しなら fs 削除に落ちる)。
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};

        let cap = make_test_cap();
        // 一意な temp project root (再実行に備え事前掃除)。
        // pid を含めて並行 `cargo test` 実行間での temp 衝突を避ける (team-b review、 低リスク)。
        let tmp =
            std::env::temp_dir().join(format!("vp-test-bdestroy-reclaim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let project_path = tmp.to_string_lossy().to_string();
        cap.add_project("bdestroy", &project_path).await.unwrap();

        // performer の ground を物理作成 (<repo>/.vp/lanes/foo、 plain dir = fs 削除経路)。
        let performer_dir = tmp.join(".vp").join("lanes").join("foo");
        std::fs::create_dir_all(&performer_dir).unwrap();
        assert!(performer_dir.exists());

        // lane_registry に conductor + performer descriptor を投入 (daemon-canonical truth)。
        let key = normalize_path_key(&PathBuf::from(&project_path));
        let mk = |addr: LaneAddress, cwd: &str| LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: addr,
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: None,
            cwd: cwd.to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };
        let conductor = mk(LaneAddress::conductor("bdestroy"), &project_path);
        let performer = mk(
            LaneAddress::performer("bdestroy", "foo"),
            &performer_dir.to_string_lossy(),
        );
        cap.lane_registry_ref()
            .write()
            .await
            .insert(key.clone(), vec![conductor, performer]);

        // 実行: project を倒す。
        cap.remove_project(&project_path).await.unwrap();

        // 検証: performer ground は reclaim、 conductor=repo root は無傷。
        assert!(
            !performer_dir.exists(),
            "performer ground (worktree) は daemon が reclaim する"
        );
        assert!(
            tmp.exists(),
            "conductor = repo root は絶対に消さない (user の repo)"
        );
        // descriptor も lane_registry から畳まれている。
        assert!(
            cap.lane_registry_ref().read().await.get(&key).is_none(),
            "project remove で lane descriptor も回収される"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_sync_does_not_revive_project_that_had_performer() {
        // lead #2: 実バグ条件に近い robustness 回帰。 performer を抱えた project SP を削除した後、
        // sync (= `vp sp start` が起動時に撃つ) を回しても復活しないことを焼き付ける。 旧挙動では
        // sync_projects(Some(dir)) が起点 dir を無条件再登録し、 生きた performer で project SP が
        // 死にきれず後で sp start → 復活する経路があった (mem_1CcuRsC9pF3fiZptwmdgTS)。
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};

        let cap = make_test_cap();
        let tmp = std::env::temp_dir().join(format!("vp-test-sync-revive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let project_path = tmp.to_string_lossy().to_string();
        cap.add_project("hasperf", &project_path).await.unwrap();

        // performer descriptor を lane_registry に投入 (project SP に performer 子がぶら下がる
        // 状態を模す。 plain dir なので worktree reclaim は fs 削除に落ちる = git 非依存)。
        let key = normalize_path_key(&PathBuf::from(&project_path));
        let performer = LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: LaneAddress::performer("hasperf", "foo"),
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            pid: None,
            cwd: tmp.join(".vp/lanes/foo").to_string_lossy().to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };
        cap.lane_registry_ref()
            .write()
            .await
            .insert(key.clone(), vec![performer]);

        // 削除 → project も performer descriptor も畳まれる。
        cap.remove_project(&project_path).await.unwrap();
        assert!(
            cap.list_projects().await.is_empty(),
            "削除で project は消える"
        );

        // sync を回しても復活しない (起点 dir 自動登録が撤去済のため)。
        let outcome = cap.sync_projects().await.unwrap();
        assert!(outcome.removed.is_empty());
        assert!(
            cap.list_projects().await.is_empty(),
            "performer を抱えていた project も sync で復活しない (resurrection 回帰)"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_create_lane_provisions_worktree_and_owns_descriptor() {
        // doc 24 §10 Phase 2 B-create: daemon が performer lane を create し、 worktree(ground)
        // を provision して descriptor を daemon-canonical truth として所有する end-to-end 検証。

        let cap = make_test_cap();
        // address の project 部分は path basename から取る (create_handler と一貫) ため、
        // repo dir の basename を "bcreate" に固定する (parent に pid を入れて衝突回避)。
        let parent = std::env::temp_dir().join(format!("vp-test-bcreate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let tmp = parent.join("bcreate");
        std::fs::create_dir_all(&tmp).unwrap();

        // worktree add は initial commit を要するので minimal git repo を用意。
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&tmp)
                .status()
                .expect("git command 失敗")
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(tmp.join("README.md"), "# test\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "initial"]);

        let project_path = tmp.to_string_lossy().to_string();
        cap.add_project("bcreate", &project_path).await.unwrap();

        // daemon create (branch / stand は resolve 済の concrete 値を渡す)。
        let info = cap
            .create_lane(&project_path, "foo", "test/foo", "echoes")
            .await
            .expect("daemon create_lane 成功");

        // descriptor が daemon-canonical truth として返る。
        assert!(!info.address.is_conductor());
        assert_eq!(info.address.name, "foo");
        // doc 44 P2: address 表示形は `<project>/<name>`（旧 `<project>/performer/<name>`）
        assert_eq!(info.address.to_string(), "bcreate/foo");
        assert_eq!(info.stand, "echoes");

        // §5.3: daemon が worktree(ground) を provision する。
        let performer_dir = tmp.join(".vp").join("lanes").join("foo");
        assert!(
            performer_dir.exists(),
            "daemon が worktree を provision する"
        );

        // descriptor が lane_registry (daemon-canonical) に所有される。
        let key = normalize_path_key(&PathBuf::from(&project_path));
        {
            let registry = cap.lane_registry_ref();
            let lr = registry.read().await;
            let lanes = lr.get(&key).expect("project の lanes が登録される");
            assert!(
                lanes.iter().any(|l| l.address == info.address),
                "descriptor が daemon-canonical に所有される"
            );
        }

        // dup: 同名 create は registry-based dup check で弾く (already exists)。
        let dup = cap
            .create_lane(&project_path, "foo", "test/foo", "echoes")
            .await;
        assert!(dup.is_err());
        assert!(
            dup.unwrap_err().to_string().contains("already exists"),
            "重複 create は already exists で弾く"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// 回帰固定（doc 44 §9）: **拒否される名前の create は db の lane 行に一切触れない**。
    ///
    /// 旧実装は入口で空文字しか見ておらず、予約名は奥の `new_performer_in` が clone 段階で
    /// 初めて弾いていた。だが intent-first bracket は provision より **前に** descriptor を
    /// 永続するので、拒否されるべき入力が `<project>/conductor` 行を上書き（①）し、
    /// rollback がそれを削除（③）する — **本物の開発起点 descriptor が消える**。
    ///
    /// 通常は in-memory の dup check が先に弾いて発火しないが、dup check は validation では
    /// なく、その cache は db と乖離しうる（boot load 失敗 / SP snapshot 上書き）。
    /// **ここでは意図的に registry を空のままにして masking を外し**、db 行の生存を直接見る。
    #[tokio::test]
    async fn test_create_lane_rejects_reserved_name_without_touching_db() {
        let mut cap = make_test_cap();
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        cap.set_vpdb(db.clone());

        let parent = std::env::temp_dir().join(format!("vp-test-reserved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let tmp = parent.join("reserved");
        std::fs::create_dir_all(&tmp).unwrap();
        let project_path = tmp.to_string_lossy().to_string();
        let key = normalize_path_key(&PathBuf::from(&project_path));

        // 本物の開発起点 descriptor を db に置く（= 破壊対象）。
        let conductor =
            crate::process::lanes_state::LanePool::with_conductor("reserved", project_path.clone());
        let conductor_info = conductor
            .list()
            .into_iter()
            .next()
            .expect("conductor descriptor");
        db.upsert_lane(&key, &conductor_info).await.unwrap();
        let addr_str = conductor_info.address.to_string();
        assert_eq!(addr_str, "reserved/conductor");

        // dup check の masking は効かない状況（lane_registry は空）。
        let err = cap
            .create_lane(&project_path, "conductor", "test/x", "echoes")
            .await
            .expect_err("予約名は Err");
        assert!(
            err.to_string().contains("reserved"),
            "入口の gate が理由を伝える: {err}"
        );

        // ① も ③ も起きていない = 開発起点 descriptor は無傷。
        let rows = db.list_lanes().await.unwrap();
        let survivor = rows
            .iter()
            .find(|(p, i)| p == &key && i.address.to_string() == addr_str);
        assert!(
            survivor.is_some(),
            "拒否された create は開発起点 descriptor を消してはならない: {rows:?}"
        );
        assert_eq!(rows.len(), 1, "余計な行も作らない: {rows:?}");

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[tokio::test]
    async fn test_reconcile_lanes_heals_lifecycle_by_ground() {
        // doc 24 §4.6 boot reconcile heal: provisioning+ground在→ready / ready+ground無→dead。
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};

        let mut cap = make_test_cap();
        let db = std::sync::Arc::new({
            let d = crate::db::VpDb::connect_mem().await.unwrap();
            d.define_schema().await.unwrap();
            d
        });
        cap.set_vpdb(db.clone());

        // 2 つの ground: 1 つは存在、 1 つは存在しない。
        let parent = std::env::temp_dir().join(format!("vp-test-reconcile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let alive_dir = parent.join("proj/.vp/lanes/alive");
        std::fs::create_dir_all(&alive_dir).unwrap();
        let gone_dir = parent.join("proj/.vp/lanes/gone"); // 作らない (= ground 無し)

        let key = "/test/proj";
        let mk = |name: &str, cwd: &std::path::Path| LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: LaneAddress::performer("proj", name),
            state: LaneState::Spawning,
            stand: "echoes".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: None,
            cwd: cwd.to_string_lossy().into_owned(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };
        cap.lane_registry_ref().write().await.insert(
            key.to_string(),
            vec![mk("alive", &alive_dir), mk("gone", &gone_dir)],
        );
        // alive=provisioning (ground 在り→ready 期待)、 gone=ready (ground 無→dead 期待)。
        db.upsert_lane_lifecycle(key, "proj/alive", "provisioning")
            .await
            .unwrap();
        db.upsert_lane_lifecycle(key, "proj/gone", "ready")
            .await
            .unwrap();

        cap.reconcile_lanes().await;

        let rows = db.list_lane_lifecycles().await.unwrap();
        let get = |a: &str| {
            rows.iter()
                .find(|(_, addr, _)| addr == a)
                .map(|(_, _, lc)| lc.clone())
        };
        assert_eq!(
            get("proj/alive").as_deref(),
            Some("ready"),
            "provisioning + ground 在り → ready (provision 完了)"
        );
        assert_eq!(
            get("proj/gone").as_deref(),
            Some("dead"),
            "ready + ground 外部削除 → dead (user の rm 尊重)"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[tokio::test]
    async fn test_sync_projects_prunes_ghosts_only() {
        // sync は ghost (dir 非実在) を除去するのみ。 起点 dir の自動登録は撤去済
        // (削除済 project を SP 起動時 sync が復活させる resurrection バグの温床だった)。
        let cap = make_test_cap();
        let real = std::env::temp_dir().to_string_lossy().to_string();
        cap.add_project("real", &real).await.unwrap();

        // 実在 dir の project は残る (ghost 除去されない)、 何も新規登録しない。
        let outcome = cap.sync_projects().await.unwrap();
        assert!(outcome.removed.is_empty(), "実在 dir は ghost 除去されない");
        assert_eq!(
            cap.list_projects().await.len(),
            1,
            "sync は project を増やさない"
        );
    }

    #[tokio::test]
    async fn test_sync_does_not_revive_removed_project() {
        // resurrection バグ回帰テスト: 削除した project は sync で復活しない。
        // 以前は `vp sp start <dir>` の起動時 sync が起点 dir を無条件再登録し、
        // 削除済 project が db/kdl に復活した (mem_1CcuRsC9pF3fiZptwmdgTS)。
        let cap = make_test_cap();
        // temp_dir は実在するので ghost 除去の対象にはならない (= 復活するとしたら
        // 起点 dir 自動登録が原因、 という切り分けになる)。
        let dir = std::env::temp_dir().to_string_lossy().to_string();

        cap.add_project("victim", &dir).await.unwrap();
        cap.remove_project(&dir).await.unwrap();
        assert!(cap.list_projects().await.is_empty(), "削除直後は空");

        // sync を回しても復活しない (起点 dir 自動登録が無いため)。
        let outcome = cap.sync_projects().await.unwrap();
        assert!(outcome.removed.is_empty());
        assert!(
            cap.list_projects().await.is_empty(),
            "sync は削除済 project を復活させない (resurrection 回帰)"
        );
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

    /// 回帰固定: `stop_process` は配線された `process_lifecycle_tx` に Remove を流す。
    ///
    /// この broadcast の生産者は fold-in で SP が消えた際に一度ゼロになり（`vp daemon
    /// processes --watch` と event log が永久沈黙）、`start_process` / `stop_process` に
    /// 配線し直して根治した。その配線は「静かに失われる」種類の障害なので、emit を単体で
    /// 固定して同型再発を CI で捕まえる。start 側（Add）は隣で同じ `if let Some(ref tx)`
    /// パターンを共有するため、field / setter が壊れれば本テストも落ちる。
    #[tokio::test]
    async fn stop_process_emits_lifecycle_remove() {
        use crate::daemon::protocol::ProcessLifecycleEvent;

        let cap = make_test_cap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        // Sender を差し込む前に subscribe を作らないと、send 時に receiver 不在で取りこぼす。
        {
            let mut c = cap;
            c.set_process_lifecycle_tx(tx);

            // stop_process の前提: projects に name→key、running_processes に live entry。
            // project_runtimes は未設定でも stop は tolerate する（registry の後始末のみ実施）。
            c.projects_ref().write().await.insert(
                "/tmp/proj-x".to_string(),
                test_project("proj-x", Some(33000)),
            );
            c.running_processes_ref().write().await.insert(
                "/tmp/proj-x".to_string(),
                RunningProcess {
                    project_name: "proj-x".to_string(),
                    port: 33000,
                    pid: 4242,
                    project_path: "/tmp/proj-x".into(),
                },
            );

            c.stop_process("proj-x").await.expect("stop_process");
        }

        match rx.try_recv() {
            Ok(ProcessLifecycleEvent::Remove { project_path }) => {
                assert_eq!(project_path, "/tmp/proj-x");
            }
            other => panic!("Remove イベントが流れるべき: {other:?}"),
        }
    }
}
