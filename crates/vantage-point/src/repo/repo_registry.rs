//! daemon が抱える per-repo 実行状態の registry（doc 44 P1 = SP fold-in）。
//!
//! # 位置付け
//!
//! 旧構成では repo 1 件 = repo プロセス 1 本で、daemon は QUIC registry / control /
//! canvas-ingest の 3 channel を張って外から操作していた。fold-in はこのプロセス境界を
//! 取り払い、repo を **daemon プロセス内の `Arc<AppState>` 1 個**に降格させる。
//! 本 registry がその map の実体で、旧 `running_repos` + `control_channels` の
//! 役割を 1 つにまとめて引き継ぐ。
//!
//! doc 44 D2 が言う「repo は認知境界に退化する」はここで達成される — repo は
//! もはやプロセスでも actor でもなく、**この HashMap のエントリ**でしかない。
//!
//! # なぜ AppState を 1 枚に畳まないのか
//!
//! `LanePool` は key が `LaneAddress { repo, kind, name }` なので原理的には全 repo
//! 分を 1 枚に merge できる。ただしそれをやると `AppState.repo_dir` の単一前提が壊れ、
//! `dispatch_repo_method` の signature 変更 → 約 50 箇所のテスト改修に波及する。
//! 一方 fold-in の目的（Daemon↔repo 配管の撤去・`vp sp` 退役・spine 三段→二段）は
//! **プロセス境界を消すだけで達成される**ため、1 枚化は分離して doc 44 P2
//! （`LaneAddress` フラット化）の担当とした。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::state::AppState;

/// repo 1 件分の実行状態と、その停止スイッチ。
pub(crate) struct RepoRuntime {
    pub state: Arc<AppState>,
    /// cancel すると当該 repo の spawn 済 task 群（lane monitor / snapshot publish 等）が止まる。
    pub shutdown: CancellationToken,
}

/// path_key → [`RepoRuntime`] の map。
///
/// key は [`normalize_path_key`](crate::capability::normalize_path_key) 正規化済パス
/// （旧 `running_repos` / `lane_registry` / `control_channels` と同じ規約）。
#[derive(Default)]
pub(crate) struct RepoRuntimes {
    inner: RwLock<HashMap<String, RepoRuntime>>,
    /// daemon の lane 集約 view（`RepoManagerCapability::lane_registry` と同一 Arc）。
    ///
    /// doc 44 P1 (fold-in): 旧構成では repo が QUIC "lanes" channel で daemon へ register
    /// snapshot を push し、この view を最新化していた。repo が消えた今、更新役は
    /// repo 自身の publish task が引き継ぐ（[`start`] が本 Arc を渡す）。
    /// `None` は「Daemon 以外の文脈」= test / repo 単体起動で、その場合 view は存在しない。
    node_lanes: Option<super::server::NodeLaneView>,
    /// daemon が開いた唯一の SurrealDB handle（doc 44 P1 PR4 = DB 統合）。
    ///
    /// 旧構成では repo ごとに `db/sp_{slug}/` を開いていた（VP-182: 別プロセス間の
    /// surrealkv LOCK 衝突回避）。fold-in で同一プロセスになったため handle を共有し、
    /// repo 次元は table の `repo_path` 列が持つ。`None` は DB なし（test / 接続失敗）。
    daemon_db: Option<crate::db::SharedVpDb>,
    /// doc 44 §11: repo の publish が vp-app への push を起こすための通知路。
    ///
    /// fold-in 前は repo の QUIC uplink（register / lanes-diff）が daemon の `lane_registry` を
    /// 更新しつつ `lane_change_tx` も撃っていた。fold-in で **view の更新だけが
    /// `publish_lanes` へ移管され、起床通知が移管されなかった**ため、vp-app の sidebar は
    /// wire 活動がある間しか新鮮でなくなっていた。この Arc がその辺を戻す。
    lane_change_tx: Option<tokio::sync::broadcast::Sender<String>>,
    /// daemon の canvas 集約 map（[`CanvasRouters`](super::topic_router::CanvasRouters)）。
    ///
    /// boot 窓の根治: daemon 再起動直後、vp-app の canvas subscribe が repo spawn より
    /// 先に届くと `canvas_router_for` は placeholder router を作って購読させる。repo 起動時に
    /// この map を引き、**placeholder が居ればそれを自分の `topic_router` として養子縁組**する
    /// （= 既存購読者ごと実 router になる）。居なければ従来どおり新規作成し、後から来る
    /// subscribe が live 結線する（get-or-create の両側性）。`None` は Daemon 以外の文脈（test）。
    canvas_routers: Option<super::topic_router::CanvasRouters>,
    /// shutdown 開始後に新規登録を受け付けないための門。
    ///
    /// [`shutdown_all`](Self::shutdown_all) は map を drain して停止するが、drain の**後**に
    /// 進行中の [`start`](Self::start) が insert を完了させると、その repo の spawn 済 task と
    /// SurrealDB handle が「Daemon stopped」ログの後も生き残る（= プロセスが終了できない）。
    ///
    /// 起動の入口は複数あり、いずれも daemon の shutdown 手続きの射程外で走る:
    ///   - `autostart_enabled_repos`（spawn した JoinHandle を保持していない）
    ///   - `repos/start` RPC（unison が接続ごとに独立 task で handler を回すため、
    ///     accept loop を abort しても既存接続の in-flight handler には波及しない）
    ///
    /// task の abort では解決できない: `start_repo` の await 途中で future を drop すると、
    /// その時点で既に spawn 済みの内部 task が孤児として残り、防ぎたい状態そのものになる。
    /// よって「進行中の起動は完走させ、登録直前に自己回収させる」設計を取る。
    closing: AtomicBool,
}

impl RepoRuntimes {
    pub fn new() -> Self {
        Self::default()
    }

    /// daemon の資源（lane 集約 view + DB handle）を結線した registry を作る。
    ///
    /// Daemon bootstrap 専用。
    /// - `node_lanes` を渡さないと repo を起こしても daemon の view が更新されず、
    ///   `vp ps` / sidebar / Unison `lanes/list` が boot 時の db 値で固まる。
    /// - `vpdb` を渡さないと repo は DB なしで走り、board / agent status が
    ///   永続しない（doc 44 P1 PR4 以前は repo が自分で db を開いていた）。
    pub fn for_daemon(
        node_lanes: super::server::NodeLaneView,
        vpdb: Option<crate::db::SharedVpDb>,
        lane_change_tx: tokio::sync::broadcast::Sender<String>,
        canvas_routers: super::topic_router::CanvasRouters,
    ) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            node_lanes: Some(node_lanes),
            daemon_db: vpdb,
            lane_change_tx: Some(lane_change_tx),
            canvas_routers: Some(canvas_routers),
            closing: AtomicBool::new(false),
        }
    }

    /// repo を in-process で起動して登録する。
    ///
    /// 既に起動済みなら **no-op で `Ok(false)`**（旧 `start_process` の dedup 相当。
    /// プロセスが無くなったので「重複 spawn」は map への二重 insert として自然に防げる）。
    /// 新規起動できたら `Ok(true)`。
    ///
    /// shutdown 開始後は `Err` を返す（[`closing`](Self::closing) 参照）。`Ok(false)` に
    /// しないのは、caller の `start_process` が「false = 既に起動済み」と解釈して
    /// `running_repos` / presence に **動いていない repo を登録してしまう**ため。
    pub async fn start(&self, repo_dir: &str) -> Result<bool> {
        let key = crate::capability::normalize_path_key(std::path::Path::new(repo_dir));

        if self.closing.load(Ordering::Acquire) {
            anyhow::bail!("daemon が shutdown 中のため repo を起動しない (key={key})");
        }

        // 二重起動の早期棄却。 起動には時間がかかるので、まず read lock だけで判定する。
        if self.inner.read().await.contains_key(&key) {
            return Ok(false);
        }

        let shutdown = CancellationToken::new();
        let cap_config = super::capabilities::CapabilityConfig {
            repo_dir: repo_dir.to_string(),
        };
        // boot 窓の根治: 先行 subscribe が作った placeholder canvas router が居れば
        // それを repo の topic_router として養子縁組する（既存購読者ごと実 router 化）
        let adopted_router = self.adopted_router_for(&key).await;
        if adopted_router.is_some() {
            tracing::info!("canvas placeholder router を養子縁組 (key={})", key);
        }
        // port はもう bind されない（SP-portless の遺産）。fold-in で概念ごと消えるため 0 を渡す。
        let state = super::server::start_repo(
            0,
            cap_config,
            shutdown.clone(),
            self.node_lanes.clone(),
            self.daemon_db.clone(),
            self.lane_change_tx.clone(),
            adopted_router,
        )
        .await?;

        // 起動中に別 caller が同 repo を起こしていた場合はこちらを捨てる（後勝ちにしない）。
        let mut guard = self.inner.write().await;
        if guard.contains_key(&key) {
            drop(guard);
            shutdown.cancel();
            super::server::shutdown_repo(&state).await;
            return Ok(false);
        }
        // 起動している間に shutdown が始まっていたら、ここで自己回収する。
        // `shutdown_all` は closing を立ててから drain するので、この recheck を write lock 内で
        // 行うことで「drain の後に insert が滑り込む」窓が閉じる（漏れると当該 repo の
        // task と db handle が残り、プロセスが終了できなくなる）。
        if self.closing.load(Ordering::Acquire) {
            drop(guard);
            shutdown.cancel();
            super::server::shutdown_repo(&state).await;
            anyhow::bail!("起動中に daemon の shutdown が始まったため巻き戻した (key={key})");
        }
        let live_router = state.topic_router.clone();
        let bridge_shutdown = shutdown.clone();
        guard.insert(key.clone(), RepoRuntime { state, shutdown });
        drop(guard);

        // 養子縁組の後追い reconcile: spawn 中（上の adoption check の後）に先行 subscribe が
        // placeholder を滑り込ませた狭い race の後始末。
        if let Some(routers) = &self.canvas_routers
            && bridge_orphan_placeholder(routers, &key, &live_router, bridge_shutdown).await
        {
            tracing::info!(
                "spawn 中に滑り込んだ canvas placeholder へ橋渡し (key={})",
                key
            );
        }
        Ok(true)
    }

    /// 先行 subscribe が作った placeholder canvas router を引く（養子縁組の lookup）。
    ///
    /// key は正規化済パス（[`start`](Self::start) と同一規約 = subscribe handshake の
    /// `normalize_path_key` とも一致）。`None` = map 不在（test 文脈）or entry 無し。
    async fn adopted_router_for(&self, key: &str) -> Option<Arc<super::topic_router::TopicRouter>> {
        let routers = self.canvas_routers.as_ref()?;
        let map = routers.read().await;
        map.get(key).cloned()
    }

    /// repo を停止して登録解除する。停止対象が居なければ `false`。
    pub async fn stop(&self, path_key: &str) -> bool {
        let Some(rt) = self.inner.write().await.remove(path_key) else {
            return false;
        };
        rt.shutdown.cancel();
        super::server::shutdown_repo(&rt.state).await;
        true
    }

    /// 登録済 repo を**すべて**停止する（daemon の graceful shutdown 用）。戻り値は停止数。
    ///
    /// doc 44 P1 (fold-in): 旧構成では repo = 別プロセス (repo) だったため、daemon が
    /// 落ちても repo は生き残るのが**設計上の正**だった（`vp daemon stop` の gentle 挙動）。
    /// repo が Daemon 内の `Arc<AppState>` になった今、この前提は反転する — repo は
    /// daemon の tokio task と SurrealDB handle でしかないので、**daemon が畳まなければ
    /// 誰も畳まない**。
    ///
    /// これを怠ると daemon は listener を閉じて「停止」を名乗るのにプロセスが終了できない。
    /// PR4（DB 統合）以前はこれが db の LOCK 保持として表面化し、次に起動した daemon が
    /// その LOCK を「重複 spawn」と誤検出して**全 repo の起動に失敗**していた
    /// （実機で観測済み）。db が単一化された今、残留プロセスは `db/machine/` の LOCK と
    /// :32000 の bind を握るので、次の daemon は起動そのものが弾かれる（= 失敗が早期化した
    /// だけで、畳み残しが致命的である点は変わらない）。
    pub async fn shutdown_all(&self) -> usize {
        // drain より先に受付を閉じる。この順序が要点で、逆にすると「drain 済みの map へ
        // 進行中の start が insert を完了させる」窓が残る（= 畳んだはずの repo が生き残る）。
        self.closing.store(true, Ordering::Release);
        // 先に map を空にしてから停止する（停止の await 中に別 caller が同じ repo を
        // 掴んで二重に畳むのを防ぐ。`stop()` の remove-then-shutdown と同じ順序）。
        let drained: Vec<(String, RepoRuntime)> = {
            let mut guard = self.inner.write().await;
            guard.drain().collect()
        };
        let count = drained.len();
        for (key, rt) in drained {
            rt.shutdown.cancel();
            super::server::shutdown_repo(&rt.state).await;
            tracing::info!("repo 停止 (key={})", key);
        }
        count
    }

    /// 登録済 repo の `AppState` を引く。
    pub async fn get(&self, path_key: &str) -> Option<Arc<AppState>> {
        self.inner
            .read()
            .await
            .get(path_key)
            .map(|r| r.state.clone())
    }

    /// 当該 repo の [`dispatch_repo_method`] を **直接呼ぶ**。
    ///
    /// 旧 `forward_to_sp_control`（Daemon → QUIC control channel → repo）の後継。
    /// 戻り値の形（成功は生の JSON、失敗は `{"error": ...}`）は caller が
    /// `send_response` でそのまま client に relay するため互換に保つ。
    ///
    /// [`dispatch_repo_method`]: super::unison_server::dispatch_repo_method
    pub async fn dispatch(
        &self,
        path_key: &str,
        method: &str,
        payload: &serde_json::Value,
    ) -> serde_json::Value {
        let Some(state) = self.get(path_key).await else {
            return serde_json::json!({
                "error": format!("repo 未起動 (key={})", path_key)
            });
        };
        match super::unison_server::dispatch_repo_method(&state, method, payload.clone()).await {
            Ok(v) => v,
            Err(e) => serde_json::json!({
                "error": format!("repo dispatch 失敗 (key={}): {}", path_key, e)
            }),
        }
    }
}

/// spawn 中に滑り込んだ orphan placeholder への橋渡し（戻り値 = 橋を張ったか）。
///
/// map の当該 entry が live router と別個体（= adoption check 後に `canvas_router_for` の
/// slow path が作った placeholder）なら、live の全 topic を orphan へ relay する task を張り、
/// 取り残された購読者を生かす。map の差し替えと live への demand hook 登録は次の subscribe 時の
/// `canvas_router_for` live 分岐に任せる（ここで live を先置きすると ptr_eq 早期 return で
/// hook 登録がスキップされるため、意図的に触らない）。橋は repo の shutdown token で畳む。
///
/// 既知の残課題（pre-existing）: orphan と live で demand count の実体が分裂する性質は
/// placeholder 機構自体が持つもので、本橋渡しは配信だけを直す（発生自体は養子縁組が減らす）。
async fn bridge_orphan_placeholder(
    routers: &super::topic_router::CanvasRouters,
    key: &str,
    live_router: &Arc<super::topic_router::TopicRouter>,
    shutdown: CancellationToken,
) -> bool {
    let orphan = {
        let map = routers.read().await;
        map.get(key)
            .filter(|existing| !Arc::ptr_eq(existing, live_router))
            .cloned()
    };
    let Some(orphan) = orphan else {
        return false;
    };
    let (sub_id, mut rx) = live_router.subscribe("#").await;
    let live = live_router.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                msg = rx.recv() => {
                    let Some((_topic, msg)) = msg else { break };
                    orphan.route(msg).await;
                }
            }
        }
        live.unsubscribe(sub_id).await;
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_on_unknown_repo_reports_not_started() {
        let runtimes = RepoRuntimes::new();
        let res = runtimes
            .dispatch("/no/such/repo", "lanes_list", &serde_json::json!({}))
            .await;
        let err = res
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            err.contains("repo 未起動"),
            "未起動 repo は error JSON を返すべき: {res}"
        );
    }

    #[tokio::test]
    async fn stop_on_unknown_repo_is_false() {
        let runtimes = RepoRuntimes::new();
        assert!(!runtimes.stop("/no/such/repo").await);
    }

    #[tokio::test]
    async fn empty_registry_resolves_nothing() {
        let runtimes = RepoRuntimes::new();
        assert!(runtimes.get("/anything").await.is_none());
    }

    // ─── boot 窓根治（canvas router 養子縁組 + orphan 橋渡し）───

    /// 養子縁組の lookup: 先行 subscribe の placeholder が**同一 Arc のまま**引けること。
    /// （ここが clone 等で別個体になると、購読者ごと実 router 化する仕組み全体が壊れる）
    #[tokio::test]
    async fn adoption_lookup_returns_shared_placeholder() {
        use crate::repo::topic_router::{CanvasRouters, TopicRouter};

        let canvas_routers: CanvasRouters = Default::default();
        let placeholder = Arc::new(TopicRouter::new());
        canvas_routers
            .write()
            .await
            .insert("/tmp/proj-a".to_string(), placeholder.clone());

        let (tx, _) = tokio::sync::broadcast::channel::<String>(4);
        let runtimes =
            RepoRuntimes::for_daemon(Default::default(), None, tx, canvas_routers.clone());

        let adopted = runtimes
            .adopted_router_for("/tmp/proj-a")
            .await
            .expect("placeholder が引けること");
        assert!(
            Arc::ptr_eq(&adopted, &placeholder),
            "同一 Arc であること（購読者ごと養子縁組できる個体）"
        );
        assert!(runtimes.adopted_router_for("/tmp/other").await.is_none());
        // Daemon 以外の文脈（map なし = RepoRuntimes::new）は常に None
        assert!(
            RepoRuntimes::new()
                .adopted_router_for("/tmp/proj-a")
                .await
                .is_none()
        );
    }

    /// spawn 中に滑り込んだ orphan placeholder への橋渡し: live に route した message が
    /// orphan の購読者へ relay されること（boot 窓の狭い race の後始末が実際に配信を生かす）。
    #[tokio::test]
    async fn orphan_bridge_relays_live_messages() {
        use crate::protocol::RepoMessage;
        use crate::repo::topic_router::{CanvasRouters, TopicRouter};

        let canvas_routers: CanvasRouters = Default::default();
        let orphan = Arc::new(TopicRouter::new());
        let (_sub, mut orphan_rx) = orphan.subscribe("#").await;
        canvas_routers
            .write()
            .await
            .insert("/tmp/p".to_string(), orphan.clone());

        let live = Arc::new(TopicRouter::new());
        let token = CancellationToken::new();
        assert!(
            bridge_orphan_placeholder(&canvas_routers, "/tmp/p", &live, token.clone()).await,
            "別個体の placeholder が居るので橋が張られること"
        );

        live.route(RepoMessage::Ping).await;
        let (topic, _) = tokio::time::timeout(std::time::Duration::from_secs(2), orphan_rx.recv())
            .await
            .expect("relay が 2s 以内に届くこと")
            .expect("channel が生きていること");
        assert_eq!(topic, "repo/star-platinum/event/ping");
        token.cancel();
    }

    /// 養子縁組済み（map の entry = live と同一個体）/ entry 不在では橋を張らないこと。
    /// （不要な relay task は二重配信・leak の芽になるため、張らない側も固定する）
    #[tokio::test]
    async fn adopted_or_absent_router_needs_no_bridge() {
        use crate::repo::topic_router::{CanvasRouters, TopicRouter};

        let canvas_routers: CanvasRouters = Default::default();
        let live = Arc::new(TopicRouter::new());
        canvas_routers
            .write()
            .await
            .insert("/tmp/p".to_string(), live.clone()); // 養子縁組済み = 同一個体
        assert!(
            !bridge_orphan_placeholder(&canvas_routers, "/tmp/p", &live, CancellationToken::new())
                .await
        );
        assert!(
            !bridge_orphan_placeholder(
                &canvas_routers,
                "/tmp/none",
                &live,
                CancellationToken::new()
            )
            .await
        );
    }

    /// shutdown 後の `start` は登録せず Err を返す。
    ///
    /// 回帰固定: ここが `Ok(false)` だと caller の `start_process` が「既に起動済み」と
    /// 解釈して running_repos / presence に動いていない repo を載せる。また
    /// `shutdown_all` の drain 後に insert が滑り込むと、その repo の task と db handle が
    /// 残ってプロセスが終了できなくなる（Daemon shutdown の 82 分ハングの再現経路）。
    #[tokio::test]
    async fn start_after_shutdown_is_rejected() {
        let runtimes = RepoRuntimes::new();
        assert_eq!(runtimes.shutdown_all().await, 0);

        let err = runtimes
            .start("/tmp/proj-after-shutdown")
            .await
            .expect_err("shutdown 後の start は Err であること");
        assert!(
            err.to_string().contains("shutdown"),
            "shutdown 中である旨が伝わるエラーであること: {err}"
        );
        assert!(
            runtimes.get("/tmp/proj-after-shutdown").await.is_none(),
            "拒否した repo は登録されないこと"
        );
    }
}
