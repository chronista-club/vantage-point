//! ActorRegistry — 常駐 task の台帳と停止責任
//!
//! repo（`start_repo`）/ daemon（`run_daemon`）が spawn する常駐 task を名前・scope・kind
//! 付きで預かり、`JoinHandle` を保持する。repo 側は `shutdown_repo`（PR-S3b）、daemon 側は
//! `run_daemon` の停止末尾（PR-S3c）が [`stop_all`] で終了を確認する（棚卸し 9-2 段階 3、doc 63 §8）。
//!
//! ## 経緯
//!
//! VP-159（creo-memories `mem_1CavCepJdf8XyQ82AAiSpv`）の PR-4a で「caller 経路を開かない
//! passive catalog」として新設、PR-4b で `SpawnableService::spawn_loop` と `spawn_service`
//! を足して lane-spawn / delivery の spawn を集約した。保持した handle を await / abort する
//! 経路は長らく無く（「supervisor 統一で activate」と書かれたまま）、PR-S3b で接続した。
//!
//! ## 役割
//!
//! - actor 名 / scope / kind の **metadata catalog** (= HashMap<String, ActorRegistryEntry>)
//! - layer_scope / kind による filter / lookup
//! - `spawn_service`（mailbox を持つ Service）/ `spawn_task`（持たない loop）で spawn し、
//!   `task: Option<JoinHandle<()>>` を保持する
//! - [`stop_all`]: 受付を閉じ（`closing`）、lock 内で handle を取り出し、lock 外で並行に
//!   終了を待つ（runner の `process_runner::stop_all` と同じ停止契約）
//!
//! ## 関連
//!
//! - parent epic: VP-156 (Mailbox routing 統一)、 VP-159 (H1 段)
//! - spike v0.1: `mem_1CavCepJdf8XyQ82AAiSpv`
//! - PR-1 受け皿 pattern 先例: 旧 `LaneComponentHost` (PR-δ-1、 #288/VP-135、 2026-09 撤去)、 Agent/Service trait (PR-1、 #326)
//! - PR-2 同型: agent / protocol を Agent impl (= #327)
//! - PR-3 同型: notify / lane-spawn / devices を Service impl (= #329)
//! - PR-4b 想定: Service trait sig 拡張 (`spawn_loop`) + 既存 3 Service の migration + caller 集約

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::component_service::{Component, LayerScope, Service, SpawnableService};

/// actor の種類 (= Agent か Service か)。
///
/// VP-159 PR-1 で定義した 2 trait に対応、 ActorRegistry が同 catalog で host する際の
/// discriminator として機能する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    /// ECS entity bound actor (= agent / protocol / board 等)。 PR-2 で formalized。
    Agent,
    /// singleton infra actor (= lane-spawn / delivery)。 PR-3 で formalized。
    Service,
    /// mailbox を持たない常駐 loop（TopicRouter bridge / lanes publish / periodic sweep 等）。
    /// 停止責任だけを registry に預ける（棚卸し 9-2 段階 3）。
    Task,
}

/// registry に登録される actor の entry。
///
/// `register_*` で登録した entry は `task: None`（metadata のみ）、`spawn_*` で登録した
/// entry は `Some(JoinHandle)` を持ち、`stop_all` が取り出して待つ。
pub struct ActorRegistryEntry {
    /// actor 名 (= mailbox address の actor 部分と一致、 例: `"notify"` / `"agent"`)
    pub name: String,
    /// actor の lifecycle / address scope (= `LayerScope`: Machine / Repo / Lane)
    pub scope: LayerScope,
    /// actor の種類 (= Agent or Service)
    pub kind: ActorKind,
    /// background task の JoinHandle（`spawn_service` / `spawn_task` で attach、
    /// `close_and_take_tasks` が取り出した後は `None`）。
    pub task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ActorRegistryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorRegistryEntry")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .field("kind", &self.kind)
            .field("task_attached", &self.task.is_some())
            .finish()
    }
}

/// 常駐 task の台帳。repo / daemon がそれぞれ 1 つ持つ（`RepoState.actor_registry` /
/// `DaemonState.actor_registry`）。
///
/// `register_*` は metadata だけの登録（task を持たない Agent / Service）、`spawn_*` は
/// spawn して `JoinHandle` を保持する登録。保持した handle は [`stop_all`] が回収する。
///
/// ## scope と kind の 2 軸 filter
///
/// scope (= Machine / Repo / Lane) と kind (= Agent / Service / Task) の 2 軸で filter できる
/// （`list_by_scope` / `list_by_kind`）。
#[derive(Default)]
pub struct ActorRegistry {
    entries: HashMap<String, ActorRegistryEntry>,
    /// `stop_all` が立てる「受付終了」。以後の `spawn_service` / `spawn_task` は何も起動せず Err。
    /// 停止の取り出しと同じ lock 内で立てるので、「取り出した後に新しい task が滑り込む」窓が無い。
    closing: bool,
}

/// `stop_all` が task 1 本の終了を待つ上限。超えたら `abort` して見切る。
pub const STOP_ALL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(8);

impl ActorRegistry {
    /// 空の registry を構築する。
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            closing: false,
        }
    }

    /// `Service` を registry に register する (= metadata only、 task=None)。
    ///
    /// 引数は `&S` で borrow、 actor instance は caller が保持する。 spawn まで registry に
    /// 任せるなら `spawn_service`。
    ///
    /// 同 name の existing entry があれば上書き (= caller の責務で重複 register を回避)。
    pub fn register_service<S: Service>(&mut self, service: &S) {
        let name = service.actor_name().to_string();
        let entry = ActorRegistryEntry {
            name: name.clone(),
            scope: service.layer_scope(),
            kind: ActorKind::Service,
            task: None,
        };
        self.entries.insert(name, entry);
    }

    /// `Agent` を registry に register する (= metadata only)。
    pub fn register_component<S: Component>(&mut self, agent: &S) {
        let name = agent.actor_name().to_string();
        let entry = ActorRegistryEntry {
            name: name.clone(),
            scope: agent.layer_scope(),
            kind: ActorKind::Agent,
            task: None,
        };
        self.entries.insert(name, entry);
    }

    /// `SpawnableService` を spawn し registry に register する (= spawn 統合、 PR-4b)。
    ///
    /// `service.spawn_loop(shutdown)` で background recv loop を起動し、 返り値の `JoinHandle<()>`
    /// を `ActorRegistryEntry.task` に保持する。 保持した handle は [`stop_all`] が回収する
    /// （棚卸し 9-2 段階 3 PR-S3b で接続。 それまでは保持するだけで await / abort の経路が無かった）。 同 name の existing entry は上書き
    /// (= caller の責務で重複 spawn を回避)。
    ///
    /// `service` は consume される (= `spawn_loop` の `self` 引数)。 instance を後で query
    /// する必要がある actor (= `DeviceRegistry` 等の hold pattern) は本 method ではなく
    /// `register_service(&service)` で metadata only register する。
    ///
    /// `stop_all` の後は Err（service は起動されずに drop される）。
    pub fn spawn_service<S: SpawnableService>(
        &mut self,
        service: S,
        shutdown: CancellationToken,
    ) -> Result<(), String> {
        let name = service.actor_name().to_string();
        let scope = service.layer_scope();
        if self.closing {
            return Err(format!("actor registry は停止済み: {name} は起動しない"));
        }
        // spawn_loop は self consume するため、 name / scope を先に取得してから起動する。
        let task = service.spawn_loop(shutdown);
        let entry = ActorRegistryEntry {
            name: name.clone(),
            scope,
            kind: ActorKind::Service,
            task: Some(task),
        };
        self.entries.insert(name, entry);
        Ok(())
    }

    /// mailbox を持たない常駐 loop を `tokio::spawn` して `JoinHandle` を預かる（棚卸し 9-2 段階 3）。
    ///
    /// future は `shutdown` token の cancel で自ら終わる契約。`stop_all` はその終了を待ち、
    /// 待ちきれなければ `abort` する。`stop_all` の後は future を spawn せずに Err。
    pub fn spawn_task<F>(&mut self, name: &str, scope: LayerScope, fut: F) -> Result<(), String>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if self.closing {
            return Err(format!("actor registry は停止済み: {name} は起動しない"));
        }
        let entry = ActorRegistryEntry {
            name: name.to_string(),
            scope,
            kind: ActorKind::Task,
            task: Some(tokio::spawn(fut)),
        };
        self.entries.insert(name.to_string(), entry);
        Ok(())
    }

    /// 受付を閉じ、預かっている `JoinHandle` を全部取り出す（entry の catalog は残す）。
    ///
    /// lock を持ったまま handle を await すると、task 側が同じ lock を取る経路で行き詰まるので、
    /// 取り出しだけを lock 内で行い、待つのは [`stop_all`] が lock 外で行う。
    pub fn close_and_take_tasks(&mut self) -> Vec<(String, JoinHandle<()>)> {
        self.closing = true;
        self.entries
            .values_mut()
            .filter_map(|e| e.task.take().map(|t| (e.name.clone(), t)))
            .collect()
    }

    /// 全 entry の iter。
    pub fn entries(&self) -> impl Iterator<Item = &ActorRegistryEntry> {
        self.entries.values()
    }

    /// `LayerScope` で filter (= supervisor が scope 別に dispatch するための pattern)。
    pub fn list_by_scope(&self, scope: LayerScope) -> Vec<&ActorRegistryEntry> {
        self.entries.values().filter(|e| e.scope == scope).collect()
    }

    /// `ActorKind` で filter (= Agent or Service 別の subset 取得)。
    pub fn list_by_kind(&self, kind: ActorKind) -> Vec<&ActorRegistryEntry> {
        self.entries.values().filter(|e| e.kind == kind).collect()
    }

    /// 名前で entry lookup。
    pub fn get(&self, name: &str) -> Option<&ActorRegistryEntry> {
        self.entries.get(name)
    }

    /// entry 数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// empty check。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// registry が預かる task を全部回収する（棚卸し 9-2 段階 3）。戻り値は終了を確認できた本数。
///
/// 呼び手は先に shutdown token を cancel していること（本関数は cancel しない — token の
/// 所有者は `RepoRuntime` / `run_daemon` で、registry は借りている側）。
///
/// 1. lock 内で受付を閉じて handle を取り出す（以後の `spawn_*` は Err）
/// 2. lock 外で 1 本ずつ終了を待つ。[`STOP_ALL_CONFIRM_TIMEOUT`] を超えた task は `abort`
///    （abort は future を drop するので、task が `JoinSet` で抱える子 task も一緒に落ちる）
pub async fn stop_all(registry: &Arc<RwLock<ActorRegistry>>) -> usize {
    stop_all_with(registry, STOP_ALL_CONFIRM_TIMEOUT).await
}

/// [`stop_all`] の timeout 可変版（test 用）。
///
/// task は互いに独立なので**並行に**待つ（合計は本数 × timeout ではなく最大 1 timeout）。
/// daemon 停止は repo を直列に畳むので、ここが直列だと詰まった task の数だけ秒が積む。
pub(crate) async fn stop_all_with(
    registry: &Arc<RwLock<ActorRegistry>>,
    timeout: Duration,
) -> usize {
    let tasks = registry.write().await.close_and_take_tasks();
    let waits = tasks.into_iter().map(|(name, task)| async move {
        // `timeout` は JoinHandle を消費する（Elapsed で drop = detach）ので、abort 用の
        // handle を先に控える。
        let abort = task.abort_handle();
        match tokio::time::timeout(timeout, task).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                tracing::warn!("actor task {name} が異常終了: {e}");
                false
            }
            Err(_) => {
                tracing::warn!("actor task {name} が {timeout:?} 以内に終わらない — abort");
                abort.abort();
                false
            }
        }
    });
    futures::future::join_all(waits)
        .await
        .into_iter()
        .filter(|ok| *ok)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    /// test fixture: minimal `Agent` impl
    struct FixtureComponent {
        name: &'static str,
        scope: LayerScope,
    }

    impl Component for FixtureComponent {
        fn actor_name(&self) -> &str {
            self.name
        }
        fn layer_scope(&self) -> LayerScope {
            self.scope
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// test fixture: minimal `Service` impl
    struct FixtureService {
        name: &'static str,
        scope: LayerScope,
    }

    impl Service for FixtureService {
        fn actor_name(&self) -> &str {
            self.name
        }
        fn layer_scope(&self) -> LayerScope {
            self.scope
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn registry_new_is_empty() {
        let r = ActorRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn registry_default_is_empty() {
        let r = ActorRegistry::default();
        assert!(r.is_empty());
    }

    #[test]
    fn register_service_adds_entry() {
        let mut r = ActorRegistry::new();
        let s = FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        };
        r.register_service(&s);
        assert_eq!(r.len(), 1);
        let entry = r.get("notify").expect("notify entry exists");
        assert_eq!(entry.name, "notify");
        assert_eq!(entry.scope, LayerScope::Repo);
        assert_eq!(entry.kind, ActorKind::Service);
        assert!(entry.task.is_none(), "PR-4a では task は常に None");
    }

    #[test]
    fn register_stand_adds_entry() {
        let mut r = ActorRegistry::new();
        let s = FixtureComponent {
            name: "agent",
            scope: LayerScope::Repo,
        };
        r.register_component(&s);
        assert_eq!(r.len(), 1);
        let entry = r.get("agent").expect("agent entry exists");
        assert_eq!(entry.kind, ActorKind::Agent);
    }

    #[test]
    fn register_same_name_overwrites() {
        // VP-159 PR-4a invariant: 同 name の re-register は上書き (caller の責務で重複回避)
        let mut r = ActorRegistry::new();
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        });
        // 同 name で異 scope を register
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Machine, // 異 scope
        });
        assert_eq!(r.len(), 1, "同 name は 1 entry");
        assert_eq!(
            r.get("notify").unwrap().scope,
            LayerScope::Machine,
            "上書き済"
        );
    }

    #[test]
    fn list_by_scope_filters_correctly() {
        let mut r = ActorRegistry::new();
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "lane-spawn",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "devices",
            scope: LayerScope::Machine,
        });

        let repo_entries = r.list_by_scope(LayerScope::Repo);
        let daemon_entries = r.list_by_scope(LayerScope::Machine);
        let lane_entries = r.list_by_scope(LayerScope::Lane);
        assert_eq!(repo_entries.len(), 2, "Repo scope は 2 個");
        assert_eq!(daemon_entries.len(), 1, "machine scope は 1 個");
        assert_eq!(lane_entries.len(), 0, "Lane scope は 0 個");
    }

    #[test]
    fn list_by_kind_filters_correctly() {
        let mut r = ActorRegistry::new();
        r.register_component(&FixtureComponent {
            name: "agent",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        });

        let agents = r.list_by_kind(ActorKind::Agent);
        let services = r.list_by_kind(ActorKind::Service);
        assert_eq!(agents.len(), 1);
        assert_eq!(services.len(), 1);
        assert_eq!(agents[0].name, "agent");
        assert_eq!(services[0].name, "notify");
    }

    #[test]
    fn coexist_5_actors_in_registry() {
        // VP-159 PR-4a invariant (= PR-2 / PR-3 invariant の延長): 5 actor (2 Agent + 3 Service)
        // が 1 registry に coexist できる事。 PR-4b で旧 AgentCapability (Agent) + ProtocolCapability（2026-09 撤去）
        // (Agent) + NotificationActor (Service) + LaneSpawnActor (Service) + DeviceRegistry
        // (Service) を同 registry で host する path を fixture で代理検証。
        let mut r = ActorRegistry::new();
        r.register_component(&FixtureComponent {
            name: "agent",
            scope: LayerScope::Repo,
        });
        r.register_component(&FixtureComponent {
            name: "protocol",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "lane-spawn",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "devices",
            scope: LayerScope::Machine,
        });

        assert_eq!(r.len(), 5);
        assert_eq!(r.list_by_kind(ActorKind::Agent).len(), 2);
        assert_eq!(r.list_by_kind(ActorKind::Service).len(), 3);
        assert_eq!(r.list_by_scope(LayerScope::Repo).len(), 4);
        assert_eq!(r.list_by_scope(LayerScope::Machine).len(), 1);
        assert_eq!(r.list_by_scope(LayerScope::Lane).len(), 0);
    }

    #[test]
    fn actor_kind_variants_are_distinct() {
        // 2 variant が PartialEq で区別される事 (supervisor が kind で dispatch する基盤)
        assert_ne!(ActorKind::Agent, ActorKind::Service);
    }

    #[test]
    fn entries_iter_returns_all() {
        let mut r = ActorRegistry::new();
        r.register_component(&FixtureComponent {
            name: "agent",
            scope: LayerScope::Repo,
        });
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        });

        let all: Vec<&ActorRegistryEntry> = r.entries().collect();
        assert_eq!(all.len(), 2);
    }

    /// test fixture: minimal `SpawnableService` impl (= shutdown を待つだけの loop)
    struct FixtureSpawnableService {
        name: &'static str,
        scope: LayerScope,
    }

    impl Service for FixtureSpawnableService {
        fn actor_name(&self) -> &str {
            self.name
        }
        fn layer_scope(&self) -> LayerScope {
            self.scope
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl SpawnableService for FixtureSpawnableService {
        fn spawn_loop(self, shutdown: CancellationToken) -> JoinHandle<()> {
            tokio::spawn(async move {
                shutdown.cancelled().await;
            })
        }
    }

    #[tokio::test]
    async fn spawn_service_registers_with_task() {
        // VP-159 PR-4b invariant: spawn_service で task (= JoinHandle) が attach される事
        let mut r = ActorRegistry::new();
        let shutdown = CancellationToken::new();
        r.spawn_service(
            FixtureSpawnableService {
                name: "notify",
                scope: LayerScope::Repo,
            },
            shutdown.clone(),
        )
        .expect("停止前の spawn_service は成功する");
        assert_eq!(r.len(), 1);
        let entry = r.get("notify").expect("notify entry exists");
        assert_eq!(entry.name, "notify");
        assert_eq!(entry.kind, ActorKind::Service);
        assert!(
            entry.task.is_some(),
            "spawn_service で task が attach される"
        );
        shutdown.cancel(); // background task を停止
    }

    #[tokio::test]
    async fn spawned_service_and_registered_stand_coexist() {
        // VP-159 PR-4b invariant: spawn_service (task あり) と register_component (task なし) が
        // 1 registry で coexist できる事 (= PR-4b で notify (spawned Service) + agent (Agent)
        // が同 registry で host される path を fixture で代理検証)。
        let mut r = ActorRegistry::new();
        let shutdown = CancellationToken::new();
        r.register_component(&FixtureComponent {
            name: "agent",
            scope: LayerScope::Repo,
        });
        r.spawn_service(
            FixtureSpawnableService {
                name: "notify",
                scope: LayerScope::Repo,
            },
            shutdown.clone(),
        )
        .expect("停止前の spawn_service は成功する");
        assert_eq!(r.len(), 2);
        // Agent は task なし (= register_component)、 spawned Service は task あり (= spawn_service)
        assert!(r.get("agent").unwrap().task.is_none(), "Agent は task なし");
        assert!(
            r.get("notify").unwrap().task.is_some(),
            "spawned Service は task あり"
        );
        shutdown.cancel();
    }

    /// 棚卸し 9-2 段階 3（PR-S3b）: `stop_all` は預かった task を全部待ち、終了を確認した
    /// 本数を返す。以後の `spawn_service` / `spawn_task` は起動せずに Err（受付終了）。
    ///
    /// 落ちるべき壊し方: `stop_all` が待たずに返る（= 旧挙動、handle を握るだけ）なら
    /// `finished` は 2 に届かない。`closing` を立てなければ末尾の 2 assert が落ちる。
    #[tokio::test]
    async fn stop_all_waits_for_every_task_and_closes_the_registry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let finished = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(RwLock::new(ActorRegistry::new()));
        let shutdown = CancellationToken::new();

        registry
            .write()
            .await
            .spawn_service(
                FixtureSpawnableService {
                    name: "notify",
                    scope: LayerScope::Repo,
                },
                shutdown.clone(),
            )
            .expect("停止前は起動できる");
        for name in ["sweep", "bridge"] {
            let done = finished.clone();
            let token = shutdown.clone();
            registry
                .write()
                .await
                .spawn_task(name, LayerScope::Repo, async move {
                    token.cancelled().await;
                    // cancel からすぐには終わらない task を模す（待たない停止だとここが未達）
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    done.fetch_add(1, Ordering::SeqCst);
                })
                .expect("停止前は起動できる");
        }
        assert_eq!(registry.read().await.len(), 3);

        shutdown.cancel();
        let confirmed = stop_all(&registry).await;
        assert_eq!(confirmed, 3, "3 本すべての終了を確認する");
        assert_eq!(
            finished.load(Ordering::SeqCst),
            2,
            "stop_all が返った時点で task の末尾まで到達している"
        );

        // 受付は閉じている: 何も起動せず Err（catalog の entry 数は変わらない）
        let mut guard = registry.write().await;
        assert!(
            guard
                .spawn_task("late", LayerScope::Repo, async {})
                .is_err(),
            "停止後の spawn_task は拒否"
        );
        assert!(
            guard
                .spawn_service(
                    FixtureSpawnableService {
                        name: "late-service",
                        scope: LayerScope::Repo,
                    },
                    CancellationToken::new(),
                )
                .is_err(),
            "停止後の spawn_service は拒否"
        );
        assert_eq!(guard.len(), 3, "拒否した分は catalog にも載らない");
    }

    /// cancel を無視する task は timeout で見切って `abort` する（future が drop される）。
    /// 落ちるべき壊し方: `abort_handle` を控えずに timeout だけ掛けると task は detach され
    /// `dropped` が立たない。
    #[tokio::test]
    async fn stop_all_aborts_a_task_that_ignores_cancel() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let registry = Arc::new(RwLock::new(ActorRegistry::new()));
        let flag = DropFlag(dropped.clone());
        registry
            .write()
            .await
            .spawn_task("stubborn", LayerScope::Repo, async move {
                let _flag = flag;
                std::future::pending::<()>().await;
            })
            .expect("停止前は起動できる");

        let confirmed = stop_all_with(&registry, Duration::from_millis(50)).await;
        assert_eq!(confirmed, 0, "終了を確認できた task は無い");
        // abort は非同期に効く — task が drop されるまで少し待つ
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timeout した task は abort されて future が drop される");
    }
}
