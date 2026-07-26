//! Lane lifecycle の core 関数群 (lanes portless、 doc 27 §3.4.5)。
//!
//! repo 直結 HTTP route (`GET`/`POST`/`DELETE /api/lanes` `POST /api/lanes/restart`) は全廃。
//! create / list / delete / restart は全て daemon repo-proxy ask の dispatch method
//! (`lane_create` / `lanes_list` / `lane_delete` / `lane_restart`) に移管し、 本 module は
//! その core 関数 (axum 非依存) のみを保持する。 全 surface (CLI flow / MCP / lane watcher) は
//! 同 dispatch method を共有する (semantics SSOT)。
//!
//! 関連 memory:
//! - `mem_1CaSsN7xj69aVQtLPQFJxQ` (repo-as-Repo-Master: 9 component minimum)
//! - VP-124 Phase 1 (Lane Lifecycle delete orchestration、 `delete_lane_orchestrated`)
//!
//! ## core 関数 (dispatch_repo_method が呼ぶ)
//!
//! - [`build_lanes_snapshot`] — `lanes_list` + QUIC `LanesSnapshot` publish 経路で共有する list
//!   (`LanePool` 由来のみ、 F.8 B Convergent で disk-scan merge 撤去。 disk-only Lane は lane
//!   watcher / repo bootstrap の auto-spawn 経由で active 化)
//! - [`create_performer_orchestrated`] — `lane_create` core (Phase 3-A: lane clone + PtySlot spawn、
//!   F.8 B Convergent で spawn 失敗時の disk dir rollback ポリシー)
//! - [`delete_lane_orchestrated`] / [`restart_lane_orchestrated`] — `lane_delete` / `lane_restart` core
//!   (F6②③、 PtySlot kill + tmux kill + SystemEvent broadcast)
//!
//! ## 未実装 (後 phase)
//!
//! - 1 件取得 (addr 指定) / Stand 切替 dispatch (addr encoding path 確定後)
//! - WS /ws/terminal の lane param 強化 (A4-2d)

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::super::lanes_state::{Diff, LaneAddress, LaneInfo, LaneState, SystemEvent};
use super::super::state::AppState;

// doc 11 §3.7 の `migrate_legacy_stand` shim は 2026-05-03 削除済。 PR #257 の
// stand 識別子 String 化と同タイミングで導入した旧 stand 名 → 現行名の変換 (PR-pre2 で hd → echoes)
// migration shim を 1 release 期間 deprecation warn 付きで accept していたが、
// VP は user 1 人 + lane performer のみで vp-app + daemon が常に同 binary で deploy される
// 構成のため、 外部 client が旧 wire format で来る window が実質ゼロと判断、 即削除。

/// disk-only performer（pool 未登録）の `created_at` を **決定的に**求める。
///
/// 旧実装は `chrono::Utc::now()` を焼いていた。表示上はほぼ無害だが、
/// [`super::super::server::publish_lanes`] の指紋（doc 44 §11.3）が呼ぶたびに変わり、
/// **「変わった時だけ vp-app を起こす」が無効化される** — 該当 repo では
/// 5s tick がそのまま push 源に戻り、修正の意味が消える。
///
/// ground dir の birthtime（無ければ mtime）を使う。決定的であることが要件で、
/// 「その lane がいつ作られたか」という意味にも合う。
///
/// 残余リスク: birthtime 非対応 FS では mtime に落ちるため、dir 直下のファイル増減で
/// 値が動きうる。その場合の劣化は「未 spawn performer が居る repo で push が増える」
/// = 本修正**前**の挙動に戻るだけで、それ以上悪くはならない。
fn ground_created_at(path: &str) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    meta.created()
        .or_else(|_| meta.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default()
}

/// repo の全 Lane snapshot を build する (LanePool 由来のみ、 disk-only は乗せない)。
///
/// daemon repo-proxy ask `lanes_list` と QUIC `lanes_snapshot` 両 publish 経路で **同一 logic**
/// を共有するための helper（旧 HTTP `GET /api/lanes` も同 logic だったが lanes portless で撤去）。
///
/// ## F.8 B Convergent (2026-05-26): disk-only Lane の表示廃止
///
/// 旧版は LanePool に居ない performer dir を disk-scan で `pid: None, state: Running` として
/// merge し sidebar に italic dim で表示していた。これは **中間状態 (= disk dir はあるが
/// LanePool に居ない)** を可視化する設計だったが、 click 不可 / Activate 経路なしで
/// 「死に体」 として user 体験を悪化させていた。
///
/// 新版では:
/// - sidebar に表示される Lane は **LanePool 由来のみ** (= 必ず active 化を試みた結果)
/// - disk dir 発見 → repo 起動時 bootstrap (server.rs) or lane watcher Create event
///   (capability/repo_manager_capability.rs `handle_lane_create_event`) で
///   auto-spawn → LanePool 経由で sidebar に出る
/// - spawn 失敗時は LanePool に `LaneState::Dead` で record される (= 失敗が見える)
///
/// この変更で「disk dir はあるが sidebar に出ない」 ケースが理論上一瞬発生するが、
/// lane watcher が即座に POST /api/lanes を発火して spawn → LanePool entry が即追加
/// される (= convergence、 user 視点では遅延 ms 単位)。
///
/// Phase 5-D: Performer Lane に対しては `cwd` から git 状態 (`PerformerStatus`) を populate。
/// registry には保存せず、 build 時に都度 `performer_status()` を呼ぶ (volatile + 5-7 git subprocess)。
pub async fn build_lanes_snapshot(state: &AppState) -> Vec<LaneInfo> {
    let pool = state.lane_pool.read().await;
    let mut lanes = pool.list();
    drop(pool); // git subprocess 中の lock を保たない (performer_status は数 100ms かかる事あり)

    // 起動窓 self-heal: disk 上の intended performer で LanePool にまだ入っていない
    // (= repo 再起動直後、bootstrap の SpawnLane がまだ処理されず pool に入る前) ものを
    // Spawning(pid=null) で snapshot に merge する。
    //
    // 背景: F.8 B Convergent で disk-scan merge を撤去した結果、repo 再起動直後の初回 snapshot が
    // conductor-only になり、vp-app の LanesLoaded reconcile が performer を「消えた」と誤判定して
    // console を teardown する regression が発生していた。snapshot に常に intended performer を
    // 含めることで根治する。pid=null なので vp-app は ensureLane せず(xterm を描画しない)、spawn
    // 完了で pid が付いた次 snapshot で ensure される。steady-state では全 performer が pool 由来で
    // 既に snapshot に居るため address key で dedup し無加算。
    let repo = std::path::Path::new(&state.repo_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let present: std::collections::HashSet<String> =
        lanes.iter().map(|l| l.address.to_string()).collect();
    let default_stand = crate::config::Config::load()
        .unwrap_or_default()
        .default_stand_or_echoes()
        .to_string();
    // ⚠️ この placeholder の field は **publish ごとに変わってはいけない**（doc 44 §11.3）。
    // `publish_lanes` は snapshot の指紋で「変わった時だけ vp-app を起こす」ので、ここに
    // 呼ぶたび変わる値を焼くと 5s tick がそのまま push 源に戻り、修正が無効化される。
    for entry in
        crate::lane::commands::list_performers_for_repo(std::path::Path::new(&state.repo_dir))
    {
        let address =
            crate::repo::lanes_state::LaneAddress::performer(repo.clone(), entry.name.clone());
        if present.contains(&address.to_string()) {
            continue; // 既に pool 由来 (spawn 済 or Dead) で snapshot に居る
        }
        lanes.push(LaneInfo {
            // doc 53 R1: boot 窓の act は `sessions`（下の refresh_engine_session_id が registry
            // から populate）で運ぶ — 旧 console_mode 投影は退役。vp-app は sessions から root の
            // act を導出して「chat lane を xterm で開く」誤復元を防ぐ（doc 47 §4 の性質は不変）。
            id: crate::lane::lane_id::load_or_create(&repo, &entry.name),
            address,
            state: crate::repo::lanes_state::LaneState::Spawning,
            stand: default_stand.clone(),
            created_at: ground_created_at(&entry.path),
            pid: None,
            cwd: entry.path,
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        });
    }

    // 既存 Performer の git status を populate
    for lane in lanes.iter_mut() {
        if !lane.address.is_root() {
            let path = std::path::Path::new(&lane.cwd);
            if path.exists() && path.join(".git").exists() {
                lane.performer_status = Some(crate::lane::commands::performer_status(path));
            }
        }
        // doc 40 §5: chip（engine_session_id）/ channel D（cc_session_id）/ sessions を
        // registry 1 read で enrich する（LaneInfo 側メソッドに一本化 — 旧「conductor 限定の
        // cc_session 個別 enrich」は本 method に畳んだ。uplink の agent_card / LaneDiff push と
        // 同一実装になり、供給点ごとの実装差（#683 地形）が消えた）。
        // QUIC 5s tick 経路で lane 数 × registry 1 file read の同期 I/O。通常運用（〜十数 lane）
        // では無害。桁で増える運用になったら spawn_blocking 化 / active lane 限定 read が最適化余地
        //（moody 参考指摘 2026-07-15）。
        lane.refresh_engine_session_id();
    }

    // doc 44 §12: 帳簿の並び順を最後に適用する（既定順 = 開発起点が先頭 → created_at の上に
    // 被せる）。ここが全経路の choke point なので、`lanes_list` も QUIC snapshot も
    // **同じ並び**になる（供給点ごとに sort が散ると「見る場所で順番が違う」が起きる）。
    crate::host::ledger::sort_lanes_by_ledger(state.vpdb.as_ref(), &state.repo_dir, &mut lanes)
        .await;

    lanes
}

/// Performer Lane create の request body (Phase 3-A: Performer Lane create + lane clone)。
///
/// lanes portless: 旧 `POST /api/lanes` body。 daemon repo-proxy ask `lane_create` の payload
/// として `dispatch_repo_method` が serde で deserialize する。
#[derive(Debug, Deserialize)]
pub struct CreateLaneReq {
    // doc 44 P2: `kind` を撤去。lane に種別は無く、作成できるのは名前付き lane だけ
    // （開発起点は repo 起動時に予約名で自動生成される）ため、指定する余地が無い。
    // 旧 client が `kind: "performer"` を送っても unknown field として無視される。
    /// lane 名 (人間可読、 `LaneAddress.name` に入る)
    pub name: String,
    /// LaneStand: "echoes" (default) or "shell"
    #[serde(default)]
    pub stand: Option<String>,
    /// 既存 worktree path。 Some なら直接 cwd として使う、 None なら branch 指定で lane clone を実行する。
    #[serde(default)]
    pub cwd: Option<String>,
    /// Phase 3-A: lane clone する branch 名。 cwd が None で branch が Some の時、
    /// `vp lane new <name> <branch>` を repo 内で実行して performer dir を作成、 そこに Lane を spawn する。
    #[serde(default)]
    pub branch: Option<String>,
    /// worktree の分岐元 ref の override (co-evolution #2)。未 push の local branch も可。
    /// 省略時は performer-files.kdl の base-ref → origin/HEAD → main。
    #[serde(default)]
    pub base: Option<String>,
    /// lane の claude model alias (co-evolution #1、例: 'opus' / 'sonnet' / 'claude-fable-5')。
    /// spawn 前に `engine_model` へ永続し、Act I spawn / respawn / Act II engine が共有する。
    /// 省略時は config の `default-lane-model`（未設定なら Opus）にフォールバックして record する。
    #[serde(default)]
    pub model: Option<String>,
}

/// Daemon 入口（Unison `daemon-control.lanes/create`）の引数を [`CreateLaneReq`] に写す (= calc)。
///
/// doc 44 §9.4 の統合で、daemon 側の `RepoManagerCapability::create_lane` は
/// **自前の実装を持たず**本 module の [`create_performer_orchestrated`] を呼ぶ薄い adapter に
/// なった。その境界で唯一発生するのが「(name, branch, stand) → `CreateLaneReq`」の写像で、
/// ここが黙ってズレると **GUI から作った lane だけ branch / stand が効かない**という
/// 経路差が復活する。純関数に切り出して往復を test で固定する。
///
/// `cwd` / `base` / `model` が None なのは Daemon 入口がそれらを受け取らないため
/// （= 既定の lane clone に落ちる。旧 `create_lane` と同じ範囲）。
pub(crate) fn build_create_lane_req(name: &str, branch: &str, stand: &str) -> CreateLaneReq {
    CreateLaneReq {
        name: name.to_string(),
        stand: Some(stand.to_string()),
        cwd: None,
        branch: Some(branch.to_string()),
        base: None,
        model: None,
    }
}

/// lane descriptor / lifecycle を db に永続する時の repo key。
///
/// `AppState.repo_dir` は生パス（`CapabilityConfig` にそのまま入る）だが、db の
/// `repo_path` 列と daemon の registry key は**正規化済パス**なので、境界で 1 回だけ畳む。
/// call site に任せると 1 箇所忘れて「boot load では引けない行」が無音で生まれる
/// （doc 44 §10.4 の帳簿 key と同じ罠）。
fn lane_db_key(state: &AppState) -> String {
    crate::capability::normalize_path_key(std::path::Path::new(&state.repo_dir))
}

/// intent-first bracket の enter（doc 24 §4.6 / doc 44 §9.4）: descriptor +
/// `lifecycle=Provisioning` を **provision（lane clone）より先に**永続する。
///
/// crash が provision の途中で起きても「provisioning が残る」ので、boot reconcile が
/// ground の有無で heal できる。旧 daemon 側 `create_lane` だけが持っていた振る舞いで、
/// 統合で全入口（MCP / CLI / watcher / GUI）に効くようになった。
///
/// 失敗は warn のみ（db が無い / 書けない時に lane 作成そのものを止めない — 永続の欠落は
/// 「再起動後に reconcile 対象から漏れる」degrade で、作成自体は成立する）。
async fn persist_lane_intent(state: &Arc<AppState>, key: &str, info: &LaneInfo) {
    let Some(db) = &state.vpdb else { return };
    let addr = info.address.to_string();
    if let Err(e) = db.upsert_lane(key, info).await {
        tracing::warn!("lane descriptor の db 永続に失敗 (作成は継続): {}", e);
    }
    if let Err(e) = db
        .upsert_lane_lifecycle(
            key,
            &addr,
            crate::repo::lanes_state::LaneLifecycle::Provisioning.as_str(),
        )
        .await
    {
        tracing::warn!("lane_lifecycle=provisioning の db 永続に失敗: {}", e);
    }
}

/// intent-first bracket の exit（成功）: 確定 descriptor で上書きし `lifecycle=Ready` にする。
///
/// enter 時点の descriptor は cwd が**予測値**（clone 前なので実 path が無い）なので、
/// ここで実測値に置き換える。lifecycle は ground（worktree）の生死であって PtySlot の
/// 生死ではないため、spawn 失敗で `LaneState::Dead` になった lane も ground があるなら
/// `Ready` を書く（PtySlot は restart で復帰できる = ground は正常）。
///
/// ⚠️ **永続する descriptor は process liveness を名乗らない**（`state=Spawning` / `pid=None`
/// に正規化する）。descriptor は「この lane が在るという意図」の記録で、動いているかどうかは
/// `LanePool` にしか無い。`Running` のまま焼くと、repo が起動していない間 boot load 済の
/// 行が「稼働中」を主張し、`vp lane cleanup` の稼働 guard（`host::liveness`）が
/// **永久に見送りを止める**（doc 44 §7.5 が `Spawning` を稼働に数えないのと同じ理由）。
async fn persist_lane_ready(state: &Arc<AppState>, key: &str, info: &LaneInfo) {
    let Some(db) = &state.vpdb else { return };
    let addr = info.address.to_string();
    let descriptor = LaneInfo {
        state: LaneState::Spawning,
        pid: None,
        ..info.clone()
    };
    if let Err(e) = db.upsert_lane(key, &descriptor).await {
        tracing::warn!("lane descriptor (確定) の db 永続に失敗: {}", e);
    }
    if let Err(e) = db
        .upsert_lane_lifecycle(
            key,
            &addr,
            crate::repo::lanes_state::LaneLifecycle::Ready.as_str(),
        )
        .await
    {
        tracing::warn!("lane_lifecycle=ready の db 永続に失敗: {}", e);
    }
}

/// lane descriptor + lifecycle を db から回収する（rollback / delete 共用）。
async fn discard_lane_rows(state: &Arc<AppState>, key: &str, addr: &LaneAddress) {
    let Some(db) = &state.vpdb else { return };
    let addr_str = addr.to_string();
    let _ = db.delete_lane(key, &addr_str).await;
    let _ = db.delete_lane_lifecycle(key, &addr_str).await;
}

/// create 失敗時の後始末を **1 つの動詞**に畳む（pool の reservation + db の intent）。
///
/// 失敗経路は 4 本（clone task panic / clone 失敗 / spawn task panic / spawn 失敗 rollback）
/// あり、そこで落とすものが 2 つある。別々に書くと片方だけ足した経路が必ず生まれ、
/// 「placeholder が leak してその addr の lane が二度と作れない」か「拒否されたはずの
/// lane が db に残る」のどちらかが**無音で**起きる（1 辺が 2 仕事をしている罠）。
async fn abort_lane_creation(state: &Arc<AppState>, key: &str, addr: &LaneAddress) {
    state.lane_pool.write().await.remove(addr);
    discard_lane_rows(state, key, addr).await;
}

/// Performer Lane create core orchestration (Phase 3-A: lane clone + PtySlot spawn)。
///
/// lanes portless (doc 27 §3.4.5): 旧 `POST /api/lanes` の core を抽出し、 全 trigger
/// (MCP `add_performer`/`flow_handoff` / CLI `vp flow handoff` / lane watcher) が Daemon
/// repo-proxy ask `lane_create` 経由で共有する core logic に。 `delete_lane_orchestrated` /
/// `restart_lane_orchestrated` と対称 (repo HTTP route + axum handler は撤去)。
///
/// **doc 44 §9.4 の統合後、lane 作成の実装はこの関数 1 本**。旧 daemon 側
/// `RepoManagerCapability::create_lane`（worktree provision + descriptor 永続のみで
/// PtySlot は watcher 経由という別実装）は本関数を呼ぶ adapter に畳んだ。
/// repo がプロセスだった頃の「ground を provision する唯一の主体は daemon」(doc 24 §5.3) は
/// fold-in で daemon = repo = 同一プロセスになった時点で意味を失っており、
/// 分かれている理由が消えていた。
///
/// 流れ:
/// 1. 入力 validation (name の gate = `validate_performer_name` / model 名)
/// 2. reserve (LanePool の Spawning placeholder) + **intent-first の descriptor 永続**
/// 3. cwd 決定:
///    - `req.cwd` Some → そのまま使う
///    - `req.branch` Some → `vp lane new <name> <branch>` subprocess で performer dir 作成
///    - 両方 None → `<git-user>/<sanitized-name>` を auto-derive して lane clone
/// 4. PtySlot::spawn で実 PTY 起動 (LaneStand 別 command builder 経由)
/// 5. LanePool に insert (state=Running、 pid 付き) + descriptor 確定 / `lifecycle=Ready`
///
/// 戻り値: 成功 `LaneInfo` / 失敗 `String`（旧 HTTP の CONFLICT="already exists" 等 error message
/// を保持。 unison error frame `{"error":..}` 経由で caller に Err 化される）。
///
/// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process + lane clone 連動)
pub(crate) async fn create_performer_orchestrated(
    state: &Arc<AppState>,
    req: CreateLaneReq,
) -> Result<LaneInfo, String> {
    // 入力 validation。
    //
    // doc 44 §9: 名前の gate は **`validate_performer_name` 1 本**（空文字 / 文字 allowlist /
    // 先頭文字 / 予約名）。P2 はここに予約名チェックを直書きで足したが、同じ意図の判定が
    // 奥の `new_performer_in` にもあり、**経路ごとに効く範囲が違う**状態だった（§6.5）。
    //
    // 入口で全部弾くと、reserve も disk dir も db 行も作らずに済む（下の model 検証を
    // reserve より前に置いているのと同じ理由 — bad input で副作用を残さない）。
    crate::lane::config::validate_performer_name(req.name.trim())?;
    // model 名の検証は reserve / clone より**前**に置く (bad input で reservation も disk dir も
    // 作らない = orphan worktree / placeholder leak を構造的に防ぐ)。永続 (engine_model::record)
    // は addr が要るので clone 後まで遅らせる。
    if let Some(model) = req.model.as_ref().filter(|m| !m.trim().is_empty())
        && !crate::lane::engine_model::is_valid_model(model.trim())
    {
        return Err(format!("model 名が不正です: {:?}", model.trim()));
    }

    // repo_id: AppState の repo_dir から basename
    let repo_id = std::path::Path::new(&state.repo_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let addr = LaneAddress::performer(&repo_id, &req.name);
    // doc 11 PR-B: stand 識別子 String 化。 wire format は新 stand 名 (echoes / shell / tmux 等)
    // をそのまま受け取る。 未指定なら config の `default_stand` (未設定なら "echoes" fallback、
    // PR-pre2 / VP-118 で "hd" → "echoes" rename)。
    //
    // Config::load() は他 handler でも ad-hoc に呼ばれており (server.rs:49, mcp.rs:2577 等)、
    // SSOT は config.toml ファイル自体。 AppState に持たせない pattern を踏襲。
    let config = crate::config::Config::load().unwrap_or_default();
    let stand: String = req
        .stand
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| config.default_stand_or_echoes().to_string());

    // 重複チェック + 予約 (check-and-reserve、二重 dispatch race の根治)。
    //
    // 旧実装は read lock で存在確認するだけで即 lock 解放 → 登録 (末尾 pool.insert、~400 行後) は
    // clone+spawn (~1-2s) を跨いだ**後**。この間 pool.lanes に addr が無いため、同 lane への 2 個目
    // の lane_create が dedup を素通りして 2 個目の PtySlot を fork し、片方が orphan 化する
    // (= 1 worktree に claude 2 セッション、bug memory mem_1Ccyoa6PuE9z5yKuqxuDWr)。
    //
    // 特に filesystem watcher (repo_manager_capability の run_lane_watcher) は、この関数自身の
    // lane clone が作った disk dir を検知して 2 個目の lane_create を loopback 発火するため、
    // **単独 dispatcher でも** race に入る (watcher が実質 2 人目の dispatcher)。
    //
    // 根治: チェックと同じ write lock 内で Spawning placeholder を即 insert して addr を claim する。
    // 2 個目の ask はこの placeholder を見て "already exists" で弾かれる (watcher 側は既存の
    // "already exists" substring 判定で silent に飲み込む)。placeholder は build_lanes_snapshot が
    // intended performer に使う Spawning(pid=None) と同型 = presence を足す方向で、console teardown
    // 教訓 (mem: performer console snapshot teardown) の安全側。末尾で実 state に置換する。
    //
    // ⚠️ reservation lifecycle: reserve 後〜末尾の実 insert 前に return する**全経路**で reservation
    // を除去しないと placeholder が leak し、その addr の lane が二度と作れなくなる (別種の regression)。
    // 該当は clone/spawn の失敗系 4 経路 (clone task join panic / clone 実行失敗 / spawn task join
    // panic / spawn 実行失敗 rollback)。bad input (model 名不正) は reserve **前**に弾くので対象外。
    // 後始末は [`abort_lane_creation`] 1 本に畳んである (db intent と同時に落とす)。
    let lane_id = crate::lane::lane_id::load_or_create(&addr.repo, &req.name);
    {
        let mut pool = state.lane_pool.write().await;
        if pool.get(&addr).is_some() {
            return Err(format!("Lane {} already exists", addr));
        }
        pool.insert(LaneInfo {
            id: lane_id.clone(),
            address: addr.clone(),
            state: LaneState::Spawning,
            stand: stand.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: String::new(), // clone 前で未確定。末尾の実 insert で確定 cwd に置換される
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        });
    }

    // doc 24 §4.6 intent-first bracket (enter): descriptor + lifecycle=Provisioning を
    // **provision より先に**永続する。cwd は lane clone の決定的 path
    // (`<repo>/.vp/lanes/<name>`) なので clone 前に予測でき、explicit cwd ならそれ自体。
    // 実 path は clone 後に [`persist_lane_ready`] が上書きする。
    //
    // doc 44 §9.4 の統合で daemon 側 `create_lane` から移設した。旧構成ではこの bracket が
    // GUI 経由の create にしか効かず、MCP / CLI / watcher で作った lane は **descriptor が
    // 一度も db に載らなかった** (= 経路ごとの差。boot reconcile の射程外だった)。
    let db_key = lane_db_key(state);
    let intended_cwd = req.cwd.clone().unwrap_or_else(|| {
        crate::lane::config::repo_lanes_dir(std::path::Path::new(&state.repo_dir))
            .join(&req.name)
            .to_string_lossy()
            .into_owned()
    });
    persist_lane_intent(
        state,
        &db_key,
        &LaneInfo {
            id: lane_id.clone(),
            address: addr.clone(),
            // process liveness: PtySlot は未起動 (= lifecycle と別軸)
            state: LaneState::Spawning,
            stand: stand.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: intended_cwd,
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        },
    )
    .await;

    // Phase 4-X / R5: cwd 決定 ── 優先順位 explicit cwd > lane clone (branch 明示 or auto-derive)。
    //
    // 旧 fallback (`else { state.repo_dir }` で Conductor と同 worktree を share) は撤廃。
    // 理由: UI から name="sub" だけ入力した場合、 silent に Conductor と同 dir を共有してしまい、
    // 「Performer = 隔離 worktree」の mental model が崩れていた (race condition の温床)。
    //
    // 新規約: branch が None の時は `git config user.name` から prefix を取り、
    // `<user>/<sanitized-name>` 形式の branch を auto-derive して必ず lane clone を実行する。
    // explicit に同 dir を share したい場合は API caller が `cwd` を明示的に指定する。
    // F.8 B Convergent: cwd 決定経路を tag 付き で track。 spawn 失敗時の rollback 可否を判定する。
    // - `lane clone` 経路 (= 自分が作った disk dir): spawn 失敗時 rollback (disk dir 削除)
    // - `explicit cwd` 経路 (= user / watcher が既存 dir を渡してきた): rollback しない (= dir 保護)
    let (cwd, was_lane_cloned) = if let Some(c) = req.cwd {
        (c, false)
    } else {
        let branch = req
            .branch
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                derive_default_branch(std::path::Path::new(&state.repo_dir), &req.name)
            });
        let repo_dir = state.repo_dir.clone();
        let name = req.name.clone();
        let branch_for_log = branch.clone();
        let base = req.base.clone().filter(|s| !s.trim().is_empty());
        let join = tokio::task::spawn_blocking(move || {
            crate::lane::commands::new_performer_in(
                std::path::Path::new(&repo_dir),
                &name,
                &branch,
                false,                                      // force=false
                crate::lane::commands::Isolation::Worktree, // repo は worktree default
                base.as_deref(),
            )
        })
        .await;
        let result = match join {
            Ok(r) => r,
            Err(e) => {
                // 後始末 (clone task panic で早期 return、placeholder / db intent leak 防止)。
                abort_lane_creation(state, &db_key, &addr).await;
                return Err(format!("lane task join: {}", e));
            }
        };
        // lane::commands::new_performer_in は performer dir 既存 + force=false の時に
        // 「パフォーマー '<name>' は既に存在します」を返す。 error message をそのまま流し、
        // caller (MCP add_performer 等) が "既に存在"/"already exists" を substring 判定可能にする。
        let path_buf = match result {
            Ok(p) => p,
            Err(e) => {
                // 後始末 (clone 失敗で早期 return)。disk dir は new_performer_in が
                // 作れなかった or 途中失敗なので、ここで残るのは pool の placeholder と
                // db の intent だけ = [`abort_lane_creation`] が両方落とす。
                abort_lane_creation(state, &db_key, &addr).await;
                return Err(format!(
                    "lane clone failed (branch={}): {}",
                    branch_for_log, e
                ));
            }
        };
        tracing::info!(
            "Performer Lane clone: name={} branch={} dir={}",
            req.name,
            branch_for_log,
            path_buf.display()
        );
        (path_buf.to_string_lossy().into_owned(), true)
    };

    // co-evolution #1: model 指定を spawn 前に永続する。 build_stand_command が Act I claude の
    // `--model` として読み、 respawn（repo restart）や Act II engine も同じ file を共有する。
    // 検証は関数冒頭 (reserve 前) で済んでいるので、ここは永続のみ。 IO 失敗は best-effort warn
    // （claude default に degrade するだけで lane 作成は続行）。
    //
    // doc 54 §8-11: 明示 model > config `default-lane-model` > **無記録**（engine 側の
    // user 既定に委ねる — 旧「未設定なら Opus 強制」は撤去）。CLI (persist_lane_model) と
    // 同じ既定規則を共有し、Act I/II 両方に効く（model は per-lane 単一真実源）。
    if let Some(model) = crate::lane::engine_model::resolve_default(
        req.model.as_deref(),
        config.default_lane_model(),
    ) {
        let lane_label = crate::repo::stand_spawner::lane_label(&addr);
        if let Err(e) = crate::lane::engine_model::record(&addr.repo, lane_label, &model) {
            tracing::warn!(
                "engine_model 永続失敗（claude default で spawn）: addr={} model={} err={}",
                addr,
                model,
                e
            );
        }
    }

    // doc 54 §3.1 / §8-11: 生成の既定レンズ（**純粋計算** — chat_capable な engine は Chat、
    // shell 等は Tui = 定義）。registry への永続は**実 insert 確定後**（下の stand_store::record
    // と同じ規律）: spawn 前に書くと create 失敗の rollback（abort_lane_creation）が回収せず、
    // 孤児 registry の stale な stand が同名再作成時に engine を取り違えさせる（moody 指摘
    // 2026-07-25）。Tui 経路の初回 spawn は registry 不在でも安全 — build_stand_command の
    // root entry 解決は不在時に引数の stand へ fallback する。
    let root_act = crate::lane::session_registry::default_act_for_stand(&stand);

    // PtySlot::spawn は openpty + spawn_command の OS syscall でブロッキング。
    // Phase review fix #2: tokio worker thread (= async executor の OS thread) を占有しないよう spawn_blocking でラップ。
    // Phase 4-X の lane clone と同じ pattern。
    // tmux decoupling PR2: slot (login shell) + claude 注入の Rust-native spawn (design §13)。
    // build_stand_command も closure 内で呼ぶ（state file 直読みの同期 I/O を async worker から
    // 外す。PtySlot::spawn 自体が openpty + syscall でブロッキングなので同形）。
    //
    // doc 54 §8-11: root act=Chat の生成は **PTY を立てない**（chat lane は engine-less
    // idle が正常形 — engine は初回 submit / demand で lazy spawn。boot の chat 分岐
    // = lane_spawn_actor と同じ形）。
    let (lane_state, pid) = if root_act == crate::lane::session_registry::SessionAct::Chat {
        tracing::info!(
            "Performer Lane created as chat (PTY skip): addr={} stand={} cwd={}",
            addr,
            stand,
            cwd
        );
        (LaneState::Running, None)
    } else {
        let stand_for_spawn = stand.clone();
        let addr_for_spawn = addr.clone();
        let cwd_for_spawn = cwd.clone();
        let spawn_join = tokio::task::spawn_blocking(move || {
            let cmd = crate::repo::stand_spawner::build_stand_command(
                &stand_for_spawn,
                &addr_for_spawn,
                std::path::Path::new(&cwd_for_spawn),
            );
            crate::repo::stand_spawner::spawn_stand(&cmd, 120, 48)
        })
        .await;
        let spawn_result = match spawn_join {
            Ok(r) => r,
            Err(e) => {
                // 後始末 (spawn task panic で早期 return、placeholder / db intent leak 防止)。
                abort_lane_creation(state, &db_key, &addr).await;
                return Err(format!("PtySlot spawn task join: {}", e));
            }
        };
        match spawn_result {
            Ok((slot, term_rx)) => {
                let pid = slot.pid();
                let mut pool = state.lane_pool.write().await;
                // Stage 1 (ADR-0001): TermAttach も同時 spawn (race フリー、 Conductor 経路と統一)
                // session=None = root（performer の boot slot も lane の代表、doc 46 P5）。
                pool.insert_pty_slot(addr.clone(), None, slot, term_rx);
                tracing::info!(
                    "Performer Lane spawned: addr={} stand={} cwd={} pid={}",
                    addr,
                    stand,
                    cwd,
                    pid
                );
                (LaneState::Running, Some(pid))
            }
            Err(e) => {
                // F.8 B Convergent: spawn 失敗時の rollback ポリシー
                // - was_lane_cloned=true (自分で disk dir を作った): rollback で dir 削除 + 500 早期 return
                //   (= 中間状態 disk-only Lane を残さない)
                // - was_lane_cloned=false (explicit cwd): dir は user / watcher 由来なので保護、
                //   Dead state で LanePool に record (= sidebar に失敗が見える、 後で手動 retry 可能)
                if was_lane_cloned {
                    tracing::warn!(
                        "Performer Lane spawn failed → rollback (lane clone で作った disk dir を削除): addr={} cwd={}: {}",
                        addr,
                        cwd,
                        e
                    );
                    let cwd_for_rm = cwd.clone();
                    let rm_result =
                        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&cwd_for_rm))
                            .await;
                    match rm_result {
                        Ok(Ok(())) => tracing::info!("rollback: disk dir 削除成功 cwd={}", cwd),
                        Ok(Err(rm_err)) => tracing::warn!(
                            "rollback: disk dir 削除失敗 (orphan dir 残置) cwd={}: {}",
                            cwd,
                            rm_err
                        ),
                        Err(join_err) => {
                            tracing::warn!("rollback: rm task join 失敗 cwd={}: {}", cwd, join_err)
                        }
                    }
                    // 後始末 (spawn 失敗 + disk rollback で早期 return)。disk dir を消したので
                    // descriptor / lifecycle も残してはいけない (= 存在しない ground を指す行)。
                    abort_lane_creation(state, &db_key, &addr).await;
                    return Err(format!(
                        "Performer Lane spawn failed (rollback executed): {}",
                        e
                    ));
                }
                tracing::warn!(
                    "Performer Lane spawn failed (explicit cwd、 Dead で record): addr={} cwd={}: {}",
                    addr,
                    cwd,
                    e
                );
                (LaneState::Dead, None)
            }
        }
    };

    // I1: performer の安定 id は reserve 時に load_or_create 済 (address = repo + name で決まる
    // 決定的な値なので、reservation・intent・確定 descriptor の 3 者で同じものを使う)。
    let info = LaneInfo {
        id: lane_id,
        address: addr.clone(),
        state: lane_state,
        stand: stand.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // create 時点では git 状態は registry に保存しない、 GET 時に都度 performer_status() で取得
        performer_status: None,
        cc_session_id: None,
        sessions: None,
        engine_session_id: None,
        engine_stand: None,
        flow_state: None,
    };

    {
        let mut pool = state.lane_pool.write().await;
        pool.insert(info.clone());
    }

    // doc 24 §4.6 intent-first bracket (exit): ground は在るので確定 descriptor + lifecycle=Ready。
    // spawn 失敗の Dead 登録 (explicit cwd 経路) もここを通る — lifecycle は ground の生死で、
    // PtySlot の生死は `LaneInfo.state` が持つ別軸だから (dead を書くと boot reconcile が
    // 実在する worktree を「消えた」扱いにする)。
    persist_lane_ready(state, &db_key, &info).await;

    // per-lane stand 永続（mem_1Cd4M7i5Enp3HHMLVYayRe）: repo 再起動後の boot bootstrap が
    // この記録を読んで同じ stand で respawn する（従来は全 performer が default_stand に
    // 倒れていた）。全 create 入口（GUI watcher / MCP / CLI）が本関数を通る choke point。
    // ⚠️ 位置は**実 insert 確定後**（moody 指摘）: dedup reject / clone・spawn 失敗の rollback
    // 経路で record すると、既存 lane の永続 stand を「作れなかった create」が上書きし得る +
    // 失敗系テストが実 state dir に file を残す。lane が pool に実在化した時だけ記録する
    // （Dead 登録も disk に lane が実在 = 次回 boot respawn の対象なので記録する）。
    // 失敗は warn のみ（記録欠落は「再起動後 default に戻る」従来挙動に退化するだけ）。
    if let Err(e) = crate::lane::stand_store::record(&repo_id, &req.name, &stand) {
        tracing::warn!(
            "lane stand の永続に失敗（再起動後は default に fallback）: addr={addr} stand={stand}: {e}"
        );
    }

    // doc 54 §8-11: 生成の既定レンズを registry に**明示的に書く**（仕込み = explicit intent。
    // 以降の boot = lane_spawn_actor がこれを honor する）。⚠️ 位置は stand_store と同じく
    // **実 insert 確定後** — 「作れなかった create」が state file を残さない（rollback 経路は
    // 上で早期 return 済み。Dead 登録は disk に lane が実在 = boot respawn の対象なので書く）。
    // 失敗は warn のみ（欠落時の読み手 fallback = Tui なので、次 boot が Tui で立つ従来形に退化）。
    {
        let lane_label = crate::repo::stand_spawner::lane_label(&addr);
        if let Err(e) =
            crate::lane::session_registry::set_root_act(&addr.repo, lane_label, &stand, root_act)
        {
            tracing::warn!(
                "既定レンズの永続失敗（次 boot は Tui 相当に退化）: addr={addr} act={root_act:?}: {e}"
            );
        }
    }

    // wiremsg Stage 0: Lane 追加を SystemEvent::Lane(Diff::Add) で発火する。
    // これを購読する producer (server.rs) が LanePool 全 snapshot を retained topic
    // (`process/star-platinum/state/lanes`) に republish し、vp-app の "lanes" 購読へ
    // push される。delete 経路 (delete_lane_orchestrated) は Diff::Remove を発火済だが、
    // create 経路はこれが欠けており add_performer 後に sidebar が追従しなかった (Stage 1
    // consumer dogfood で発覚)。
    if let Err(e) = state.system_event_tx.send(SystemEvent::Lane(Diff::Add {
        payload: info.clone(),
    })) {
        tracing::warn!(
            "SystemEvent::Lane(Diff::Add) broadcast failed: addr={} err={}",
            addr,
            e
        );
    }

    Ok(info)
}

/// VP-124 Phase 1: Lane delete orchestration の戻り値。
///
/// 全 trigger (HTTP DELETE / MCP `delete_performer` / `vp lane rm` CLI) が共有する成功 payload。
#[derive(Debug, Serialize)]
pub struct DeletedLaneInfo {
    /// Display 形 ("<repo>/performer/<name>")
    pub address: String,
    /// PtySlot drop 直前の child pid (= killed)
    pub pid: Option<u32>,
    /// lane workspace cleanup の結果 (None = cleanup=false で skip)
    pub cleanup_status: Option<String>,
}

/// VP-124 Phase 1: Lane delete orchestration の error。
///
/// HTTP handler はこれを 4xx ステータスに mapping。 MCP / CLI も同じ enum を消費。
#[derive(Debug, thiserror::Error)]
pub enum DeleteLaneError {
    /// architecture rule: Conductor Lane は repo lifetime 紐付きのため削除不可。
    #[error("Conductor Lane is fixed per repo and cannot be deleted (use repo shutdown instead)")]
    ConductorCannotBeDeleted,
    /// LanePool に該当 address の entry なし (idempotent re-call で発生)。
    #[error("Lane not found: {0}")]
    LaneNotFound(LaneAddress),
}

/// VP-124 Phase 1: Lane delete の 3-step orchestration を関数化。
///
/// 全 trigger (HTTP DELETE / MCP `delete_performer` / `vp lane rm` CLI / future Phase 3 FSEvents
/// watcher) が共有する core logic。 既存 `delete_handler` から extract、 同時に **欠落していた
/// tmux session kill + SystemEvent broadcast を補完** (= bug fix 兼 refactor)。
///
/// ## 動作
///
/// 1. **architecture rule check**: Conductor は削除拒否 (`DeleteLaneError::ConductorCannotBeDeleted`)
/// 2. **Phase 1 (in-memory authoritative mutation)**: `LanePool::remove` で LaneInfo + PtySlot を
///    drop (PtySlot::Drop で child kill + wait)
/// 3. **Phase 2a (state file GC)**: `console_mode::clear` + `session_registry::clear` で lane 単位
///    state file を削除 (best-effort。 残すと同名 lane 再作成時に旧 mode / 旧 session が蘇る)
/// 4. **Phase 2b (filesystem cleanup)**: `cleanup=true` なら `lane::remove_performer_in` で workspace
///    dir 削除 (best-effort、 既存挙動踏襲)
/// 5. **Phase 3 (broadcast)**: `SystemEvent::Lane(Diff::Remove)` を broadcast → sidebar 即時反映
///    (既存挙動では欠落していた → sidebar refresh 不全の根本原因)
///
/// ## 契約
///
/// - **idempotent**: 二度呼ばれても 2 回目は `LaneNotFound` を返す、 sidebar 状態に矛盾なし
/// - **best-effort cleanup**: tmux / lane 失敗は warn log のみ、 LanePool 削除は authoritative success
/// - **失敗時**: Phase 1 で fail (Conductor / NotFound) なら early return、 Phase 2 以降の partial failure
///   は `DeletedLaneInfo` の field で結果を伝える
///
/// 関連: VP-124 (PR-Phase 1 設計)、 mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane lifecycle)
pub async fn delete_lane_orchestrated(
    state: &Arc<AppState>,
    addr: LaneAddress,
    cleanup: bool,
) -> Result<DeletedLaneInfo, DeleteLaneError> {
    // architecture rule: 開発起点 lane は repo lifetime 紐付きのため削除不可
    if addr.is_root() {
        return Err(DeleteLaneError::ConductorCannotBeDeleted);
    }

    // Phase 1: in-memory authoritative state mutation。
    // PtySlot は LanePool 内部で保持されており、 remove() の戻り値 LaneInfo と一緒に
    // pool 外へ移動 → drop されるタイミングで child kill + wait される (PtySlot::Drop)。
    let info = {
        let mut pool = state.lane_pool.write().await;
        pool.remove(&addr)
            .ok_or(DeleteLaneError::LaneNotFound(addr.clone()))?
    };
    let pid = info.pid;

    tracing::info!(
        "Lane delete orchestrated: addr={} pid={:?} (PtySlot dropped → child killed)",
        addr,
        pid
    );

    // terminal pump を lane 削除に追随させる（doc 53 R2: 動詞の末尾 = reconcile の契機）。
    // pool から lane が消えた後なので live slot = ∅ → 全 pump が撤去される。task 自体は
    // PtySlot drop の broadcast Closed でも自壊するが、台帳 entry の掃除は reconcile が担う。
    crate::repo::unison_server::reconcile_terminal_pumps(state, &addr.to_string()).await;

    // tmux decoupling PR2: 旧 Phase 2a (tmux session kill) は退役 — claude は PtySlot の
    // 子なので Phase 1 の remove (= PtySlot drop) で完全停止する（第 2 の生存木は無い）。

    // Phase 2a: lane-scoped state file の一元 GC (best-effort)。lane 削除後に file が残ると、
    // 同名 lane を作り直した時に旧 mode / 旧 session（会話 id）/ 旧 replay 等が蘇る
    // (ghost file の state leak)。cleanup flag には従わない — workspace dir と違い state file は
    // lane が消えた時点で意味を失う (残す価値がない)。破棄リストは CLI 側 (`remove_performer`)
    // と共有する `clear_lane_state` に一元化 (2 経路が別リストを持って片方に漏れる ghost leak を
    // 構造的に断つ — replay_log / terminal_replay / lane_id はここが従来欠落していた)。
    let lane_label = crate::repo::stand_spawner::lane_label(&addr).to_string();
    crate::lane::commands::clear_lane_state(&addr.repo, &lane_label);

    // Phase 2a': db の descriptor + lifecycle も回収する (best-effort)。
    //
    // doc 44 §9.4 の統合で create が **全入口**で descriptor を書くようになった対の後始末。
    // 書き手だけ増やして消し手を据え置くと、削除した lane の行が db に残り続け、次の boot で
    // `lane_registry` に ghost として載る (= `reconcile_lanes` が毎回 dead に倒す仕事を増やし、
    // `remove_repo` の worktree reclaim が実在しない lane を掃除しに行く)。
    // 旧構成でもこの非対称は在ったが、書き手が GUI 経由だけだったため表に出にくかった。
    discard_lane_rows(state, &lane_db_key(state), &addr).await;

    // Phase 2b: lane workspace dir cleanup (best-effort、 cleanup=true 時のみ)。
    // 既存挙動踏襲、 直 lib call (`crate::lane::commands::remove_performer_in`)。
    // 注意: `spawn_blocking` closure は `repo_name` / `name` のみ move、 `addr` は capture
    // されないので後続 match arm の `tracing` で参照可能 (= compile time 保証)。
    // doc 44 P2: 旧 `if let Some(name) = info.address.name` は name が Option だった時代の形。
    // フラット化で常に在るため、開発起点でないこと（= 上で弾き済）だけが条件になった。
    let cleanup_status = if cleanup {
        let name = info.address.name.clone();
        // repo-local lane refactor PR 1: remove_performer_in は repo_root: &Path を受け取る。
        // sidebar delete trigger は dual-read で repo-local + legacy global 両 path 対応。
        let repo_root = std::path::PathBuf::from(&state.repo_dir);
        let result = tokio::task::spawn_blocking(move || {
            crate::lane::commands::remove_performer_in(&repo_root, &name)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                tracing::info!("lane remove 成功: {}", addr);
                Some("cleaned".to_string())
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "lane remove 失敗 (lane は削除済、 dir 残置): {}: {}",
                    addr,
                    e
                );
                Some("dir_retained_remove_failed".to_string())
            }
            Err(e) => {
                tracing::warn!("lane task join: {}", e);
                Some("dir_retained_join_failed".to_string())
            }
        }
    } else {
        None
    };

    // Phase 3: SystemEvent::Lane(Diff::Remove) broadcast → sidebar 即時反映。
    // 既存挙動では欠落していた → sidebar refresh 不全 bug の根本原因 (= 本 PR で fix)。
    // send 失敗 (= subscriber 全 drop) は warn のみ、 LanePool 削除は既に成功してるので
    // authoritative state は問題なし。
    if let Err(e) = state
        .system_event_tx
        .send(SystemEvent::Lane(Diff::Remove { id: addr.clone() }))
    {
        tracing::warn!(
            "SystemEvent::Lane(Diff::Remove) broadcast failed: addr={} err={}",
            addr,
            e
        );
    }

    Ok(DeletedLaneInfo {
        address: addr.to_string(),
        pid,
        cleanup_status,
    })
}

/// VP-131: restart の透過 retry 設定。 tmux kill + spawn の race / transient failure を
/// 吸収するため exponential backoff で 3 attempts まで自動 retry。 user click 1 回で
/// 「auto retry」 が走り、 dogfood UX で「Restart したら確実に Echoes 復活する」 を担保。
const RESTART_MAX_ATTEMPTS: u32 = 3;
const RESTART_BACKOFF_MS: [u64; 2] = [200, 500]; // attempt 0→1: 200ms、 attempt 1→2: 500ms

/// lane の現況を `SystemEvent::Lane(Diff::Update)` として daemon へ push する。
///
/// 供給 push 根治（session chip 凍結、2026-07-17 解剖）: `Diff::Update` は従来、受信側
/// （uplink / Daemon registry / vp-app）だけ実装されて **emitter が repo に存在しなかった**。
/// そのため restart や session pointer の変化が Daemon lane_registry に届かず、vp-app の
/// header（session chip 等）が repo 登録時の enrich 値で凍結していた。
/// lane を in-place mutate した後（restart 等）と `lane_session_changed`（hook 通知）から呼ぶ。
///
/// engine_session_id は focused session 規則（`refresh_engine_session_id`）でここで確定させる
/// （uplink 側 enrich は同じ lazy read の冪等な保険）。lane 不在（削除 race）と購読者ゼロ
/// （boot 直後）は正常系なので no-op / warn に留める。
///
/// doc 53 §11: **session を変える動詞の末尾でも呼ぶ**（roster の供給が snapshot 1 本に
/// なったので、これが「roster が変わった」を知らせる唯一の経路 — 撃たない動詞の変化は
/// 次の定期 snapshot まで GUI に出ない）。R2 の「動詞の末尾で reconcile」と同型の規律。
pub(crate) async fn emit_lane_update(state: &AppState, addr: &LaneAddress) {
    let Some(mut info) = state.lane_pool.read().await.get(addr).cloned() else {
        return;
    };
    info.refresh_engine_session_id();
    if let Err(e) = state
        .system_event_tx
        .send(SystemEvent::Lane(Diff::Update { payload: info }))
    {
        tracing::warn!(
            "SystemEvent::Lane(Diff::Update) broadcast failed: addr={} err={}",
            addr,
            e
        );
    }
}

/// **restart**（VP-131 / F6③ の透過 retry orchestration）: root の実体を捨てて立て直す。
///
/// doc 53 §12.3 / R3c-2: 動詞（[`LanePool::drop_root_entities`]）が実体を捨て、
/// reconcile があるべき姿に戻す。**intent（registry）は 1 bit も動かない** — 会話 id が
/// あれば `--resume` で、無ければ素で立ち直るのは reconcile が registry から決めること。
///
/// 戻り値: `{restarted, pid, attempts}` / 全 attempts 失敗で `Err`（`LaneInfo.state` は
/// reconcile が root の実体から Dead を導出済み）。
///
/// [`LanePool::drop_root_entities`]: crate::repo::lanes_state::LanePool::drop_root_entities
pub async fn restart_lane_orchestrated(
    state: &Arc<AppState>,
    addr: LaneAddress,
) -> Result<serde_json::Value, String> {
    // ⚠️ 実在確認は**ここでしかできない**: `drop_root_entities` は不在なら no-op、reconcile も
    // 「合わせる相手が居ない」で静かに返るので、guard が無いと **存在しない lane の restart が
    // 成功を返す**（旧 `restart_lane` は `Lane not found` を返していた）。動詞を薄くすると
    // 「不在」と「何もすることが無い」が同じ形になる — その区別は呼び手の責任に移る。
    {
        let mut pool = state.lane_pool.write().await;
        if !pool.contains(&addr) {
            return Err(format!("Lane not found: {addr}"));
        }
        pool.drop_root_entities(&addr);
    }
    converge_lane(state, addr).await
}

/// **Reset**（sidebar の Reset lane）: intent ごと素に戻して立て直す。
///
/// [`LanePool::reset_lane`] が registry / replay / 全実体を捨てて既定形を書き、reconcile が
/// 新しい root を bare（会話 id が無い = 継がない）で立てる。破棄に失敗した場合は
/// **何も遷移させずに** Err を返す（fresh でない中間状態を作らない）。
///
/// [`LanePool::reset_lane`]: crate::repo::lanes_state::LanePool::reset_lane
pub async fn reset_lane_orchestrated(
    state: &Arc<AppState>,
    addr: LaneAddress,
) -> Result<serde_json::Value, String> {
    state
        .lane_pool
        .write()
        .await
        .reset_lane(&addr)
        .map_err(|e| e.to_string())?;
    converge_lane(state, addr).await
}

/// 捨てたあとに **収束させる**共通部（restart / Reset が共有する）。
///
/// VP-131 の透過 retry を reconcile の上で回す: `reconcile_lane` は冪等なので、再試行は
/// **もう一度呼ぶだけ**。旧実装の all-or-nothing な retry と違い、部分的に立った slot は
/// そのまま残り、立たなかったものだけが次の attempt の対象になる（desired との差分だけを
/// 埋めるのが reconcile なので、この性質は自動的に手に入る）。
async fn converge_lane(
    state: &Arc<AppState>,
    addr: LaneAddress,
) -> Result<serde_json::Value, String> {
    let mut last_err: Option<String> = None;
    for attempt in 0..RESTART_MAX_ATTEMPTS {
        let r = crate::repo::unison_server::reconcile_lane(state, &addr).await;
        if r.failed == 0 {
            let pid = state
                .lane_pool
                .read()
                .await
                .get(&addr)
                .and_then(|i| i.pid)
                .unwrap_or(0);
            // Act II（chat lane）の restart は engine drop（lazy respawn）で終わる。ここで
            // eager に起こすのは「新品になった」feedback を早く出すため（resume の開始も早い）。
            // 失敗しても restart 自体は成功扱い、次 submit の self-heal で再試行される。
            // doc 53 R1: 分岐は root の act = registry 直読（実在 check は従来どおり pool）。
            let is_chat = {
                let pool = state.lane_pool.read().await;
                pool.contains(&addr)
                    && pool.root_act(&addr) == crate::lane::session_registry::SessionAct::Chat
            };
            if is_chat {
                let mut pool = state.lane_pool.write().await;
                if let Err(e) = pool.ensure_chat_engine(&addr, None, &state.topic_router) {
                    tracing::warn!(
                        "restart_lane: chat engine eager spawn 失敗（次 submit で再試行）: {e}"
                    );
                }
            }
            tracing::info!(
                "Lane restart OK: addr={} new_pid={} attempts={}",
                addr,
                pid,
                attempt + 1
            );
            // 供給 push 根治: restart は pid / engine_session_id を変える in-place mutation なのに
            // 従来 Diff を emit しておらず、Daemon registry が凍結していた。
            emit_lane_update(state, &addr).await;
            return Ok(serde_json::json!({
                "restarted": addr.to_string(),
                "pid": pid,
                "attempts": attempt + 1,
            }));
        }
        tracing::warn!(
            "Lane restart attempt {}/{} failed: addr={} failed_spawns={}",
            attempt + 1,
            RESTART_MAX_ATTEMPTS,
            addr,
            r.failed
        );
        last_err = Some(
            r.last_error
                .unwrap_or_else(|| format!("{} 件の slot が立ち上がりませんでした", r.failed)),
        );
        if attempt < RESTART_MAX_ATTEMPTS - 1 {
            let backoff = RESTART_BACKOFF_MS[attempt as usize];
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }
    }

    // 全 attempts 失敗。state=Dead は reconcile の代表値導出（act=Tui × slot 無し）が既に付けている。
    Err(last_err.unwrap_or_else(|| "unknown restart failure".to_string()))
}

/// Performer name から default branch を auto-derive する。
///
/// 形式: `<git-user>/<sanitized-name>`。
///
/// - `git-user` は `git config user.name` (repo local > global の標準解決) を lowercase + sanitize したもの。
///   取得失敗・空・sanitize 後 empty なら fallback `performer` prefix を使う。
/// - `sanitized-name` は `sanitize_for_branch` で git ref 制約に合わせる。
///
/// 例: user="Mako", name="sub" → `mako/sub`
///
/// branch 未指定時の create で使う。 doc 24 §10 B-create で daemon 側 create
/// (`routes/daemon.rs` の `resolve_create_lane_args` = Unison `lanes/create` の実体) からも
/// sibling 呼びするため `pub(crate)`。
pub(crate) fn derive_default_branch(repo_root: &std::path::Path, name: &str) -> String {
    let prefix = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| sanitize_for_branch(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "performer".to_string());
    format!("{}/{}", prefix, sanitize_for_branch(name))
}

/// 文字列を git ref として安全な形に変換する。
///
/// 規則:
/// - lowercase
/// - ASCII alphanumeric + `-` `_` `.` 以外は `-` に置換
/// - 連続 `-` は 1 つに圧縮
/// - 先頭/末尾の `-` `.` は trim
///
/// ※ 完全な `git check-ref-format` 互換ではないが、 `~^:?*[\\` 等の禁止文字 + 制御文字を確実に除去する。
fn sanitize_for_branch(s: &str) -> String {
    let lowered: String = s
        .trim()
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| {
            // PR #228 review fix (Moody Blues #4): `.` を allowlist から外す。
            // git check-ref-format は連続 `.` (`..`) を禁止するため、 `v1.2.3` のような
            // 入力も `v1-2-3` として安全側に倒す。 末尾 `.lock` suffix も同時に予防。
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // 連続 `-` の圧縮
    let mut compact = String::with_capacity(lowered.len());
    let mut prev_dash = false;
    for c in lowered.chars() {
        if c == '-' {
            if !prev_dash {
                compact.push('-');
            }
            prev_dash = true;
        } else {
            compact.push(c);
            prev_dash = false;
        }
    }
    compact.trim_matches(|c| c == '-' || c == '.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_handles_typical_inputs() {
        assert_eq!(sanitize_for_branch("Mako"), "mako");
        assert_eq!(sanitize_for_branch("sub"), "sub");
        assert_eq!(sanitize_for_branch("Feat/API V2"), "feat-api-v2");
        assert_eq!(sanitize_for_branch("  spaces  "), "spaces");
        assert_eq!(sanitize_for_branch("multi---dash"), "multi-dash");
        assert_eq!(
            sanitize_for_branch("--leading-trailing--"),
            "leading-trailing"
        );
        assert_eq!(sanitize_for_branch("symbols!@#$%"), "symbols");
        assert_eq!(sanitize_for_branch(""), "");
        // PR #228 review fix (#4): `..` 連続が git ref として無効になるのを防ぐ
        assert_eq!(sanitize_for_branch("a..b"), "a-b");
        assert_eq!(sanitize_for_branch("v1.2.3"), "v1-2-3");
    }
}

#[cfg(test)]
mod core_tests {
    //! VP-13 sub-scope E: lanes.rs core 関数 smoke test。
    //!
    //! lanes portless (doc 27 §3.4.5): 旧 Axum oneshot route test は HTTP route 撤去に伴い
    //! `create_performer_orchestrated` / `build_lanes_snapshot` の直 call test に転換。
    //! 既存 `mod tests` (= helpers / sanitize_for_branch test 等) とは別 mod で配線。

    use super::*;

    /// テスト用の最小 `CreateLaneReq` builder（validation だけ叩く時に使う）。
    fn req(name: &str) -> CreateLaneReq {
        CreateLaneReq {
            name: name.to_string(),
            stand: None,
            cwd: None,
            branch: None,
            base: None,
            model: None,
        }
    }

    /// boot-window merge 後: `build_lanes_snapshot` は LanePool 由来 + `list_performers_for_repo`
    /// の disk performer (pool 未登録分) を Spawning(pid=null) で merge する。ただし merge は
    /// **実在する intended performer 限定** (`<repo_dir>/.vp/lanes/*`) なので、
    /// repo_dir に performer worktree が無ければ (build_test_app_state は repo_dir="" →
    /// list_performers_for_repo 空) LanePool が空 → snapshot も空。
    /// (performer 有りの merge 検証は repo_dir を差せる fixture が要るため follow-up)
    #[tokio::test]
    async fn build_lanes_snapshot_empty_when_pool_and_disk_empty() {
        let state = crate::repo::state::build_test_app_state(None).await;
        // LanePool 空 (LanePool::new()) + repo_dir="" (performer 不在) → 0 件
        let lanes = build_lanes_snapshot(&state).await;
        assert!(
            lanes.is_empty(),
            "LanePool 空 + performer worktree 不在なら snapshot は空 (merge は intended performer 限定)"
        );
    }

    /// 回帰固定（doc 44 §11.3）: **disk-only performer の `created_at` は呼ぶたびに変わらない**。
    ///
    /// 旧実装は `chrono::Utc::now()` を焼いており、`publish_lanes` の指紋が毎回変わって
    /// 「変わった時だけ vp-app を起こす」が無効化されていた（= その repo では 5s tick が
    /// そのまま push 源に戻り、修正の意味が消える）。
    ///
    /// 統合経路（`build_lanes_snapshot` 越し）で固定できないのは `build_test_app_state` が
    /// `repo_dir` を空で固定しており performer merge に到達しないため（既存 test の
    /// コメントも同じ制約を挙げている）。非決定性の実体はこの関数なので、ここで直接押さえる。
    #[test]
    fn ground_created_at_is_stable_across_calls() {
        let dir = std::env::temp_dir().join(format!("vp-ground-ts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();

        let first = ground_created_at(&path);
        let second = ground_created_at(&path);
        assert!(!first.is_empty(), "実在 dir では値が取れる");
        assert_eq!(
            first, second,
            "呼ぶたびに変わってはいけない（指紋が汚れる）"
        );

        // 不在 path は panic せず空文字（snapshot 生成を止めない）
        assert!(ground_created_at("/nonexistent/vp-ground-ts").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// doc 44 P2: 開発起点の予約名 `conductor` では lane を作れない。
    ///
    /// 旧 `kind != "performer"` ガードの後継。明示的に弾かないと既存 conductor lane との
    /// address 重複として「already exists」で拒否され、理由がミスリードになる
    /// （結果は安全なので "たまたま安全" に頼らないための固定）。
    ///
    /// doc 44 §9: 判定は `validate_performer_name` に一本化された（両経路で同じ gate）。
    /// message は同関数のものになるので、**予約名を名指ししていること**だけを見る。
    #[tokio::test]
    async fn create_rejects_reserved_conductor_name() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let err = create_performer_orchestrated(&state, req("root"))
            .await
            .expect_err("予約名は Err");
        assert!(
            err.contains(crate::repo::lanes_state::ROOT_LANE_NAME) && err.contains("reserved"),
            "error message が予約名である旨を伝える: {}",
            err
        );
    }

    /// `create_performer_orchestrated`: name が空白のみの場合は早期 Err を返す。
    #[tokio::test]
    async fn create_rejects_empty_name() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let err = create_performer_orchestrated(&state, req("   "))
            .await
            .expect_err("name 空白のみは Err");
        assert!(
            err.contains("empty"),
            "error message に name 必須を含む: {}",
            err
        );
    }

    /// doc 44 §9: 入口の gate は予約名だけでなく **文字 allowlist** も見る。
    ///
    /// 旧実装はここで空文字と予約名しか見ておらず、`../` や `;` は奥の
    /// `new_performer_in` が clone 段階で初めて弾いていた（= 経路によって効く範囲が違う、§6.5）。
    /// 入口に寄せたので、reserve も disk dir も作らずに拒否される。
    #[tokio::test]
    async fn create_rejects_unsafe_name_at_the_door() {
        let state = crate::repo::state::build_test_app_state(None).await;
        for bad in ["../etc/passwd", "foo bar", "foo;rm", ".hidden", "-leading"] {
            let err = create_performer_orchestrated(&state, req(bad))
                .await
                .expect_err("不正な名前は Err");
            assert!(
                err.contains("invalid performer name"),
                "入口の gate が弾く ({bad}): {err}"
            );
        }
    }

    /// 二重 dispatch race の根治 (bug memory mem_1Ccyoa6PuE9z5yKuqxuDWr): 同 addr の
    /// Spawning placeholder (reservation) が pool に居れば、2 個目の create は clone/spawn に
    /// 進む前に "already exists" で弾かれる。旧実装は登録が末尾 (spawn 後) だったため、
    /// この window で 2 個目が素通りして PtySlot を二重 fork していた。
    ///
    /// deterministic test: reservation を手で置いた状態で 2 個目 create が早期 Err になり、
    /// pool に PtySlot が生えない (= claude を fork しない) ことを確認する。
    #[tokio::test]
    async fn create_rejects_second_when_reservation_present() {
        let state = crate::repo::state::build_test_app_state(None).await;
        // repo_dir="" → repo_id="unknown"。1 個目が claim した想定の Spawning placeholder。
        let addr = LaneAddress::performer("unknown", "dup");
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Spawning,
                stand: "echoes".to_string(),
                created_at: "2026-07-13T00:00:00Z".to_string(),
                pid: None,
                cwd: String::new(),
                performer_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                engine_stand: None,
                flow_state: None,
            });
        }
        let err = create_performer_orchestrated(&state, req("dup"))
            .await
            .expect_err("Spawning reservation 中の同 addr create は Err");
        assert!(
            err.contains("already exists"),
            "reservation 衝突は 'already exists' を返す (clone/spawn 前に弾く): {}",
            err
        );
        // spawn に進んでいない = PtySlot が生えていない (placeholder のみ)。
        let pool = state.lane_pool.read().await;
        assert!(
            pool.get(&addr)
                .is_some_and(|l| l.state == LaneState::Spawning),
            "placeholder は Spawning のまま (2 個目が上書きしていない)"
        );
    }

    /// doc 44 §9.4: Daemon 入口（`daemon-control.lanes/create`）→ core の引数写像を固定する。
    ///
    /// 統合で daemon 側は自前の実装を捨てて本 module を呼ぶだけになった。残った唯一の
    /// 変換がここで、黙ってズレると「GUI から作った lane だけ branch / stand が効かない」
    /// という**経路差が復活する**（統合が壊れる時に最初に壊れる場所）。
    #[test]
    fn daemon_entry_maps_args_into_create_req() {
        let req = build_create_lane_req("sub", "mako/sub", "codex");
        assert_eq!(req.name, "sub");
        assert_eq!(req.branch.as_deref(), Some("mako/sub"));
        assert_eq!(req.stand.as_deref(), Some("codex"));
        // Daemon 入口が受け取らない 3 つは None = 既定の lane clone に落ちる（旧 create_lane と同じ範囲）。
        assert!(req.cwd.is_none(), "Daemon 入口は cwd を受け取らない");
        assert!(req.base.is_none(), "Daemon 入口は base を受け取らない");
        assert!(req.model.is_none(), "Daemon 入口は model を受け取らない");
    }

    /// doc 44 §9.4 の回帰固定: **失敗した create は db に descriptor も lifecycle も残さない**。
    ///
    /// intent-first bracket（provision より先に descriptor を永続する）は統合で daemon 側から
    /// 本 core に移設した。移設で落としやすいのは enter ではなく **exit（失敗時の rollback）**で、
    /// 落ちても成功系のテストは緑のまま「拒否された lane の行が db に残る」状態になる。
    /// vpdb=None の fixture では書き込み自体が no-op で素通りするため、ここは実 db を差す。
    #[tokio::test]
    async fn failed_create_leaves_no_lane_rows() {
        // ⑤ の session_registry 検査が vp_state_dir() を読むため隔離（実 state を見ない）。
        let _state_dir = crate::test_env::state_dir_async().await;
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();

        // git repo ではない dir を repo にする → lane clone (worktree add) が失敗する。
        let repo = std::env::temp_dir().join(format!("vp-test-rollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let repo_dir = repo.to_string_lossy().to_string();

        let state =
            crate::repo::state::build_test_app_state_with(&repo_dir, Some(db.clone()), None).await;

        let res = create_performer_orchestrated(&state, req("ghost")).await;
        assert!(
            res.is_err(),
            "git repo でない repo の clone は失敗する: {res:?}"
        );

        // ① descriptor が残っていない（= 拒否された lane が boot load で蘇らない）
        assert!(
            db.list_lanes().await.unwrap().is_empty(),
            "失敗した create の descriptor は db に残らない"
        );
        // ② lifecycle も残っていない（provisioning のまま残ると boot reconcile が heal 対象にする）
        assert!(
            db.list_lane_lifecycles().await.unwrap().is_empty(),
            "失敗した create の lane_lifecycle は db に残らない"
        );
        // ③ worktree も残っていない
        assert!(
            !crate::lane::config::repo_lanes_dir(&repo)
                .join("ghost")
                .exists(),
            "失敗した create の worktree dir は残らない"
        );
        // ④ reservation も残っていない（後始末を 1 関数に畳んだので、db と pool は必ず同時に落ちる）
        assert!(
            state.lane_pool.read().await.list().is_empty(),
            "失敗した create の Spawning placeholder は残らない"
        );
        // ⑤ 生成の既定レンズ（session_registry）も残っていない — write は**実 insert 確定後**
        //    （moody 指摘 2026-07-25: 孤児 registry の stale stand が同名再作成時に engine を
        //    取り違えさせる）。write が spawn 前へ戻る regression をここで機械検知する。
        let repo_id = repo.file_name().unwrap().to_string_lossy();
        assert!(
            !crate::lane::session_registry::exists(&repo_id, "ghost"),
            "失敗した create は session_registry file を残さない"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// doc 44 §9.4 の回帰固定（対）: bracket が **書く側**も効いていること。
    ///
    /// 上の rollback test は「消えていること」しか見ないので、intent 永続が丸ごと no-op に
    /// なっていても緑のままになる（消えたのではなく最初から無い、= 掃除の検証で主対象の
    /// 消滅だけを見る罠）。ここで enter → exit の意味論を直接押さえる。
    ///
    /// ⚠️ `create_performer_orchestrated` の**成功系**を通した end-to-end は書けない —
    /// 成功は PtySlot spawn = user の login shell を実際に起こすことを意味する
    /// (既存の core test が全て失敗系なのも同じ理由)。書く側の call site 検証は、
    /// spawn を経ない [`delete_lane_orchestrated`] 側の test（下）が担う。
    #[tokio::test]
    async fn lane_intent_bracket_writes_then_clears_rows() {
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        let state =
            crate::repo::state::build_test_app_state_with("/tmp/vp-intent", Some(db.clone()), None)
                .await;
        let key = lane_db_key(&state);
        let addr = LaneAddress::performer("vp-intent", "sub");

        let mut info = LaneInfo {
            id: Default::default(),
            address: addr.clone(),
            state: LaneState::Spawning,
            stand: "echoes".to_string(),
            created_at: "2026-07-22T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp/vp-intent/.vp/lanes/sub".to_string(), // clone 前の予測値
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };

        // enter: provision より先に descriptor と provisioning が載る（crash してもここが残る）。
        persist_lane_intent(&state, &key, &info).await;
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "descriptor が永続される: {rows:?}");
        assert_eq!(rows[0].1.address, addr);
        let lifecycles = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].2, "provisioning", "enter は provisioning");

        // exit (成功): 確定 cwd で上書きし ready にする（予測値のまま残さない）。
        info.cwd = "/tmp/vp-intent/.vp/lanes/sub-real".to_string();
        info.state = LaneState::Running;
        info.pid = Some(4242);
        persist_lane_ready(&state, &key, &info).await;
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "行は増えない (upsert): {rows:?}");
        assert_eq!(rows[0].1.cwd, info.cwd, "確定 cwd で上書きされる");
        // 永続 descriptor は process liveness を名乗らない（動いているかは LanePool にしか無い）。
        // ここが Running/pid のまま焼かれると、repo 未起動の間 boot load 済の行が「稼働中」を
        // 主張して `vp lane cleanup` の稼働 guard を恒久的に効かせてしまう。
        assert_eq!(
            rows[0].1.state,
            LaneState::Spawning,
            "descriptor は稼働を主張しない"
        );
        assert!(rows[0].1.pid.is_none(), "descriptor に pid を焼かない");
        assert_eq!(
            db.list_lane_lifecycles().await.unwrap()[0].2,
            "ready",
            "exit は ready"
        );

        // exit (失敗) / delete: descriptor も lifecycle も回収する。
        discard_lane_rows(&state, &key, &addr).await;
        assert!(
            db.list_lanes().await.unwrap().is_empty()
                && db.list_lane_lifecycles().await.unwrap().is_empty(),
            "回収後は行が残らない"
        );
    }

    /// doc 44 §9.4 の対の後始末: **lane を削除したら db の descriptor / lifecycle も消える**。
    ///
    /// 統合で create が全入口で descriptor を書くようになった分、消し手が居ないと削除済 lane の
    /// 行が db に溜まり、次の boot で `lane_registry` に ghost として載る。
    /// delete は spawn を経ないので、こちらは **実際の call site を通して**固定できる。
    #[tokio::test]
    async fn delete_lane_clears_persisted_rows() {
        let db = std::sync::Arc::new(crate::db::VpDb::connect_mem().await.unwrap());
        db.define_schema().await.unwrap();
        let state =
            crate::repo::state::build_test_app_state_with("/tmp/vp-delete", Some(db.clone()), None)
                .await;
        let addr = LaneAddress::performer("vp-delete", "sub");
        let info = LaneInfo {
            id: Default::default(),
            address: addr.clone(),
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: "2026-07-22T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp/vp-delete/.vp/lanes/sub".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };
        state.lane_pool.write().await.insert(info.clone());
        persist_lane_ready(&state, &lane_db_key(&state), &info).await;
        assert_eq!(db.list_lanes().await.unwrap().len(), 1, "前提: 行がある");

        delete_lane_orchestrated(&state, addr, false)
            .await
            .expect("delete 成功");

        assert!(
            db.list_lanes().await.unwrap().is_empty(),
            "lane 削除で descriptor が回収される (ghost 行を残さない)"
        );
        assert!(
            db.list_lane_lifecycles().await.unwrap().is_empty(),
            "lane 削除で lifecycle も回収される"
        );
    }

    /// reservation cleanup の regression: 1 個目の create が失敗した経路で reservation が
    /// 確実に除去され、同 addr が再作成可能な状態に戻ることを確認する (placeholder leak しない)。
    ///
    /// build_test_app_state は repo_dir="" なので、cwd=None の create は clone 経路に入り
    /// new_performer_in(Path::new("")) が失敗する → clone 失敗 cleanup を通る。失敗が panic でも
    /// (JoinError cleanup 経路) reservation 除去は同じく走るので、どちらでも placeholder は残らない。
    #[tokio::test]
    async fn reservation_removed_after_failed_create() {
        let state = crate::repo::state::build_test_app_state(None).await;
        let addr = LaneAddress::performer("unknown", "fail");
        let res = create_performer_orchestrated(&state, req("fail")).await;
        assert!(res.is_err(), "repo_dir 不在での clone は失敗する: {res:?}");
        // 失敗経路で reservation が除去済み = placeholder leak なし = 再作成可能。
        let pool = state.lane_pool.read().await;
        assert!(
            pool.get(&addr).is_none(),
            "失敗した create の Spawning placeholder は pool から除去されている (leak なし)"
        );
    }
}
