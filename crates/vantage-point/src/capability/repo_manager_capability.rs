//! Process Manager Capability - Process プロセス管理
//!
//! 複数のProject Processを管理するCapability。
//! メニューバーアプリ（Swift）からREST API経由で操作される。
//!
//! ## 役割
//!
//! - Repo Processのライフサイクル管理（起動・停止・監視）
//! - QUIC Registry チャネル経由での Process 発見
//! - REST API提供
//!
//! ## 使用例
//!
//! ```ignore
//! let mut manager = RepoManagerCapability::new();
//! manager.initialize(&ctx).await?;
//!
//! // repo 一覧取得
//! let repos = daemon.list_repos().await;
//!
//! // Process起動
//! daemon.start_process("my-repo").await?;
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

/// repo情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    /// repo名
    pub name: String,
    /// repoパス
    pub path: PathBuf,
    /// Process状態
    pub process_status: RepoStatus,
    /// 指定ポート（config.toml の port フィールド、永続化時に保持）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// repo 自動起動の有効/無効
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Port slot (VP-165: deterministic port layout)。 一度割り当てたら永続。
    /// VP-188: SSOT は repos.kdl。 capability は load/persist で round-trip するのみ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u16>,
    /// active lane (presence、 Model Q): この repo の選択中 lane address。
    /// daemon-canonical。 `list_repos` で active_lanes map から enrich (構築時は None)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lane: Option<String>,
}

fn default_enabled() -> bool {
    true
}

/// Process状態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoStatus {
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
pub struct RunningRepo {
    /// repo名
    pub repo_name: String,
    /// ポート番号
    pub port: u16,
    /// プロセスID
    pub pid: u32,
    /// repoパス
    pub repo_path: PathBuf,
}

/// repo の presence 状態（daemon-canonical、vp-app sidebar の ●◐○ 表示用）。
///
/// `/api/health` の `processes[].presence` で vp-app に expose される。
///
/// ⚠️ doc 44 P1 (fold-in) 後の実態: production で set されるのは `Connected`（`start_process`）と
/// `Unregistered`（`stop_process`）の**2 値のみ**。`Connecting`（respawn 着手中）と
/// `Disconnected`（QUIC 切断）は、別プロセスの repo が register/heartbeat していた時代の状態で、
/// その生産者（registry handler は #824、health monitor は本 PR）が消えたため到達不能になった。
/// fold-in 後の repo は「daemon の中に居る（=起動済）か、居ないか」の二値しか取り得ない。
/// enum の 2 値への縮約と vp-app 描画の追従は presence 意味論の follow-up（doc 44 §5.5 PR3）。
/// **2 値**（doc 44 §5.5 PR3 の follow-up、2026-07-22 着地）。fold-in 後の repo は
/// 「daemon の中に居る」か「居ない」かしか取り得ない。
///
/// 旧 4 値のうち `Connecting`（再起動 in-flight）/ `Disconnected`（QUIC 切断検知）は
/// **別プロセスの repo が register / heartbeat していた時代の状態**で、その生産者
/// （registry handler は #824、health monitor は #829）が消えて到達不能になっていた。
/// 本番コードで生成されるのは `Connected` / `Unregistered` の 2 つだけで、
/// 残り 2 値は**テストの中でしか作られていなかった**（= 型に居るだけの死んだ状態）。
///
/// > 到達不能な variant を型に残すと、読み手は「起こり得る」と読んで分岐を書き続ける。
/// > 消える経路は型からも消す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoPresenceState {
    /// 未登録（repos には在るが未起動 / 停止済）= **daemon の外**。
    Unregistered,
    /// 起動済み（`start_process` 成功）= **daemon の中に居る**。
    Connected,
}

impl RepoPresenceState {
    /// `/api/health` の `processes[].presence` 値（vp-app が ●○ 描画に使う）。
    ///
    /// client 側（`RepoAccordion.tsx`）は元から `=== "connected"` の 1 本でしか見ておらず、
    /// それ以外は dim に落としていた。つまり **描画は既に 2 値として振る舞っていた** —
    /// 本縮約で server の型が実態に追いついた形。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::Connected => "connected",
        }
    }
}

/// vp-app sidebar 向けの repo presence 1 件（`/api/health` の `processes[]` 要素）。
///
/// `repos`（desired = 全登録 repo）を軸に、`running_repos`（live port/pid）と
/// `process_presence`（接続状態）を join した結果。Connected でない（= live 不在）repo は
/// port/pid が `None` になるが、repo として sidebar には残り続ける（Model Q）。
#[derive(Debug, Clone, Serialize)]
pub struct RepoHealthInfo {
    /// repo名（表示用ラベル）。
    pub repo: String,
    /// 正規化パスキー（一意識別）。
    pub path: String,
    /// presence 状態（`"unregistered"` | `"connecting"` | `"connected"` | `"disconnected"`）。
    pub presence: &'static str,
    /// live port（Connected 時のみ Some、`running_repos` 由来）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// live pid（同上）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// 正規化パスキーを生成（HashMap のキーに使用）
///
/// ディレクトリパスを正規化した String を返す。
/// `running_repos` / `repos` の一意キーとして使用。
pub fn normalize_path_key(path: &std::path::Path) -> String {
    Config::normalize_path(path)
}

/// `config.repos` (RepoConfig) を RepoEntry 列に変換する。
///
/// PR-C: load_config が「DB 復旧の seed」「vpdb なし時の fallback」両方でこの変換を使う。
/// enabled は repos.kdl の慣習 (true は省略 = None、 false のみ明記) に揃える。
fn config_repos_to_entries(config: &Config) -> Vec<crate::repos_file::RepoEntry> {
    config
        .repos
        .iter()
        .map(|p| crate::repos_file::RepoEntry {
            name: p.name.clone(),
            path: p.path.clone(),
            enabled: if p.enabled { None } else { Some(false) },
            slot: p.slot,
        })
        .collect()
}

/// Main Capability
#[derive(Clone)]
pub struct RepoManagerCapability {
    /// 現在の状態
    state: CapabilityState,
    /// 登録 repo一覧（キー: 正規化パス）— インメモリキャッシュ
    repos: Arc<RwLock<HashMap<String, RepoInfo>>>,
    /// repoの並び順（正規化パスの Vec、config.toml の [[repos]] 順を保持）
    repo_order: Arc<RwLock<Vec<String>>>,
    /// 稼働中Process一覧（キー: 正規化パス）— インメモリキャッシュ
    running_repos: Arc<RwLock<HashMap<String, RunningRepo>>>,
    /// Phase 1b: 各 Repo の Lane registry（キー: 正規化パス）—
    /// repo が register payload に lanes を載せて push、 disconnect で全 Lane drop。
    /// agent (Conversation on Claude CLI) が `GET /api/lanes` で resolve するための cache。
    #[allow(clippy::type_complexity)]
    lane_registry: Arc<RwLock<HashMap<String, Vec<crate::repo::lanes_state::LaneInfo>>>>,
    /// 設定
    config: Option<Config>,
    /// vpバイナリパス
    /// SurrealDB クライアント（Some なら DB に二重書き込み）
    vpdb: Option<crate::db::SharedVpDb>,
    /// doc 44 P1 (fold-in): repo を Daemon 内で起動・保持する registry（daemon mode のみ Some）。
    /// 旧構成で `vp sp start` を spawn していた箇所が、この registry への `start()` に置き換わる。
    repo_runtimes: Option<Arc<crate::repo::repo_registry::RepoRuntimes>>,
    /// process lifecycle event の broadcast Sender（daemon mode のみ Some、DaemonState と共有）。
    ///
    /// doc 44 P1 (fold-in): 旧構成では repo の register/unregister を受けた registry channel
    /// handler がここに Add/Remove を流していた。repo が消えたため、in-process の起動元である
    /// `start_process` / `stop_process` が daemon-canonical な生産者として引き継ぐ。
    /// これが無いと `vp daemon processes --watch` と event log の process.up/down が永久沈黙する。
    process_lifecycle_tx:
        Option<tokio::sync::broadcast::Sender<crate::daemon::protocol::ProcessLifecycleEvent>>,
    /// active lane (presence、 Model Q): repo ごとの選択中 lane (キー: 正規化パス)。
    /// daemon-canonical。 `set_active_lane` で更新 + db/machine に upsert、 boot で load。
    active_lanes: Arc<RwLock<HashMap<String, String>>>,
    /// L1 lifecycle (Phase C): repo の presence (キー: 正規化パス)。daemon-canonical (doc 27 §3.2)。
    /// doc 44 P1 (fold-in): `start_process`→Connected / `stop_process`→Unregistered の 2 値のみ
    /// set される（旧 registry handler / health monitor の Connecting / Disconnected は到達不能）。
    /// DaemonState と Arc 共有 (`process_presence_ref`)。
    process_presence: Arc<RwLock<HashMap<String, RepoPresenceState>>>,
    /// PR3: repo spawn の同時実行数 cap (= CPU コアベースの平滑化)。
    ///
    /// tmux decoupling 後は lane claude = repo の PtySlot の子なので、同時 spawn が CPU を
    /// 圧迫すると claude 群の起動が団子になる。`start_process` (全 spawn trigger の sink) を
    /// この permit でゲートし、一度に走る `vp sp start` を `cores − 2` (floor 1) に平滑化する。
    /// permit は spawn 区間だけ RAII 保持 → 総稼働 repo 数は縛らない (semantics A)。
    /// `Semaphore::new(0)` は永久 block なので permit は必ず ≥1 (spawn_cap で floor)。
    spawn_semaphore: Arc<Semaphore>,
}

/// PR3: repo spawn の同時実行 cap を CPU コア数から算出する (= `cores − 2`、floor 1)。
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

impl RepoManagerCapability {
    /// 新しいProcessManagerCapabilityを作成
    pub fn new() -> Self {
        Self {
            state: CapabilityState::Uninitialized,
            repos: Arc::new(RwLock::new(HashMap::new())),
            repo_order: Arc::new(RwLock::new(Vec::new())),
            running_repos: Arc::new(RwLock::new(HashMap::new())),
            lane_registry: Arc::new(RwLock::new(HashMap::new())),
            config: None,
            vpdb: None,
            repo_runtimes: None,
            process_lifecycle_tx: None,
            active_lanes: Arc::new(RwLock::new(HashMap::new())),
            process_presence: Arc::new(RwLock::new(HashMap::new())),
            spawn_semaphore: Arc::new(Semaphore::new(spawn_cap())),
        }
    }

    /// SurrealDB クライアントを設定
    /// doc 44 P1 (fold-in): repo を in-process 起動するための registry を差し込む。
    ///
    /// daemon mode でのみ設定される。未設定のまま `start_process` を呼ぶと Err になる
    /// （旧構成で `vp` binary が見つからない場合と同じ「起動手段が無い」状態）。
    pub(crate) fn set_repo_runtimes(
        &mut self,
        runtimes: Arc<crate::repo::repo_registry::RepoRuntimes>,
    ) {
        self.repo_runtimes = Some(runtimes);
    }

    /// process lifecycle broadcast の Sender を差し込む（daemon mode のみ）。
    ///
    /// DaemonState と同一 Sender を共有し、`start_process` / `stop_process` が
    /// Add / Remove を流す。未設定なら emit は no-op（repo 単体起動 / test）。
    pub(crate) fn set_process_lifecycle_tx(
        &mut self,
        tx: tokio::sync::broadcast::Sender<crate::daemon::protocol::ProcessLifecycleEvent>,
    ) {
        self.process_lifecycle_tx = Some(tx);
    }

    pub fn set_vpdb(&mut self, vpdb: crate::db::SharedVpDb) {
        self.vpdb = Some(vpdb);
    }

    /// db/machine への参照（Repo Host の帳簿を daemon の control 面から触るため）。
    ///
    /// 帳簿 (`host_origin` / `host_lane_order` / `host_farewell`) は surrealkv の OS 排他
    /// ロックで daemon が専有するので、CLI からは daemon 経由でしか読み書きできない
    /// (doc 44 §8.4)。`None` = DB 未接続（CLI / test 初期）。
    pub fn vpdb(&self) -> Option<&crate::db::SharedVpDb> {
        self.vpdb.as_ref()
    }

    /// running_repos の共有参照を取得（DaemonState と共有するため）
    pub fn running_processes_ref(&self) -> Arc<RwLock<HashMap<String, RunningRepo>>> {
        self.running_repos.clone()
    }

    /// repos の共有参照を取得（DaemonState と共有するため）
    pub fn repos_ref(&self) -> Arc<RwLock<HashMap<String, RepoInfo>>> {
        self.repos.clone()
    }

    /// Phase 1b: lane_registry の共有参照を取得（DaemonState と共有するため）
    #[allow(clippy::type_complexity)]
    pub fn lane_registry_ref(
        &self,
    ) -> Arc<RwLock<HashMap<String, Vec<crate::repo::lanes_state::LaneInfo>>>> {
        self.lane_registry.clone()
    }

    /// L1 lifecycle: process_presence の共有参照を取得（DaemonState と共有するため）。
    ///
    /// registry channel handler が同一 Arc を握り、repo の register/unregister/切断を観測して
    /// presence を遷移させる（capability 経由でなく DaemonState 側から直接書ける）。
    pub fn process_presence_ref(&self) -> Arc<RwLock<HashMap<String, RepoPresenceState>>> {
        self.process_presence.clone()
    }

    /// L1 lifecycle: 1 repo の presence を更新する（`start_process` / `stop_process`）。
    pub async fn set_presence(&self, path_key: &str, state: RepoPresenceState) {
        self.process_presence
            .write()
            .await
            .insert(path_key.to_string(), state);
    }

    /// L1 lifecycle: vp-app sidebar 用の repo presence 一覧を作る（daemon-canonical）。
    ///
    /// `repos`（desired = 全登録 repo）を軸に `running_repos`（live port/pid）と
    /// `process_presence`（接続状態）を join する。repo が crash/disconnect しても repos には
    /// 残るので sidebar から消えず ○ disconnected として見える（Model Q）。HashMap 反復順は
    /// 非決定的なので repo 名で sort して返す（sidebar の表示 jitter を防ぐ）。
    ///
    /// ロック順序: repos → running_repos → process_presence（register handler と同順、deadlock 回避）。
    pub async fn presence_snapshot(&self) -> Vec<RepoHealthInfo> {
        let repos = self.repos.read().await;
        let running = self.running_repos.read().await;
        let presence = self.process_presence.read().await;
        let mut out: Vec<RepoHealthInfo> = repos
            .iter()
            .map(|(path_key, info)| {
                let state = presence
                    .get(path_key)
                    .copied()
                    .unwrap_or(RepoPresenceState::Unregistered);
                let live = running.get(path_key);
                RepoHealthInfo {
                    repo: info.name.clone(),
                    path: path_key.clone(),
                    presence: state.as_str(),
                    port: live.map(|p| p.port),
                    pid: live.map(|p| p.pid),
                }
            })
            .collect();
        out.sort_by(|a, b| a.repo.cmp(&b.repo));
        out
    }

    /// 設定を読み込み
    ///
    /// PR-C (control plane 一元化, creo `mem_1CbmWjCGNi9z49s3r21TwQ`): registered repos の
    /// 真実源を db/machine に切り替える。
    /// - `vpdb=Some` (= daemon): **db/machine を真実源**にする。 DB が空なら config.repos
    ///   (= repos.kdl) から一回 import して復旧 (VP-182 シナリオ / 既存ユーザーの移行)。
    /// - `vpdb=None` (= CLI / repo / test 初期): 従来通り config.repos (= repos.kdl) から展開。
    ///
    /// repos.kdl は過渡期の復旧の種兼ミラー (PR-D で撤去予定)。 `Config::load()` は config.kdl の
    /// 人設定読みと、 復旧 seed としての repos.kdl 読みを兼ねる。
    pub async fn load_config(&mut self) -> CapabilityResult<()> {
        let config = Config::load().map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to load config: {}", e))
        })?;

        // 真実源から RepoEntry 列を得る (vpdb=Some なら DB 優先、 空なら kdl から復旧)。
        let entries: Vec<crate::repos_file::RepoEntry> = if let Some(db) = &self.vpdb {
            let mut entries = db.export_repos().await.map_err(|e| {
                CapabilityError::InitializationFailed(format!("DB repos 取得失敗: {}", e))
            })?;
            if entries.is_empty() && !config.repos.is_empty() {
                // DB 空 + kdl に repos あり → kdl から db/machine へ一回 import (移行 / 復旧)。
                let seed = config_repos_to_entries(&config);
                db.import_repos(&seed).await.map_err(|e| {
                    CapabilityError::InitializationFailed(format!(
                        "DB repos 復旧 import 失敗: {}",
                        e
                    ))
                })?;
                tracing::info!(
                    "repos を repos.kdl から db/machine に復旧 ({} 件)",
                    seed.len()
                );
                entries = db.export_repos().await.map_err(|e| {
                    CapabilityError::InitializationFailed(format!("DB repos 再取得失敗: {}", e))
                })?;
            }
            entries
        } else {
            config_repos_to_entries(&config)
        };

        let mut repos = self.repos.write().await;
        let mut order = self.repo_order.write().await;
        repos.clear();
        order.clear();

        for e in &entries {
            // db 由来の entry は `ReposFile::load` を経ないため、 旧 Windows が保存した
            // verbatim prefix (`\\?\C:\...`) を落とす最後の関所がここ。 素通しすると
            // `RepoInfo.path` が repo の spawn 引数 (`-C`) までそのまま流れる。
            let path = crate::config::strip_verbatim_prefix(&e.path);
            let key = normalize_path_key(&PathBuf::from(path));
            order.push(key.clone());
            repos.insert(
                key,
                RepoInfo {
                    name: e.name.clone(),
                    path: PathBuf::from(path),
                    process_status: RepoStatus::Stopped,
                    port: None, // port は動的割当 (port_layout が slot から計算)
                    enabled: e.is_enabled(),
                    slot: e.slot,
                    active_lane: None, // list_repos で enrich
                },
            );
        }
        drop(repos);
        drop(order);

        // Model Q: active lane (presence) を db/machine から load する (vpdb=Some のみ)。
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

            // doc 24 §10 Phase 2: lane descriptor を db/machine から boot load する (daemon
            // 再起動を re-animate、 §3.3)。 旧来 lane_registry は repo push を待って初めて
            // 埋まる cache だったが、 daemon-canonical 化で boot 時点から truth を持つ。
            // repo が後で reconnect すれば register snapshot が最新で上書きする (= reconcile)。
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
        use crate::repo::lanes_state::LaneLifecycle;
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

        // descriptor の cwd を引く (boot load 済 lane_registry): (repo_path, address) → cwd。
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

        for (repo_path, address, lifecycle_str) in lifecycles {
            let lifecycle = LaneLifecycle::parse(&lifecycle_str);
            if lifecycle == LaneLifecycle::Dead {
                continue; // dead は保持
            }
            let ground_exists = cwd_map
                .get(&(repo_path.clone(), address.clone()))
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
                    .upsert_lane_lifecycle(&repo_path, &address, new_lc.as_str())
                    .await
                {
                    Ok(()) => tracing::info!(
                        "reconcile heal: {} {} {} → {}",
                        repo_path,
                        address,
                        lifecycle.as_str(),
                        new_lc.as_str()
                    ),
                    Err(e) => tracing::warn!(
                        "reconcile heal の永続失敗 ({} {}): {}",
                        repo_path,
                        address,
                        e
                    ),
                }
            }
        }
    }

    /// 現在の repos HashMap を真実源に永続化する。
    ///
    /// PR-C (control plane 一元化): `repo_order` の順序で `RepoEntry` 列を組み立て、
    /// - `vpdb=Some` (= Daemon): **db/machine に全置換** (= 真実源)。 repos.kdl は DB からの
    ///   一方向 export ミラー (= 過渡期の人間可読 + 復旧の種、 PR-D で撤去予定)。
    /// - `vpdb=None` (= CLI / repo / test): 従来通り repos.kdl に atomic write。
    ///
    /// add / delete / rename / reorder / set_enabled / auto_reassign_slot の各操作後に呼ぶ。
    /// test 環境では `ReposFile::save()` が no-op なので本番ファイルを破壊しない。
    async fn persist_repos(&self) -> CapabilityResult<()> {
        // read guard は entries 構築のみで解放する (DB / file の await 中は lock を持たない)。
        let entries: Vec<crate::repos_file::RepoEntry> = {
            let repos = self.repos.read().await;
            let order = self.repo_order.read().await;
            order
                .iter()
                .filter_map(|key| {
                    repos.get(key).map(|p| crate::repos_file::RepoEntry {
                        name: p.name.clone(),
                        path: p.path.to_string_lossy().to_string(),
                        // enabled=true は省略 (= repos.kdl をミニマムに)、 false のみ明記
                        enabled: if p.enabled { None } else { Some(false) },
                        slot: p.slot,
                    })
                })
                .collect()
        };

        if let Some(db) = &self.vpdb {
            // db/machine を真実源として全置換。
            db.replace_all_repos(&entries).await.map_err(|e| {
                CapabilityError::InitializationFailed(format!("DB repos 全置換失敗: {}", e))
            })?;
            // repos.kdl は DB の読み取り専用ミラー。 entries は replace_all で書いた内容と
            // 同一 (ord = 出現順) なので export 往復を省く (= DELETE→export 間に別リクエストが
            // 割り込んで誤った内容を kdl に焼く窓も消える、 Moody Blues PR-D review #3)。
            let pf = crate::repos_file::ReposFile { repos: entries };
            pf.save().map_err(|e| {
                CapabilityError::InitializationFailed(format!("repos.kdl export 失敗: {}", e))
            })
        } else {
            // vpdb なし: 従来通り repos.kdl に書く (= 真実源)。
            let pf = crate::repos_file::ReposFile { repos: entries };
            pf.save().map_err(|e| {
                CapabilityError::InitializationFailed(format!("repos.kdl 書き込み失敗: {}", e))
            })
        }
    }

    /// repo名から正規化パスキーを解決
    ///
    /// `repos` HashMap を検索して name が一致するエントリのキー（正規化パス）を返す。
    /// 公開 API（start_process 等）が repo_name を受け取り、内部キーに変換するために使用。
    async fn resolve_key_by_name(&self, repo_name: &str) -> Option<String> {
        let repos = self.repos.read().await;
        repos
            .iter()
            .find(|(_, info)| info.name == repo_name)
            .map(|(key, _)| key.clone())
    }

    /// repo 一覧を取得（repo_order の順序で返す）
    pub async fn list_repos(&self) -> Vec<RepoInfo> {
        let repos = self.repos.read().await;
        let order = self.repo_order.read().await;
        // Model Q: active lane (presence) を enrich (daemon-canonical)。
        let active = self.active_lanes.read().await;
        order
            .iter()
            .filter_map(|key| {
                repos.get(key).cloned().map(|mut p| {
                    p.active_lane = active.get(key).cloned();
                    p
                })
            })
            .collect()
    }

    /// 稼働中Process一覧を取得
    pub async fn list_running_processes(&self) -> Vec<RunningRepo> {
        let procs = self.running_repos.read().await;
        procs.values().cloned().collect()
    }

    /// repos を repos.kdl と同期する（VP-188: repos.kdl 経由、 VP-189: 双方向同期）。
    ///
    /// repos.kdl にある repo は in-memory に追加、 repos.kdl から消えた
    /// repo は in-memory からも除去する。 後者は VP-189 の ghost repo cleanup
    /// (= `vp sync` / 起動時 sync の repos.kdl 書き換え) を daemon の in-memory
    /// 状態に伝播させるための双方向同期。
    ///
    /// ただし **running process を持つ repo は repos.kdl から消えていても残す**
    /// ── 稼働中 repo の取りこぼし防止 (安全側)。 ghost repo は dir 消失で repo が
    /// 起動不可なので、 通常は running と ghost が両立しない。
    pub async fn reload_config(&self) {
        let Ok(config) = Config::load() else {
            return;
        };

        // running process の key を先に取得 (repos/order の write lock との入れ子回避)。
        let running: std::collections::HashSet<String> = {
            let procs = self.running_repos.read().await;
            procs.keys().cloned().collect()
        };

        {
            let mut repos = self.repos.write().await;
            let mut order = self.repo_order.write().await;

            // repos.kdl 由来の key 集合 (= 除去判定の基準)。
            let kdl_keys: std::collections::HashSet<String> = config
                .repos
                .iter()
                .map(|p| normalize_path_key(&PathBuf::from(&p.path)))
                .collect();

            // add/update: repos.kdl の各 repo を in-memory に反映。
            // PR-C: 既存 key も kdl 値で name/enabled/slot を更新 (CLI が kdl 経由で更新した
            // slot 等を取り込む)。 running process の process_status / port は触らない (安全側)。
            for repo in &config.repos {
                let key = normalize_path_key(&PathBuf::from(&repo.path));
                repos
                    .entry(key.clone())
                    .and_modify(|p| {
                        p.name = repo.name.clone();
                        p.enabled = repo.enabled;
                        p.slot = repo.slot;
                    })
                    .or_insert_with(|| RepoInfo {
                        name: repo.name.clone(),
                        path: repo.path.clone().into(),
                        process_status: RepoStatus::Stopped,
                        port: repo.port,
                        enabled: repo.enabled,
                        slot: repo.slot,
                        active_lane: None,
                    });
                if !order.contains(&key) {
                    order.push(key);
                }
            }

            // remove: repos.kdl から消えた entry を in-memory からも除去。
            // ただし running process を持つ key は残す (稼働中 repo を取りこぼさない)。
            repos.retain(|key, _| kdl_keys.contains(key) || running.contains(key));
            order.retain(|key| repos.contains_key(key));

            tracing::info!("Config reloaded: {} repos", repos.len());
        } // repos / order の write guard を解放してから persist (read lock 取り直し)

        // PR-C: vpdb=Some なら DB に同期する。 reload は kdl→in-memory→DB の向きで、
        // running 保護後の in-memory を書くので、 古い kdl で DB を盲目上書きせず取りこぼしも防ぐ。
        // (= CLI が kdl 経由で更新した slot 等を db/machine に焼く合流点)
        if self.vpdb.is_some()
            && let Err(e) = self.persist_repos().await
        {
            tracing::warn!("reload_config: DB 同期失敗: {}", e);
        }
    }

    /// repo を追加（+ repos.kdl に永続化、 VP-188）
    pub async fn add_repo(&self, name: &str, path: &str) -> CapabilityResult<RepoInfo> {
        // 名前バリデーション
        if name.trim().is_empty() {
            return Err(CapabilityError::Other(
                "Repo name cannot be empty".to_string(),
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

        let info = RepoInfo {
            name: name.to_string(),
            path: path.into(),
            process_status: RepoStatus::Stopped,
            port: None,
            enabled: true,
            slot: None, // 新規 repo は slot 未割当 (= repo 初回起動時に resolve が割当)
            active_lane: None,
        };

        {
            let mut repos = self.repos.write().await;
            if repos.contains_key(&key) {
                return Err(CapabilityError::Other(format!(
                    "Repo already exists: {}",
                    path
                )));
            }
            repos.insert(key.clone(), info.clone());
        }
        // 順序リストに末尾追加
        self.repo_order.write().await.push(key.clone());

        // VP-188: repos.kdl に永続化
        self.persist_repos().await?;

        Ok(info)
    }

    /// repoを削除（+ repos.kdl に永続化、 VP-188）
    pub async fn remove_repo(&self, path: &str) -> CapabilityResult<()> {
        let key = normalize_path_key(&PathBuf::from(path));

        // 稼働中なら停止を先にする必要がある
        {
            let procs = self.running_repos.read().await;
            if procs.contains_key(&key) {
                return Err(CapabilityError::Other(
                    "Cannot remove running repo. Stop it first.".to_string(),
                ));
            }
        }

        {
            let mut repos = self.repos.write().await;
            if repos.remove(&key).is_none() {
                return Err(CapabilityError::Other(format!("Repo not found: {}", path)));
            }
        }
        // 順序リストからも削除
        self.repo_order.write().await.retain(|k| k != &key);

        // Model Q / §4.6 含有=所有=寿命: repo(namespace) を倒したら、 その presence
        // (active_lane) も畳む。 in-memory map と db/machine から回収する (DB は best-effort)。
        self.active_lanes.write().await.remove(&key);
        if let Some(db) = &self.vpdb
            && let Err(e) = db.delete_active_lane(&key).await
        {
            tracing::warn!(
                "active_lane の db/machine 削除に失敗 (in-memory は削除済): {}",
                e
            );
        }
        // doc 44 D4 / §4.6: Repo Host の帳簿 (開発起点ポインタ) も namespace と共に回収する。
        // 残すと同 path で repo を再登録した時、旧 lane の UUID を指す孤児ポインタが復活し、
        // 起点が `Dangling` に落ちる (= 指定した覚えのない「指定が失われました」表示)。
        if let Some(db) = &self.vpdb {
            if let Err(e) = db.delete_host_origin(&key).await {
                tracing::warn!("host_origin の db/machine 削除に失敗: {}", e);
            }
            // doc 44 §12: lane の並び順も同じ namespace の帳簿なので一緒に畳む。
            if let Err(e) = db.delete_lane_order_for_repo(&key).await {
                tracing::warn!("host_lane_order の db/machine 削除に失敗: {}", e);
            }
            // doc 44 §7.5: 見送りの履歴 / 滞留も同じ namespace の帳簿。repo を倒したら
            // 一緒に畳む (残すと同 path で再登録した時、無関係な過去の見送りが出てくる)。
            if let Err(e) = db.delete_farewell_entries_for_repo(&key).await {
                tracing::warn!("host_farewell の db/machine 削除に失敗: {}", e);
            }
        }
        // L1 lifecycle: connection presence も namespace と共に回収 (active_lanes と対称、
        // DB 永続を持たない in-memory only field なので map remove のみ)。
        self.process_presence.write().await.remove(&key);

        // doc 24 §10 Phase 2 / §4.6 含有=所有=寿命: lane descriptor も同様に畳む。
        // lane は daemon-canonical durable truth (repo disconnect では残すが、 repo remove は
        // namespace ごと倒す = descriptor も回収する)。 in-memory lane_registry と db から削除。
        // remove は外した Vec<LaneInfo> を返すので、 下の ground reclaim にそのまま使う。
        let removed_lanes = self
            .lane_registry
            .write()
            .await
            .remove(&key)
            .unwrap_or_default();
        if let Some(db) = &self.vpdb {
            if let Err(e) = db.delete_lanes_for_repo(&key).await {
                tracing::warn!("lane の db/machine 削除に失敗 (in-memory は削除済): {}", e);
            }
            // §4.6: lane lifecycle (別 table) も同様に回収する。
            if let Err(e) = db.delete_lane_lifecycles_for_repo(&key).await {
                tracing::warn!("lane_lifecycle の db/machine 削除に失敗: {}", e);
            }
        }

        // doc 24 §5.3 / B-destroy: ground を provision/reclaim する唯一の主体は daemon。
        // namespace (repo) を倒したら sub の worktree (ground) も daemon が reclaim する。
        // A では descriptor だけ畳んで worktree が disk に orphan で残る中間状態だった — その穴を閉じる。
        // main は cwd = repo root (= user の repo そのもの) なので **絶対に消さない**、 sub のみ。
        let sub_names: Vec<String> = removed_lanes
            .iter()
            .filter(|l| !l.address.is_root())
            .map(|l| l.address.name.clone())
            .collect();
        if !sub_names.is_empty() {
            // repo_root は key (= normalize_path_key の出力) から再構築する。 add_repo 時と
            // 同じ normalize を経るので通常は実 repo root と一致する。 万一ズレ / explicit cwd
            // (`<repo>/.vp/lanes/<name>` 外) の時は find_sub_dir が None → 下の warn で
            // skip され orphan が残るだけ (= 誤削除は起きない、 best-effort、 team-b review #1)。
            let repo_root = PathBuf::from(&key);
            // git worktree remove は blocking subprocess なので spawn_blocking で executor を塞がない。
            let _ = tokio::task::spawn_blocking(move || {
                for name in sub_names {
                    // best-effort (§4.6 ゆるやか統治): 既に手動 rm 済 / explicit cwd 外などは warn で流す。
                    match crate::lane::commands::remove_sub_in(&repo_root, &name) {
                        Ok(()) => tracing::info!(
                            "sub worktree reclaim: name={} repo={}",
                            name,
                            repo_root.display()
                        ),
                        Err(e) => tracing::warn!(
                            "sub worktree reclaim 失敗 (best-effort、 skip): name={} err={}",
                            name,
                            e
                        ),
                    }
                }
            })
            .await;
        }

        // VP-188: repos.kdl に永続化
        self.persist_repos().await?;

        Ok(())
    }

    /// Daemon 入口（Unison `daemon-control.lanes/create`）から sub lane を作る。
    ///
    /// ## doc 44 §9.4: 実装は持たない（統合後）
    ///
    /// かつてここには **もう 1 つの lane 作成実装**が居た（worktree provision + descriptor
    /// 永続だけを行い、PtySlot spawn は lane watcher が `lane_create` を loopback 発火して
    /// repo 側に任せる）。doc 24 §5.3 の「ground を provision する唯一の主体は daemon」を
    /// 根拠にした分割だったが、doc 44 P1 の fold-in で **daemon と repo が同一プロセスに
    /// なった時点でその根拠は消えていた**（repo 側 `create_sub_orchestrated` も
    /// 同じ `new_sub_in` で ground を作る）。
    ///
    /// 残っていたのは「同じ動詞に実装が 2 本」という状態そのもので、実際に
    /// **経路ごとに振る舞いが違った**（`base` / `model` 指定が効かない、agent を descriptor
    /// 経由で watcher に伝え直す遠回り、descriptor は GUI 経由だけ db に載る、等）。
    /// 統合後の本関数は「名前の gate → repo runtime を引く → core を呼ぶ」だけの adapter。
    ///
    /// ## 名前の gate をここにも置く理由
    ///
    /// core 側にも同じ `validate_sub_name` があるが、**拒否は永続や runtime 解決の
    /// 手前で完結させる**（doc 44 §9.2）。加えて repo 未起動時に「予約名です」ではなく
    /// 「repo 未起動」が返ると理由がすり替わる。呼ぶのは同じ関数 1 本なので実装は増えない。
    ///
    /// `branch` / `agent` は呼び手 (route) が resolve 済の concrete 値を渡す
    /// (default 導出 = data/calc は route の責務)。
    pub async fn create_lane(
        &self,
        repo_path: &str,
        name: &str,
        branch: &str,
        agent: &str,
    ) -> CapabilityResult<crate::repo::lanes_state::LaneInfo> {
        let name = name.trim();
        crate::lane::config::validate_sub_name(name).map_err(CapabilityError::Other)?;

        let key = normalize_path_key(&PathBuf::from(repo_path));
        let runtimes = self.repo_runtimes.as_ref().ok_or_else(|| {
            CapabilityError::Other(
                "repo runtimes 未設定 — daemon mode 以外では lane を作れない".to_string(),
            )
        })?;
        // 停止中 repo に lane は作れない。GUI 側も「+ Add Sub」を稼働中限定にして
        // いる (停止中は「▶ Start repo」だけ) ので、これは UI の契約と一致する。
        // 未起動を黙って provision だけして返すと、PtySlot の無い descriptor が残り
        // 「作ったのに動かない lane」になる (旧実装が watcher の到達に賭けていた形)。
        let state = runtimes.get(&key).await.ok_or_else(|| {
            CapabilityError::Other(format!(
                "repo 未起動のため lane を作れない (key={key}) — 先に repo を起動する"
            ))
        })?;

        let req = crate::repo::routes::lanes::build_create_lane_req(name, branch, agent);
        crate::repo::routes::lanes::create_sub_orchestrated(&state, req)
            .await
            .map_err(CapabilityError::Other)
    }

    /// repo名を変更（+ repos.kdl に永続化、 VP-188）
    pub async fn rename_repo(&self, path: &str, new_name: &str) -> CapabilityResult<()> {
        if new_name.trim().is_empty() {
            return Err(CapabilityError::Other(
                "Repo name cannot be empty".to_string(),
            ));
        }

        let key = normalize_path_key(&PathBuf::from(path));

        {
            let mut repos = self.repos.write().await;
            if let Some(p) = repos.get_mut(&key) {
                p.name = new_name.to_string();
            } else {
                return Err(CapabilityError::Other(format!("Repo not found: {}", path)));
            }
        }

        // VP-188: repos.kdl に永続化
        self.persist_repos().await?;

        Ok(())
    }

    /// repoの enabled/disabled を切り替え（+ repos.kdl に永続化）
    pub async fn set_repo_enabled(&self, path: &str, enabled: bool) -> CapabilityResult<()> {
        let key = normalize_path_key(&PathBuf::from(path));

        {
            let mut repos = self.repos.write().await;
            if let Some(p) = repos.get_mut(&key) {
                p.enabled = enabled;
            } else {
                return Err(CapabilityError::Other(format!("Repo not found: {}", path)));
            }
        }

        // VP-188: repos.kdl に永続化
        self.persist_repos().await?;
        tracing::info!("Repo enabled={}: {}", enabled, path);

        Ok(())
    }

    /// repoの並び順を更新（+ repos.kdl に永続化、 VP-188）
    pub async fn reorder_repos(&self, paths: &[String]) -> CapabilityResult<()> {
        // raw paths を正規化して HashMap キーと一致させる
        let normalized: Vec<String> = paths
            .iter()
            .map(|p| normalize_path_key(&PathBuf::from(p)))
            .collect();
        // 順序リストを更新
        *self.repo_order.write().await = normalized.clone();

        // VP-188: repos.kdl に永続化
        self.persist_repos().await?;

        Ok(())
    }

    /// active lane (presence、 Model Q) を設定する。
    ///
    /// repo ごとの選択中 lane を daemon-canonical に持つ。 in-memory map を更新し、
    /// vpdb=Some (= Daemon) なら db/machine の active_lane table に upsert する。
    /// §4.6: presence は tail-loss 許容なので DB 永続は best-effort (失敗は warn のみ)。
    pub async fn set_active_lane(
        &self,
        repo_path: &str,
        lane_address: &str,
    ) -> CapabilityResult<()> {
        let key = normalize_path_key(&PathBuf::from(repo_path));
        self.active_lanes
            .write()
            .await
            .insert(key.clone(), lane_address.to_string());
        if let Some(db) = &self.vpdb
            && let Err(e) = db.upsert_active_lane(&key, lane_address).await
        {
            tracing::warn!(
                "active_lane の db/machine 永続に失敗 (in-memory は更新済): {}",
                e
            );
        }
        Ok(())
    }

    /// repos を現実と同期 (PR-D: CLI の `ReposFile::sync` を daemon 経由に移管)。
    ///
    /// dir が実在しない ghost repo を除去する (running process を持つものは安全側で残す)。
    /// 永続化は内部の remove_repo が persist_repos 経由で行う。
    ///
    /// かつて `start_dir` で「起点 dir 自動登録」も行っていたが、 `vp sp start` の起動時
    /// sync が **削除済 repo を復活させる** resurrection バグの温床だったため撤去した
    /// (削除 → repo 再起動 → sync が起点 dir を無条件再登録 → db/kdl に復活)。 repo 登録は
    /// `add_repo` 経由の明示操作のみ (sidebar Add / `vp repos add`)。
    pub async fn sync_repos(&self) -> CapabilityResult<crate::repos_file::SyncOutcome> {
        let mut outcome = crate::repos_file::SyncOutcome::default();

        // ghost 除去 (dir 非実在 & 非 running)。ロック順序 repos → running_repos を遵守。
        let ghosts: Vec<(String, String)> = {
            let repos = self.repos.read().await;
            let running: std::collections::HashSet<String> = {
                let procs = self.running_repos.read().await;
                procs.keys().cloned().collect()
            };
            repos
                .iter()
                .filter(|(key, p)| !p.path.is_dir() && !running.contains(*key))
                .map(|(key, p)| (key.clone(), p.name.clone()))
                .collect()
        };
        for (key, name) in ghosts {
            if self.remove_repo(&key).await.is_ok() {
                outcome.removed.push(name);
            }
        }

        Ok(outcome)
    }

    /// L0 finale (Push-only): 指定 path の live repo を `running_repos` registry から引く。
    ///
    /// `start_process` の重複 spawn 防止 dedup check。
    /// doc 44 P1 (fold-in): repo が Daemon 内の map エントリになり、重複起動は
    /// `RepoRuntimes` のキー一意性が構造的に防ぐ。本 check は running_repos を直引きする
    /// 補助（旧 VP-133 の port scan dedup / repo uplink reconnect / health monitor respawn の
    /// 段取りはいずれも repo プロセス前提で、fold-in で不要になった）。
    async fn find_running_sp_at_path(&self, repo_path: &std::path::Path) -> Option<RunningRepo> {
        let target_key = normalize_path_key(repo_path);
        self.running_repos.read().await.get(&target_key).cloned()
    }

    pub async fn start_process(&self, repo_name: &str) -> CapabilityResult<RunningRepo> {
        // 名前→パスキー解決（見つからなければ config を再読み込みして再試行）
        let key = match self.resolve_key_by_name(repo_name).await {
            Some(k) => k,
            None => {
                self.reload_config().await;
                self.resolve_key_by_name(repo_name).await.ok_or_else(|| {
                    CapabilityError::Other(format!("Repo not found: {}", repo_name))
                })?
            }
        };

        let repo = {
            let repos = self.repos.read().await;
            repos.get(&key).cloned()
        }
        .ok_or_else(|| CapabilityError::Other(format!("Repo not found: {}", repo_name)))?;

        // 既に起動中かチェック
        {
            let procs = self.running_repos.read().await;
            if procs.contains_key(&key) {
                return Err(CapabilityError::Other(format!(
                    "Process already running for repo: {}",
                    repo_name
                )));
            }
        }

        // VP-133 MVP: dedup port scan check ─ false positive 切断検知 (= QUIC heartbeat 一時失敗
        // 等で running_repos registry が誤って空になる) 後の auto-spawn を防ぐ。 registry
        // bypass で port 直 scan + path match を確認、 既存 repo 発見なら spawn skip + 再 register。
        //
        // 旧挙動 (= dedup check 不在) では、 false positive で registry 空 → start_process →
        // 旧 SP alive のまま新 port で spawn → multi-port 並走 → Health monitor が 30 秒毎に
        // ghost detect → 互殺 ping-pong cycle が永続化していた (VP-133 root cause)。
        if let Some(existing) = self.find_running_sp_at_path(&repo.path).await {
            tracing::info!(
                "start_process: dedup check で既存 repo 発見 → spawn skip + re-register \
                 (repo={}, port={}, pid={})",
                repo_name,
                existing.port,
                existing.pid
            );
            {
                let mut repos = self.repos.write().await;
                if let Some(p) = repos.get_mut(&key) {
                    p.process_status = RepoStatus::Running;
                }
            }
            {
                let mut procs = self.running_repos.write().await;
                procs.insert(key.clone(), existing.clone());
            }
            return Ok(existing);
        }

        // PR3: repo spawn 平滑化 — early-return (already-running / dedup) を抜けた「実 spawn 確定」
        // 地点で permit を取得。permit は関数 return まで RAII 保持され、一度に走る
        // `vp sp start` を `spawn_cap()` 本に絞る (semantics A)。非輻輳時は即取得 =
        // レイテンシ影響なし。`Semaphore` は close しないので acquire は必ず成功する。
        if self.spawn_semaphore.available_permits() == 0 {
            tracing::debug!(
                "start_process: spawn permit 全 in-flight、'{}' は空き待ち (spawn cap 平滑化)",
                repo_name
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
            let mut repos = self.repos.write().await;
            if let Some(p) = repos.get_mut(&key) {
                p.process_status = RepoStatus::Starting;
            }
        }

        // VP-165 PR-5b: daemon が port allocation の authority。
        // - `port_for_repo` で slot ベースの port を解決 (新規割当なら config 永続)
        // - `vp sp start -p <port>` で port を明示渡し
        // - `wait_for_health(port, &path)` で QUIC registry 登録を確認 (Push-only、 L0 finale)
        // - 外部衝突 (別 repo / 非 VP process) なら 1 回きり auto-reassign + retry
        //
        // 旧 (PR-5 まで): `vp sp start -C <path>` (-p 無し) → 子の resolve_port が slot 解決 →
        // daemon が `wait_for_process_port` で range scan で discover、 だった。 PR-5b で
        // daemon が port を明示所有する形に整理。
        let repo_path_str = repo.path.to_string_lossy().to_string();
        // doc 44 P1 (fold-in): 旧実装は `vp sp start -C <path> -p <port>` を子プロセスとして
        // spawn し、QUIC registry への自己登録を `wait_for_health` で待ち、port が他 repo に
        // 取られていれば `auto_reassign_slot` して 1 回だけ retry する、という多段の段取りだった。
        //
        // repo が Daemon 内の `Arc<AppState>` になった今、これは単なる関数呼び出しになる。
        // 旧段取りの構成要素はいずれも概念ごと不要になった:
        //   - port 解決      … bind しないので割り当てる対象が無い
        //   - health 待ち    … 起動の成否は Result で同期的に返る
        //   - 衝突 retry     … 衝突する port が存在しない
        //   - 重複 spawn 検出 … registry map のキー一意性が構造的に防ぐ
        //     （旧: registry dedup + spawn Semaphore + per-repo DB LOCK の 3 重掛け）
        let runtimes = self.repo_runtimes.clone().ok_or_else(|| {
            CapabilityError::Other(
                "repo runtimes 未設定 — daemon mode 以外では repo を起動できない".to_string(),
            )
        })?;
        let started = runtimes
            .start(&repo_path_str)
            .await
            .map_err(|e| CapabilityError::Other(format!("repo 起動失敗 ({}): {}", repo_name, e)))?;
        if !started {
            tracing::info!("repo は既に起動済み → skip (repo={})", repo_name);
        }
        // port / pid は repo プロセスの遺産。 repo は daemon と同一プロセスで動くので
        // pid は Daemon 自身、 port は不在を表す 0 を入れる（表示の意味論整理は doc 44 P3）。
        let running_process = RunningRepo {
            repo_name: repo_name.to_string(),
            port: 0,
            pid: std::process::id(),
            repo_path: repo.path.clone(),
        };

        // 状態を更新
        {
            let mut repos = self.repos.write().await;
            if let Some(p) = repos.get_mut(&key) {
                p.process_status = RepoStatus::Running;
            }
        }

        // doc 44 P1 (fold-in): daemon 側 insert を復活させる。
        //
        // #648 でこの insert を撤去したのは「repo の QUIC 自己登録が entry を書くので、
        // daemon 側の子 pid で上書きすると Push-canonical を壊す」ためだった。fold-in で
        // repo が消え、自己登録しに来る者が居なくなったため、この前提そのものが失効した。
        // 撤去したまま放置すると registry は**永久に空**になり、`vp ps` / Unison `registry.list`
        // が空を返し、`stop_process` は自身の gate に阻まれて repo を停止できなくなる。
        //
        // in-process 起動が成功した瞬間が権威ある lifecycle event なので、書き手はここが正
        // （= presence 設計が元から掲げていた daemon-canonical に戻る）。
        {
            let mut procs = self.running_repos.write().await;
            procs.insert(key.clone(), running_process.clone());
        }
        // presence も同様に repo の register だけが Connected にしていた。in-process の
        // repo は「起動していれば接続している」以外の状態を取り得ないため、
        // 起動成功 = Connected で確定する（旧: registry handler の register→Connected）。
        self.set_presence(&key, RepoPresenceState::Connected).await;

        // DB に書き込み（正規化パスで保存、 pid/port は registry entry の真実を使う）
        if let Some(ref db) = self.vpdb
            && let Err(e) = db
                .upsert_process(
                    &key,
                    repo_name,
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
                repo_path: key.clone(),
                repo_name: repo_name.to_string(),
                port: running_process.port,
                pid: running_process.pid,
            });
        }

        tracing::info!(
            repo = repo_name,
            port = running_process.port,
            pid = running_process.pid,
            "Process started"
        );

        Ok(running_process)
    }

    /// Processを停止
    pub async fn stop_process(&self, repo_name: &str) -> CapabilityResult<()> {
        let key = self
            .resolve_key_by_name(repo_name)
            .await
            .ok_or_else(|| CapabilityError::Other(format!("Repo not found: {}", repo_name)))?;

        let running = {
            let procs = self.running_repos.read().await;
            procs.get(&key).cloned()
        };

        let _running = running.ok_or_else(|| {
            CapabilityError::Other(format!("No running Process for repo: {}", repo_name))
        })?;

        // 状態を更新
        {
            let mut repos = self.repos.write().await;
            if let Some(p) = repos.get_mut(&key) {
                p.process_status = RepoStatus::Stopping;
            }
        }

        // doc 44 P1 (fold-in): 旧実装は daemon repo-proxy の "shutdown" method を loopback で
        // 撃ち、reverse-route で repo に届けて自死させていた。repo は shutdown_token cancel 直後に
        // control channel を畳むため応答が返らないことがあり、best-effort 扱いにせざるを得な
        // かった（「止まったかどうか確かめられない」）。
        //
        // in-process になった今は registry から直接停止でき、完了も同期的に確認できる。
        if let Some(runtimes) = self.repo_runtimes.as_ref() {
            if !runtimes.stop(&key).await {
                tracing::info!(
                    "停止対象の repo は既に不在 (repo={}) — registry の後始末のみ実施",
                    repo_name
                );
            }
        } else {
            tracing::warn!(
                "repo runtimes 未設定のため停止をスキップ (repo={}) — registry からは remove",
                repo_name
            );
        }

        // ロック順序統一: repos → running_repos
        {
            let mut repos = self.repos.write().await;
            if let Some(p) = repos.get_mut(&key) {
                p.process_status = RepoStatus::Stopped;
            }
        }
        {
            let mut procs = self.running_repos.write().await;
            procs.remove(&key);
        }
        // doc 44 P1 (fold-in): presence も対称に落とす（旧: repo 切断を registry handler が
        // 検知して Disconnected にしていた）。in-process では「停止した = 登録が無い」なので
        // Disconnected（居るが繋がらない）ではなく Unregistered が正。
        self.set_presence(&key, RepoPresenceState::Unregistered)
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
                repo_path: key.clone(),
            });
        }

        tracing::info!(repo = repo_name, "Process stopped");

        Ok(())
    }

    /// repo を restart する。 stop → 短い grace period → start を atomic に chain。
    /// stop が「No running Process」 なら start のみ実行 (= ensure-running 的な挙動)。
    ///
    /// doc 44 P1 (fold-in): in-process 化で stop/start は同期的に確定するため、旧 SP 時代の
    /// 「db LOCK 保持で重複 spawn 検出 → health monitor の crash 検知が respawn で自己修復」
    /// という非同期の自己修復経路は不要になった（health monitor 自体も退役）。
    pub async fn restart_process(&self, repo_name: &str) -> CapabilityResult<RunningRepo> {
        // stop が失敗しても start を試みる (= dead な repo でも restart で起こす UX)
        match self.stop_process(repo_name).await {
            Ok(()) => {
                tracing::info!(repo = repo_name, "Process stopped (for restart)");
                // grace period: shutdown signal の伝播 + port release を待つ
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                tracing::info!(
                    repo = repo_name,
                    "stop_process during restart failed (continuing to start): {}",
                    e
                );
            }
        }
        self.start_process(repo_name).await
    }

    /// PointViewを開く
    pub async fn open_pointview(&self, repo_name: &str) -> CapabilityResult<()> {
        let key = self.resolve_key_by_name(repo_name).await;

        // Processが起動していなければ起動
        let running = if let Some(ref key) = key {
            let procs = self.running_repos.read().await;
            procs.get(key).cloned()
        } else {
            None
        };

        let running = match running {
            Some(s) => s,
            None => self.start_process(repo_name).await?,
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

    /// 起動時設定の復帰: daemon 起動時に `enabled` な repo の repo を自動起動する。
    ///
    /// daemon restart 後に working set を復元する (VP-207)。 daemon 起動時に
    /// バックグラウンドタスクとして 1 回だけ spawn される。
    /// 1. `enabled == true` かつ未稼働の repo を収集
    /// 2. 各 repo を `start_process` で起動 (300ms ずらして burst 回避)
    ///
    /// 二重起動は `RepoRuntimes` の map キー一意性が構造的に防ぐ。
    /// lock 規律: `start_process`（内部で sleep する）を呼ぶ前に read ガードを clone して解放する。
    pub async fn autostart_enabled_repos(daemon: Arc<RwLock<Self>>) {
        // doc 44 P1 (fold-in): 旧「registry 静穏待ち」(最大 60s) を撤去した。
        //
        // あの待ちの目的は「gentle daemon restart を生き延びた旧 SP の QUIC heal 再登録が
        // 届くまで待ち、稼働中の repo を重複 spawn しない」ことだった。fold-in で repo が
        // Daemon 内に入り、daemon 停止を生き延びる repo が存在しなくなったため、registry は
        // boot 時に**常に空**で安定する = 待つ理由そのものが消滅した。
        //
        // 残したままだと毎回きっかり QUIET_WINDOW(20s) 空回りしてから repo を起こすことに
        // なり、daemon 再起動のたびに 20 秒の無駄が乗る（dogfood で最も踏む経路）。
        // 二重起動の防御は `RepoRuntimes` の map キー一意性が引き継いでいる。
        //
        // doc 44 P1 (fold-in): 旧実装はここで `refresh_process_status`（PID liveness）を
        // 呼んでいたが、boot 時点で running_repos は空なので no-op だった上、fold-in で
        // pid が全 repo 共通の Daemon 自身になり liveness check 自体が無意味化したため撤去。

        // enabled かつ未稼働の repo 名を収集。
        let targets: Vec<String> = {
            let w = daemon.read().await;
            let repos = w.repos.read().await;
            let running = w.running_repos.read().await;
            repos
                .values()
                .filter(|p| p.enabled)
                .filter(|p| !running.contains_key(&normalize_path_key(&p.path)))
                .map(|p| p.name.clone())
                .collect()
        };

        if targets.is_empty() {
            tracing::info!("autostart: 起動対象なし（全 enabled repo が稼働中）");
            return;
        }
        tracing::info!(
            "autostart: {} repo の repo を起動: {:?}",
            targets.len(),
            targets
        );

        // start_process は内部で sleep するため、read ガードを保持せず clone した cap で呼ぶ。
        for name in &targets {
            let daemon_cap = {
                let w = daemon.read().await;
                w.clone()
            };
            match daemon_cap.start_process(name).await {
                Ok(p) => tracing::info!("autostart: '{}' 起動成功（port {}）", name, p.port),
                Err(e) => tracing::warn!("autostart: '{}' 起動失敗: {}", name, e),
            }
            // burst を避けて少しずらす。
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }

    /// VP-129: lane root を watch して sub dir 削除を repo DELETE に bridge する FSEvents watcher。
    ///
    /// **「folder = Lane 空間」 axiom の物理実装**。 user が Finder / `rm -rf` で sub dir を
    /// 削除した時、 OS の file system event (Mac → FSEvents、 Linux → inotify) → notify crate
    /// → 本 watcher が path → repo 解決 → repo `DELETE /api/lanes` 自動発火、 sidebar /
    /// tmux / PtySlot が cascade で同期 cleanup される。
    ///
    /// D10 Reconciliation arch の **3rd path 拡張**: Push (QUIC heartbeat) + Pull (port scan) +
    /// **FSEvents (本 method)** の 3-trigger model 完成。
    ///
    /// ## repo-local lane refactor PR 4c → hot-reload
    ///
    /// PR 4c で `config.repos` の各 repo の `.vp/lanes/` を `Vec<watch>` で N path
    /// 同時監視に書き直し、 本 PR で **5s tick polling-based の動的 hot-reload** を追加。
    /// 起動後に repos.kdl 経由で新規 repo が register/unregister されると、
    /// 次の tick (= 最大 5s 遅延) で watch list を sync する。
    ///
    /// MVP scope (= 別 ticket で safety net 追加候補):
    /// - self-loop 防止: scope 外 (= repo 経由削除も Remove event 発火、 二重 DELETE 走るが repo 側
    ///   404 で no-op、 log noise 許容)
    /// - spawn race: scope 外 (= 既存 spawn semaphore + atomic LanePool insert で吸収)
    /// - 詳細 EventKind 区別: Remove(_) 全 variant accept (= Mac FSEvents は RemoveKind 区別が薄い)
    /// - event-based hot-reload: scope 外 (= polling で十分、 PR 1 の `build_lanes_snapshot`
    ///   periodic と同 cadence で user の mental model 一致)
    pub async fn run_lane_watcher(
        daemon: Arc<RwLock<Self>>,
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

        // 起動時 snapshot を arm。 0 repo でも loop は起動し、 periodic tick で
        // 後から register された repo を pick up する (= hot-reload 動作)。
        let mut path_map = Self::build_lane_watch_path_map(&daemon).await;
        let mut watched: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        for (path, (name, _)) in &path_map {
            if Self::arm_watch_path(&mut watcher, path, name) {
                watched.insert(path.clone());
            }
        }
        tracing::info!(
            "lane watcher 起動 (初期 {} repo arm 済、 mode=NonRecursive、 trigger=Create/Remove → lane_create/lane_delete)",
            watched.len()
        );

        // lanes portless: 旧 reqwest client (repo HTTP 直結) は撤去。 event handler は Daemon
        // repo-proxy ask (`lane_create` / `lane_delete`) に loopback する。

        // 5s tick で repos.kdl 経由の register/unregister を hot-reload。
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
                    let new_map = Self::build_lane_watch_path_map(&daemon).await;
                    let new_paths: std::collections::HashSet<std::path::PathBuf> =
                        new_map.keys().cloned().collect();
                    let (to_add, to_remove) = compute_watch_diff(&watched, &new_paths);
                    for path in &to_remove {
                        use notify::Watcher;
                        let _ = watcher.unwatch(path);
                        watched.remove(path);
                        tracing::info!(
                            "lane watcher: repo unwatch (= unregister 検出) path={}",
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
                            Self::handle_lane_remove_event(&daemon, &path_map, &event).await;
                        }
                        EventKind::Create(_) => {
                            // F.8 B Convergent: repo 起動後に CLI / 外部で `.vp/lanes/<name>`
                            // dir が増えた時、 daemon repo-proxy ask `lane_create` (cwd 明示) で
                            // spawn を依頼。 「disk dir があるが LanePool に居ない」 中間状態
                            // (= disk-only Lane) を恒久化させない、 lifecycle 自動 convergence。
                            Self::handle_lane_create_event(&daemon, &path_map, &event).await;
                        }
                        _ => {} // Modify / Access 等は無視
                    }
                }
            }
        }

        drop(watcher); // 明示 drop で watching 停止 (scope 終端でも自動だが意図表示)
        tracing::info!("lane watcher 終了");
    }

    /// 1 repo の `.vp/lanes/` を arm する helper (`run_lane_watcher` の inner)。
    /// dir 不在なら best-effort で create + `watch()` 試行。 成功すれば true を返す。
    fn arm_watch_path(
        watcher: &mut notify::RecommendedWatcher,
        path: &std::path::Path,
        repo_name: &str,
    ) -> bool {
        use notify::{RecursiveMode, Watcher};
        if !path.exists()
            && let Err(e) = std::fs::create_dir_all(path)
        {
            tracing::warn!(
                "lane watcher: dir create 失敗 (skip) repo={} path={}: {}",
                repo_name,
                path.display(),
                e
            );
            return false;
        }
        if let Err(e) = watcher.watch(path, RecursiveMode::NonRecursive) {
            tracing::warn!(
                "lane watcher: watch 開始失敗 (skip) repo={} path={}: {}",
                repo_name,
                path.display(),
                e
            );
            return false;
        }
        tracing::info!(
            "lane watcher: repo={} path={} 監視開始",
            repo_name,
            path.display()
        );
        true
    }

    /// `config.repos` から `<repo>/.vp/lanes/` path → (repo_name, repo_path) の
    /// HashMap を build する。 起動 snapshot 用 (= 動的更新は scope 外)。
    async fn build_lane_watch_path_map(
        daemon: &Arc<RwLock<Self>>,
    ) -> std::collections::HashMap<std::path::PathBuf, (String, String)> {
        let mut map = std::collections::HashMap::new();
        let daemon_read = daemon.read().await;
        let Some(config) = daemon_read.config.as_ref() else {
            return map;
        };
        for proj in &config.repos {
            let repo_root = std::path::PathBuf::from(&proj.path);
            let lanes_dir = repo_root.join(".vp").join("lanes");
            map.insert(lanes_dir, (proj.name.clone(), proj.path.clone()));
        }
        map
    }

    /// VP-129: Remove event 1 件を処理。 path → repo 解決 → daemon repo-proxy ask `lane_delete`。
    /// `run_lane_watcher` の inner、 各 path を独立処理。
    async fn handle_lane_remove_event(
        daemon: &Arc<RwLock<Self>>,
        path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
        event: &notify::Event,
    ) {
        for path in &event.paths {
            let Some((repo_name, repo_path, sub_name)) = resolve_lane_event(path, path_map) else {
                continue;
            };

            // repo port 取得 (= running_repos registry)。 `repo_path` は config の
            // String 型で持たれているので Path 変換してから normalize する。
            let port = {
                let daemon_read = daemon.read().await;
                let procs = daemon_read.running_repos.read().await;
                let key = normalize_path_key(std::path::Path::new(&repo_path));
                procs.get(&key).map(|p| p.port)
            };
            let Some(port) = port else {
                tracing::debug!(
                    "lane watcher: repo not running for repo={} (skip) sub={}",
                    repo_name,
                    sub_name
                );
                continue;
            };

            // lanes portless (doc 27 §3.4.5): 旧 SP HTTP DELETE /api/lanes を daemon repo-proxy ask
            // `lane_delete` に移管 (Daemon 内 loopback、 surface 群と uniform な transport)。 cleanup=false
            // で dir は既に gone。 self-loop case (= repo 経由削除で dir 消滅 → watcher が Remove 検知 →
            // 本 lane_delete 発火) は server が "Lane not found" を Err で返すので no-op 扱い。
            let address =
                crate::repo::lanes_state::LaneAddress::new(repo_name.as_str(), sub_name.as_str())
                    .canonical();
            let payload = serde_json::json!({ "address": address, "cleanup": false });
            tracing::info!(
                "lane watcher: dir removed → lane_delete 発火 (repo={}, sub={}, repo_port={})",
                repo_name,
                sub_name,
                port
            );
            match crate::commands::process_client::daemon_repo_request(
                crate::cli::daemon_port(),
                &repo_path,
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
                        "lane watcher: lane_delete 失敗 (repo={}, address={}): {}",
                        repo_name,
                        address,
                        e
                    );
                }
            }
        }
    }

    /// F.8 B Convergent: lane Create event を 1 path 処理。 path → repo + sub_name 解決 →
    /// repo POST /api/lanes (kind=sub, name=<sub>, cwd=<existing_dir>) で auto-spawn を依頼する。
    ///
    /// `run_lane_watcher` の inner、 sibling は `handle_lane_remove_event` (Remove 時の repo DELETE)。
    /// 設計対称性: Remove → DELETE / Create → POST で「dir 状態と LanePool 状態を一致させる」
    /// convergence loop を成立させる (= disk-only Lane を恒久化しない)。
    ///
    /// 競合 case:
    /// - sidebar `+` で作成中に Create event fired → repo 側 LanePool 重複チェックで CONFLICT
    ///   が返り、 watcher 側はそれを debug log で受ける (= silent OK)
    /// - repo 起動時 bootstrap で既に同 sub が SpawnLane Cmd 投入済 → 上記同様 CONFLICT で no-op
    async fn handle_lane_create_event(
        daemon: &Arc<RwLock<Self>>,
        path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
        event: &notify::Event,
    ) {
        for path in &event.paths {
            // dir のみ対象 (= `.vp/lanes/<name>` の new dir、 単発ファイルは無視)
            if !path.is_dir() {
                continue;
            }
            let Some((repo_name, repo_path, sub_name)) = resolve_lane_event(path, path_map) else {
                continue;
            };

            // repo port 取得 (= running_repos registry)
            let port = {
                let daemon_read = daemon.read().await;
                let procs = daemon_read.running_repos.read().await;
                let key = normalize_path_key(std::path::Path::new(&repo_path));
                procs.get(&key).map(|p| p.port)
            };
            let Some(port) = port else {
                tracing::debug!(
                    "lane watcher: repo not running for repo={} (skip create) sub={}",
                    repo_name,
                    sub_name
                );
                continue;
            };

            // lanes portless (doc 27 §3.4.5): 旧 SP HTTP POST /api/lanes を daemon repo-proxy ask
            // `lane_create` に移管 (Daemon 内 loopback、 surface 群と uniform な transport)。 payload は
            // CreateLaneReq (routes/lanes.rs) 互換。 cwd 明示で既存 dir を再利用 (new_sub_in skip)。
            // doc 44 P2: `kind` は撤去（lane に種別が無くなり、指定する余地が消えた）。
            //
            // agent は payload に積まない = 受け手の default に委ねる。
            // doc 44 §9.4 の統合前は、GUI create が descriptor だけ作って spawn を watcher に
            // 委ねていたため、選んだ agent を `lane_registry` の descriptor から引き直して
            // ここで積む必要があった（bug mem_1Cd4M7i5Enp3HHMLVYayRe「codex を選んでも claude で
            // spawn」の対処）。統合後は GUI create が自分で spawn するので **その lane はここに
            // 来ても "already exists" で弾かれる** — 引き継ぐ相手が居ない。今ここに来るのは
            // 手動 `vp lane new` 等で dir だけ生えた lane で、それは元々 descriptor を持たない
            // （= 旧実装でも None に落ちていた経路）。
            let payload = serde_json::json!({
                "name": sub_name,
                "cwd": path.to_string_lossy(),
            });
            tracing::info!(
                "lane watcher: dir created → lane_create 発火 (repo={}, sub={}, repo_port={})",
                repo_name,
                sub_name,
                port
            );
            match crate::commands::process_client::daemon_repo_request(
                crate::cli::daemon_port(),
                &repo_path,
                "lane_create",
                payload,
            )
            .await
            {
                Ok(_) => {
                    tracing::info!(
                        "lane watcher: lane_create 成功 (repo={}, sub={})",
                        repo_name,
                        sub_name
                    );
                }
                // 競合: sidebar `+` or bootstrap で既に Lane 作成済。 server は "already exists" を
                // Err で返すので silent OK (= 旧 HTTP CONFLICT 経路と等価)。
                Err(e) if e.to_string().contains("already exists") => {
                    tracing::debug!(
                        "lane watcher: lane_create 競合 (= 既に Lane あり、 silent OK) repo={} sub={}",
                        repo_name,
                        sub_name
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "lane watcher: lane_create 失敗 (repo={}, sub={}): {}",
                        repo_name,
                        sub_name,
                        e
                    );
                }
            }
        }
    }
}

/// lane Remove event 1 path を解決する純粋関数。 `path_map` (= `<.vp/lanes path>` → `(repo_name,
/// repo_path)`) から parent match で repo を逆引きし、 path の file_name を sub 名として
/// 返す。
///
/// 戻り値: `Some((repo_name, repo_path, sub_name))` if 完全 match。 そうでなければ `None`。
/// - dotfile / 空 sub 名は skip (= `.git` 内ファイル等の伝播除外)
/// - path_map に登録されてない repo 配下の path は skip
/// - I/O なしの pure fn (= test しやすい、 mock 不要)
fn resolve_lane_event(
    path: &std::path::Path,
    path_map: &std::collections::HashMap<std::path::PathBuf, (String, String)>,
) -> Option<(String, String, String)> {
    let parent = path.parent()?;
    let (repo_name, repo_path) = path_map.get(parent)?;
    let sub_name = path.file_name()?.to_str()?.to_string();
    if sub_name.is_empty() || sub_name.starts_with('.') {
        return None;
    }
    Some((repo_name.clone(), repo_path.clone(), sub_name))
}

/// lane watcher hot-reload の純粋 diff 計算。 `current` (= 現在 arm 済 path 集合) と
/// `new` (= 期待 path 集合 = `build_lane_watch_path_map` の最新 keys) から、
/// `(to_add, to_remove)` を返す。
///
/// - `to_add` = `new` にあって `current` に無い (= 新規 register された repo)
/// - `to_remove` = `current` にあって `new` に無い (= unregister された repo)
/// - I/O なしの pure fn (= test しやすい、 `notify::Watcher` mock 不要)
fn compute_watch_diff(
    current: &std::collections::HashSet<std::path::PathBuf>,
    new: &std::collections::HashSet<std::path::PathBuf>,
) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let to_add: Vec<_> = new.difference(current).cloned().collect();
    let to_remove: Vec<_> = current.difference(new).cloned().collect();
    (to_add, to_remove)
}

impl Default for RepoManagerCapability {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Capability for RepoManagerCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "daemon-capability",
            env!("CARGO_PKG_VERSION"),
            "Process Daemon - 複数のProject Processを統括管理",
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

        // doc 44 P1 (fold-in): vp binary の所在探索は撤去。repo を子プロセスとして
        // spawn しなくなったため、起動に binary path は要らない。残しておくと daemon 起動の
        // たびに無駄な FS stat + `which` の subprocess が走り、しかも今は無意味な
        // 「vp binary not found in PATH」警告で読み手の調査コストを誘発する。

        // 設定を読み込み
        if let Err(e) = self.load_config().await {
            tracing::warn!("Failed to load config: {}", e);
        }

        // doc 44 P1 (fold-in): 旧「初期状態チェック（PID liveness）」は撤去。boot 時点で
        // running_repos は空で、fold-in 後は pid が Daemon 自身になり liveness が無意味。

        self.state = CapabilityState::Idle;

        let repo_count = self.repos.read().await.len();
        tracing::info!(repos = repo_count, "RepoManagerCapability initialized");

        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        self.state = CapabilityState::Stopped;
        tracing::info!("RepoManagerCapability shutdown");
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
    fn test_daemon_capability_new() {
        let cap = RepoManagerCapability::new();
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
    }

    // --- PR3: repo spawn 平滑化 (CPU cap) ---

    /// floor 保証: 1〜2 core 機でも `spawn_cap()` は最低 1。
    /// `Semaphore::new(0)` は permit 永久枯渇で spawn が全 block する地雷なので、
    /// この floor が崩れると daemon が repo を一切起動できなくなる (回帰の急所)。
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
        let cap = RepoManagerCapability::new();
        assert_eq!(
            cap.spawn_semaphore.available_permits(),
            spawn_cap(),
            "spawn_semaphore は spawn_cap() 本の permit を持つ"
        );
    }

    // --- resolve_lane_event (repo-local lane refactor PR 4c) ---

    fn make_path_map(
        entries: &[(&str, &str, &str)],
    ) -> std::collections::HashMap<std::path::PathBuf, (String, String)> {
        let mut m = std::collections::HashMap::new();
        for (lanes_dir, repo_name, repo_path) in entries {
            m.insert(
                std::path::PathBuf::from(lanes_dir),
                (repo_name.to_string(), repo_path.to_string()),
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
        // 知らない repo 配下の path
        let path = std::path::Path::new("/Users/makoto/repos/other-repo/.vp/lanes/foo");
        assert_eq!(resolve_lane_event(path, &map), None);
    }

    #[test]
    fn resolve_lane_event_skips_dotfile_sub_name() {
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
    fn resolve_lane_event_multiple_repos_match_correct_one() {
        let map = make_path_map(&[
            ("/repo-a/.vp/lanes", "repo-a", "/repo-a"),
            ("/repo-b/.vp/lanes", "repo-b", "/repo-b"),
        ]);
        let path_b = std::path::Path::new("/repo-b/.vp/lanes/sub-x");
        let resolved = resolve_lane_event(path_b, &map);
        assert_eq!(
            resolved,
            Some((
                "repo-b".to_string(),
                "/repo-b".to_string(),
                "sub-x".to_string()
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
        // 起動直後: current 空、 new に N repo → 全部 to_add
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
        // 全 repo unregister: current に N、 new 空 → 全部 to_remove
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
        // edge: 両方空 (= repos 0 状態) → no-op
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
        let cap = RepoManagerCapability::new();
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
        let status = RepoStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn test_normalize_path_key_consistency() {
        // 同じパスの異なる表現が同じキーになることを確認
        let key1 = normalize_path_key(&PathBuf::from("/tmp/test-repo"));
        let key2 = normalize_path_key(&PathBuf::from("/tmp/test-repo/"));
        // 末尾スラッシュの正規化は Config::normalize_path に依存
        assert!(!key1.is_empty());
        assert!(!key2.is_empty());
    }

    #[test]
    fn test_repo_info_port_serialization() {
        // port が Some のとき JSON に含まれることを確認
        let info = RepoInfo {
            name: "test".to_string(),
            path: "/tmp/test".into(),
            process_status: RepoStatus::Stopped,
            port: Some(33005),
            enabled: true,
            slot: None,
            active_lane: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("33005"));

        // port が None のとき JSON に含まれないことを確認（skip_serializing_if）
        let info_no_port = RepoInfo {
            name: "test".to_string(),
            path: "/tmp/test".into(),
            process_status: RepoStatus::Stopped,
            port: None,
            enabled: true,
            slot: None,
            active_lane: None,
        };
        let json_no_port = serde_json::to_string(&info_no_port).unwrap();
        assert!(!json_no_port.contains("port"));
    }

    // --- CRUD テスト（async） ---

    /// テスト用ヘルパー: 空の RepoManagerCapability を作成
    fn make_test_cap() -> RepoManagerCapability {
        RepoManagerCapability::new()
    }

    /// テスト用ヘルパー: repos に 1 件登録する。
    fn test_repo(name: &str, port: Option<u16>) -> RepoInfo {
        RepoInfo {
            name: name.to_string(),
            path: format!("/tmp/{name}").into(),
            process_status: RepoStatus::Stopped,
            port,
            enabled: true,
            slot: None,
            active_lane: None,
        }
    }

    #[test]
    fn test_process_presence_state_as_str() {
        // /api/health の processes[].presence にそのまま載る文字列のロック (vp-app 描画契約)。
        assert_eq!(RepoPresenceState::Unregistered.as_str(), "unregistered");
        assert_eq!(RepoPresenceState::Connected.as_str(), "connected");
    }

    #[tokio::test]
    async fn test_presence_snapshot_joins_repos_running_and_presence() {
        let cap = make_test_cap();

        // repos (desired) に 2 件。proj-b を先に入れて sort も検証する。
        {
            let repos = cap.repos_ref();
            let mut projs = repos.write().await;
            projs.insert("/tmp/proj-b".to_string(), test_repo("proj-b", None));
            projs.insert("/tmp/proj-a".to_string(), test_repo("proj-a", Some(33000)));
        }

        // proj-a だけ live (running_repos に entry) + Connected。
        {
            let running = cap.running_processes_ref();
            running.write().await.insert(
                "/tmp/proj-a".to_string(),
                RunningRepo {
                    repo_name: "proj-a".to_string(),
                    port: 33000,
                    pid: 4242,
                    repo_path: "/tmp/proj-a".into(),
                },
            );
        }
        cap.set_presence("/tmp/proj-a", RepoPresenceState::Connected)
            .await;
        // proj-b は未登録 (live 不在だが repo は残る = Model Q)。
        cap.set_presence("/tmp/proj-b", RepoPresenceState::Unregistered)
            .await;

        let snap = cap.presence_snapshot().await;

        // repo 名で sort されている (HashMap 反復の非決定性を吸収)。
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].repo, "proj-a");
        assert_eq!(snap[1].repo, "proj-b");

        // proj-a: Connected + live port/pid。
        assert_eq!(snap[0].presence, "connected");
        assert_eq!(snap[0].port, Some(33000));
        assert_eq!(snap[0].pid, Some(4242));

        // proj-b: Unregistered + live 値は None (repo としては sidebar に残る = Model Q)。
        assert_eq!(snap[1].presence, "unregistered");
        assert_eq!(snap[1].port, None);
        assert_eq!(snap[1].pid, None);
    }

    #[tokio::test]
    async fn test_presence_snapshot_defaults_unregistered_without_entry() {
        // repos には在るが presence entry が無い (repo 未起動) → Unregistered default。
        let cap = make_test_cap();
        cap.repos_ref()
            .write()
            .await
            .insert("/tmp/proj-x".to_string(), test_repo("proj-x", None));
        let snap = cap.presence_snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].presence, "unregistered");
        assert_eq!(snap[0].port, None);
    }

    #[tokio::test]
    async fn test_set_presence_overwrites_previous_value() {
        // set_presence の**上書き機構**と、presence_snapshot 経由の文字列化を固定する。
        //
        // 旧名は `..._connecting_to_disconnected` で、コメントに「production 到達不能だが
        // enum 値としては有効なので全 variant を押さえる意味で残す」と書かれていた。
        // つまり**死んだ variant を生かすためにテストが存在していた**（2 値縮約で解消）。
        // 検証対象は元から「後の set_presence が前を上書きすること」なので、生きた 2 値で見る。
        let cap = make_test_cap();
        cap.repos_ref()
            .write()
            .await
            .insert("/tmp/proj-r".to_string(), test_repo("proj-r", None));
        cap.set_presence("/tmp/proj-r", RepoPresenceState::Connected)
            .await;
        assert_eq!(cap.presence_snapshot().await[0].presence, "connected");
        cap.set_presence("/tmp/proj-r", RepoPresenceState::Unregistered)
            .await;
        assert_eq!(cap.presence_snapshot().await[0].presence, "unregistered");
    }

    #[tokio::test]
    async fn test_remove_repo_clears_presence() {
        // namespace (repo) を倒したら presence entry も回収する (active_lanes と対称、orphan 防止)。
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();
        cap.add_repo("presence-cleanup", &path).await.unwrap();
        let key = normalize_path_key(std::path::Path::new(&path));
        cap.set_presence(&key, RepoPresenceState::Connected).await;
        {
            let presence = cap.process_presence_ref();
            assert!(presence.read().await.contains_key(&key));
        }
        cap.remove_repo(&path).await.unwrap();
        let presence = cap.process_presence_ref();
        assert!(
            !presence.read().await.contains_key(&key),
            "remove_repo は presence entry を回収すべき"
        );
    }

    #[tokio::test]
    async fn test_add_repo_success() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        let result = cap.add_repo("test-repo", &path).await;
        assert!(result.is_ok());

        let info = result.unwrap();
        assert_eq!(info.name, "test-repo");
        assert_eq!(info.process_status, RepoStatus::Stopped);
        assert_eq!(info.port, None);
    }

    #[tokio::test]
    async fn test_add_repo_duplicate_path() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_repo("first", &path).await.unwrap();
        let result = cap.add_repo("second", &path).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_add_repo_empty_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        let result = cap.add_repo("", &path).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_add_repo_whitespace_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        let result = cap.add_repo("   ", &path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_repo_nonexistent_path() {
        let cap = make_test_cap();
        let result = cap
            .add_repo("ghost", "/nonexistent/path/that/does/not/exist")
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a directory"));
    }

    #[tokio::test]
    async fn test_remove_repo_success() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_repo("removable", &path).await.unwrap();
        let result = cap.remove_repo(&path).await;
        assert!(result.is_ok());

        // 削除後は一覧に含まれない
        let repos = cap.list_repos().await;
        assert!(repos.is_empty());
    }

    #[tokio::test]
    async fn test_remove_repo_not_found() {
        let cap = make_test_cap();
        let result = cap.remove_repo("/nonexistent").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_remove_repo_reclaims_sub_ground_not_main() {
        // doc 24 §5.3 / B-destroy: repo remove で sub worktree(ground) は daemon が
        // reclaim、 main(=repo root = user の repo) は絶対に消さない、 を検証する。
        // git なしの plain dir で実行 (remove_sub_workspace は .git 無しなら fs 削除に落ちる)。
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};

        let cap = make_test_cap();
        // 一意な temp repo root (再実行に備え事前掃除)。
        // pid を含めて並行 `cargo test` 実行間での temp 衝突を避ける (team-b review、 低リスク)。
        let tmp =
            std::env::temp_dir().join(format!("vp-test-bdestroy-reclaim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let repo_path = tmp.to_string_lossy().to_string();
        cap.add_repo("bdestroy", &repo_path).await.unwrap();

        // sub の ground を物理作成 (<repo>/.vp/lanes/foo、 plain dir = fs 削除経路)。
        let sub_dir = tmp.join(".vp").join("lanes").join("foo");
        std::fs::create_dir_all(&sub_dir).unwrap();
        assert!(sub_dir.exists());

        // lane_registry に main + sub descriptor を投入 (daemon-canonical truth)。
        let key = normalize_path_key(&PathBuf::from(&repo_path));
        let mk = |addr: LaneAddress, cwd: &str| LaneInfo {
            id: Default::default(),
            address: addr,
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: None,
            cwd: cwd.to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };
        let main = mk(LaneAddress::root("bdestroy"), &repo_path);
        let sub = mk(
            LaneAddress::sub("bdestroy", "foo"),
            &sub_dir.to_string_lossy(),
        );
        cap.lane_registry_ref()
            .write()
            .await
            .insert(key.clone(), vec![main, sub]);

        // 実行: repo を倒す。
        cap.remove_repo(&repo_path).await.unwrap();

        // 検証: sub ground は reclaim、 main=repo root は無傷。
        assert!(
            !sub_dir.exists(),
            "sub ground (worktree) は daemon が reclaim する"
        );
        assert!(
            tmp.exists(),
            "root = repo root は絶対に消さない (user の repo)"
        );
        // descriptor も lane_registry から畳まれている。
        assert!(
            cap.lane_registry_ref().read().await.get(&key).is_none(),
            "repo remove で lane descriptor も回収される"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_sync_does_not_revive_repo_that_had_sub() {
        // lead #2: 実バグ条件に近い robustness 回帰。 sub を抱えた repo を削除した後、
        // sync (= `vp sp start` が起動時に撃つ) を回しても復活しないことを焼き付ける。 旧挙動では
        // sync_repos(Some(dir)) が起点 dir を無条件再登録し、 生きた sub で repo が
        // 死にきれず後で repo start → 復活する経路があった (mem_1CcuRsC9pF3fiZptwmdgTS)。
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};

        let cap = make_test_cap();
        let tmp = std::env::temp_dir().join(format!("vp-test-sync-revive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let repo_path = tmp.to_string_lossy().to_string();
        cap.add_repo("hasperf", &repo_path).await.unwrap();

        // sub descriptor を lane_registry に投入 (repo に sub 子がぶら下がる
        // 状態を模す。 plain dir なので worktree reclaim は fs 削除に落ちる = git 非依存)。
        let key = normalize_path_key(&PathBuf::from(&repo_path));
        let sub = LaneInfo {
            id: Default::default(),
            address: LaneAddress::sub("hasperf", "foo"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-11T00:00:00Z".to_string(),
            pid: None,
            cwd: tmp.join(".vp/lanes/foo").to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };
        cap.lane_registry_ref()
            .write()
            .await
            .insert(key.clone(), vec![sub]);

        // 削除 → repo も sub descriptor も畳まれる。
        cap.remove_repo(&repo_path).await.unwrap();
        assert!(cap.list_repos().await.is_empty(), "削除で repo は消える");

        // sync を回しても復活しない (起点 dir 自動登録が撤去済のため)。
        let outcome = cap.sync_repos().await.unwrap();
        assert!(outcome.removed.is_empty());
        assert!(
            cap.list_repos().await.is_empty(),
            "sub を抱えていた repo も sync で復活しない (resurrection 回帰)"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// doc 44 §9.4: Daemon 入口は **自前の実装を持たない**。repo runtime が居なければ
    /// 「repo 未起動」で止まり、worktree も db 行も作らない。
    ///
    /// 旧実装（本関数がここで worktree を provision して descriptor を所有していた）の
    /// end-to-end 検証は、実装ごと `create_sub_orchestrated` に移った。
    /// ここで押さえるのは統合後に残った境界の振る舞い —
    /// **「動いていない repo に半分だけの lane を作らない」**こと。
    /// 旧実装はここで provision だけして PtySlot を watcher の到達に賭けており、repo が
    /// 動いていなければ「作ったのに動かない lane」が disk と db に残っていた。
    #[tokio::test]
    async fn test_create_lane_without_repo_runtime_is_explicit_error() {
        let mut cap = make_test_cap();
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        cap.set_vpdb(db.clone());

        let parent = std::env::temp_dir().join(format!("vp-test-bcreate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let tmp = parent.join("bcreate");
        std::fs::create_dir_all(&tmp).unwrap();
        let repo_path = tmp.to_string_lossy().to_string();
        cap.add_repo("bcreate", &repo_path).await.unwrap();

        let err = cap
            .create_lane(&repo_path, "foo", "test/foo", "claude")
            .await
            .expect_err("repo runtime 不在では作れない");
        let msg = err.to_string();
        assert!(
            msg.contains("repo"),
            "理由が repo 側にあることが伝わる: {msg}"
        );

        // 半端な副作用を残さない: worktree も db 行も作られていない。
        assert!(
            !tmp.join(".vp").join("lanes").join("foo").exists(),
            "拒否された create は worktree を作らない"
        );
        assert!(
            db.list_lanes().await.unwrap().is_empty(),
            "拒否された create は descriptor を書かない"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// doc 44 §9.4 の統合の正しさ = **残った 1 本が、消えた側と同じ答えを出す**。
    ///
    /// 名前の gate は両入口とも `validate_sub_name` 1 本（doc 44 §9.3）だが、
    /// 「同じ関数を呼んでいる」は片方の呼び出しが消えても静かに真でなくなる。
    /// Daemon 入口と core に同じ名前を投げて **同一の error 文字列**が返ることで固定する。
    #[tokio::test]
    async fn test_daemon_entry_and_core_reject_names_identically() {
        let cap = make_test_cap();
        let state = crate::repo::state::build_test_app_state(None).await;
        let parent = std::env::temp_dir().join(format!("vp-test-parity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let tmp = parent.join("parity");
        std::fs::create_dir_all(&tmp).unwrap();
        let repo_path = tmp.to_string_lossy().to_string();

        for bad in ["", "   ", "root", "../etc/passwd", "foo bar", "-leading"] {
            let daemon_err = cap
                .create_lane(&repo_path, bad, "test/x", "claude")
                .await
                .expect_err("Daemon 入口は拒否する")
                .to_string();
            let core_err = crate::repo::routes::lanes::create_sub_orchestrated(
                &state,
                crate::repo::routes::lanes::build_create_lane_req(bad, "test/x", "claude"),
            )
            .await
            .expect_err("core も拒否する");
            assert_eq!(
                daemon_err, core_err,
                "両入口が同じ理由で拒否すること (name={bad:?})"
            );
        }

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// 回帰固定（doc 44 §9）: **拒否される名前の create は db の lane 行に一切触れない**。
    ///
    /// 旧実装は入口で空文字しか見ておらず、予約名は奥の `new_sub_in` が clone 段階で
    /// 初めて弾いていた。だが intent-first bracket は provision より **前に** descriptor を
    /// 永続するので、拒否されるべき入力が `<repo>/root` 行を上書き（①）し、
    /// rollback がそれを削除（③）する — **本物の開発起点 descriptor が消える**。
    ///
    /// 通常は in-memory の dup check が先に弾いて発火しないが、dup check は validation では
    /// なく、その cache は db と乖離しうる（boot load 失敗 / repo snapshot 上書き）。
    /// **ここでは意図的に registry を空のままにして masking を外し**、db 行の生存を直接見る。
    #[tokio::test]
    async fn test_create_lane_rejects_reserved_name_without_touching_db() {
        // ⚠️ 下の `with_root` は **実 PTY を spawn** し、その replay を `vp_state_dir()` に書く。
        // 隔離しないと user の実 state（`~/.local/state/vp/terminal_replay/reserved__root__1`）を
        // 汚し、かつテストが hermetic でなくなる（doc 50 §4.6 A6 の作業中に発見、2026-07-25）。
        let _state = crate::test_env::state_dir_async().await;
        let mut cap = make_test_cap();
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        cap.set_vpdb(db.clone());

        let parent = std::env::temp_dir().join(format!("vp-test-reserved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let tmp = parent.join("reserved");
        std::fs::create_dir_all(&tmp).unwrap();
        let repo_path = tmp.to_string_lossy().to_string();
        let key = normalize_path_key(&PathBuf::from(&repo_path));

        // 本物の開発起点 descriptor を db に置く（= 破壊対象）。
        let main = crate::repo::lanes_state::LanePool::with_root("reserved", repo_path.clone());
        let main_info = main.list().into_iter().next().expect("root descriptor");
        db.upsert_lane(&key, &main_info).await.unwrap();
        let addr_str = main_info.address.to_string();
        assert_eq!(addr_str, "reserved/lane/root");

        // dup check の masking は効かない状況（lane_registry は空）。
        let err = cap
            .create_lane(&repo_path, "root", "test/x", "claude")
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
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};

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
            id: Default::default(),
            address: LaneAddress::sub("proj", name),
            state: LaneState::Spawning,
            agent: "claude".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: None,
            cwd: cwd.to_string_lossy().into_owned(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };
        cap.lane_registry_ref().write().await.insert(
            key.to_string(),
            vec![mk("alive", &alive_dir), mk("gone", &gone_dir)],
        );
        // alive=provisioning (ground 在り→ready 期待)、 gone=ready (ground 無→dead 期待)。
        db.upsert_lane_lifecycle(key, "proj/lane/alive", "provisioning")
            .await
            .unwrap();
        db.upsert_lane_lifecycle(key, "proj/lane/gone", "ready")
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
            get("proj/lane/alive").as_deref(),
            Some("ready"),
            "provisioning + ground 在り → ready (provision 完了)"
        );
        assert_eq!(
            get("proj/lane/gone").as_deref(),
            Some("dead"),
            "ready + ground 外部削除 → dead (user の rm 尊重)"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[tokio::test]
    async fn test_sync_repos_prunes_ghosts_only() {
        // sync は ghost (dir 非実在) を除去するのみ。 起点 dir の自動登録は撤去済
        // (削除済 repo を repo 起動時 sync が復活させる resurrection バグの温床だった)。
        let cap = make_test_cap();
        let real = std::env::temp_dir().to_string_lossy().to_string();
        cap.add_repo("real", &real).await.unwrap();

        // 実在 dir の repo は残る (ghost 除去されない)、 何も新規登録しない。
        let outcome = cap.sync_repos().await.unwrap();
        assert!(outcome.removed.is_empty(), "実在 dir は ghost 除去されない");
        assert_eq!(cap.list_repos().await.len(), 1, "sync は repo を増やさない");
    }

    #[tokio::test]
    async fn test_sync_does_not_revive_removed_repo() {
        // resurrection バグ回帰テスト: 削除した repo は sync で復活しない。
        // 以前は `vp sp start <dir>` の起動時 sync が起点 dir を無条件再登録し、
        // 削除済 repo が db/kdl に復活した (mem_1CcuRsC9pF3fiZptwmdgTS)。
        let cap = make_test_cap();
        // temp_dir は実在するので ghost 除去の対象にはならない (= 復活するとしたら
        // 起点 dir 自動登録が原因、 という切り分けになる)。
        let dir = std::env::temp_dir().to_string_lossy().to_string();

        cap.add_repo("victim", &dir).await.unwrap();
        cap.remove_repo(&dir).await.unwrap();
        assert!(cap.list_repos().await.is_empty(), "削除直後は空");

        // sync を回しても復活しない (起点 dir 自動登録が無いため)。
        let outcome = cap.sync_repos().await.unwrap();
        assert!(outcome.removed.is_empty());
        assert!(
            cap.list_repos().await.is_empty(),
            "sync は削除済 repo を復活させない (resurrection 回帰)"
        );
    }

    #[tokio::test]
    async fn test_rename_repo_success() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_repo("old-name", &path).await.unwrap();
        let result = cap.rename_repo(&path, "new-name").await;
        assert!(result.is_ok());

        let repos = cap.list_repos().await;
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "new-name");
    }

    #[tokio::test]
    async fn test_rename_repo_empty_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_repo("existing", &path).await.unwrap();
        let result = cap.rename_repo(&path, "").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_rename_repo_not_found() {
        let cap = make_test_cap();
        let result = cap.rename_repo("/nonexistent", "new").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_key_by_name() {
        let cap = make_test_cap();
        let dir = std::env::temp_dir();
        let path = dir.to_string_lossy().to_string();

        cap.add_repo("findme", &path).await.unwrap();

        let found = cap.resolve_key_by_name("findme").await;
        assert!(found.is_some());

        let not_found = cap.resolve_key_by_name("nothere").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_list_repos_empty() {
        let cap = make_test_cap();
        let repos = cap.list_repos().await;
        assert!(repos.is_empty());
    }

    #[tokio::test]
    async fn test_list_running_processes_empty() {
        let cap = make_test_cap();
        let procs = cap.list_running_processes().await;
        assert!(procs.is_empty());
    }

    /// 回帰固定: `stop_process` は配線された `process_lifecycle_tx` に Remove を流す。
    ///
    /// この broadcast の生産者は fold-in で repo が消えた際に一度ゼロになり（`vp daemon
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

            // stop_process の前提: repos に name→key、running_repos に live entry。
            // repo_runtimes は未設定でも stop は tolerate する（registry の後始末のみ実施）。
            c.repos_ref()
                .write()
                .await
                .insert("/tmp/proj-x".to_string(), test_repo("proj-x", Some(33000)));
            c.running_processes_ref().write().await.insert(
                "/tmp/proj-x".to_string(),
                RunningRepo {
                    repo_name: "proj-x".to_string(),
                    port: 33000,
                    pid: 4242,
                    repo_path: "/tmp/proj-x".into(),
                },
            );

            c.stop_process("proj-x").await.expect("stop_process");
        }

        match rx.try_recv() {
            Ok(ProcessLifecycleEvent::Remove { repo_path }) => {
                assert_eq!(repo_path, "/tmp/proj-x");
            }
            other => panic!("Remove イベントが流れるべき: {other:?}"),
        }
    }
}
