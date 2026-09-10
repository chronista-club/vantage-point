# doc 63 — `AppState` を `DaemonState` / `RepoState` に分ける（棚卸し 項目 9-2）

> **Status**: 設計確定（2026-09-10）。PR-A（wire 離脱バグ）着地済み、本 doc が PR-0
> **Date**: 2026-09-09 起票（材料表）→ 2026-09-10 設計へ
> **Owners**: server crate（`crates/vantage-point/src/repo/state.rs` / `daemon/server.rs`）
> **台帳**: `.vp/reports/refactor-audit-2026-09-07.md` 項目 9。姉妹 doc: [doc 60](60-vp-app-layout.md) / [doc 61](61-repo-runtime-layout.md) / [doc 62](62-db-module-layout.md)
> **関連**: [doc 44](44-world-one-process.md) §5.0（fold-in で daemon と repo が同じ `AppState` を共有）/ [doc 45](45-transport-consolidation.md) §3.1・§5.3（`canvas_senders` は書き手ゼロ）/ [doc 61](61-repo-runtime-layout.md) §3（`replay_flights` の不変条件）

## 0. 何を直すのか

台帳の記述:

> `repo/state.rs:121` の AppState は daemon 専用と repo 専用の Option を多数抱え、DaemonState と同じ Arc を複数の入口へ渡す。値の複製と同一 Arc の共有を区別した上で、domain context を明確化する。**いきなり全 state を置換しない**。

⚠️ 台帳の行番号はずれている。現 HEAD で `pub(crate) struct AppState {` は **`state.rs:119`**（121 は第 1 field）。

### 起点 — fold-in が畳み残したもの

**doc 44 P1（fold-in）の前は、プロセス境界が 2 つの仕事をしていた。** ① repo ↔ daemon の配管（QUIC 3 channel）と ② 役の分離。doc 44 は ① を畳んだが、② を引き受ける先を作らなかった。

⚠️ **「別プロセスだから daemon の field に物理的に触れなかった」という言い方は誤り**（Codex 指摘）。旧構成でも repo プロセスの `AppState` は daemon 専用 field を `None` で持っていた。正確には、**役・有効な依存・停止責任を、共用 struct と初期値の組み合わせで表していた**。fold-in はその表現方法をそのまま同居させた。

```
vp daemon プロセス（1 個）
├─ Arc<DaemonState>（19 field）  ← Unison "daemon-control" channel
├─ Arc<AppState>  daemon 役 1 個 ← build_daemon_router の HTTP 9 route だけ
└─ RepoRuntimes: HashMap<path, RepoRuntime { state: Arc<AppState> }>
                  repo 役 N 個   ← dispatch_repo_method の 72 method
```

### 症状 — mode 差の表現が 3 通りに割れている

「どちらの役か」を 3 通りの方法で表していて、どれを見ればよいかが型から分からない。

| 方法 | 使っている field | 問題 |
|---|---|---|
| ① `Option` の `Some` / `None` | `daemon` / `update` / `machine_capabilities` / `wiremsg_store` / `delegation_store` | 意味の違う `vpdb` が同じ形で混ざる |
| ② **常に `Some` だが片方では書き手が居ない死んだ実体** | `hub_status` / `hub_nodes` / `hub_auth` / `creo_actions` / `wire_notifier` / `delivery_notify` / `canvas_senders` | 型が「在る」と言うのに中身が永遠に初期値。読み手は分岐すら書けない |
| ③ sentinel 文字列 | `terminal_token == "DAEMON_DISABLED"` / `repo_name == ""` | 文字列比較が mode 判定になっている |

⚠️ **`vpdb` の `None` は mode 差ではない** — daemon / repo とも通常 `Some` で、`None` は **DB 接続失敗**の degrade（`repo/server.rs:648`）。他 5 つの `Option` と意味が違う。`RepoState` に残す。

### ③ の実害 — 到達不能コード

`/api/health` は `terminal_token != "DAEMON_DISABLED"` を repo mode 判定に使う（`health.rs:119`）。だが **`/api/health` は `build_daemon_router`（`repo/server.rs:572`、mount は `:574`）にしか mount されておらず、その production 呼び手は `repo/server.rs:946` の 1 本だけ**（他 1 件は `#[cfg(test)]` 内の `:1577`）で、渡るのは必ず daemon 役。よって `health.rs:119-200` の block は **production で一度も通らない**。test fixture（`terminal_token = "test"`）だけが通している。

### ①②③ が実際に壊した 1 件 — PR-A（#1099、着地済み）

`repo/lane/lifecycle.rs` の `delete_lane_orchestrated` に

```rust
if let Some(store) = state.wiremsg_store.as_ref() { store.leave_all_threads(..).await }
```

があった。repo ctor（`repo/server.rs:205`）が `wiremsg_store: None` を固定で置き、`Arc<AppState>` なので後から代入する経路も setter も無い。**この guard は production で一度も真にならなかった。**

- PR #1019（`257269bd`、2026-08-29）が「nudge が 6 日間鳴り続けた」実害を直したはずの fix が、**入った瞬間から never-fire**（2026-09-10 に発見、**12 日間**）。
- 単一 lane 削除の全 6 入口（`vp lane rm` / MCP `delete_sub` / MCP `flow_handoff` の rollback / `vp flow` / GUI sidebar / FSEvents 自動削除）が影響。
- **fixture も `None` 固定だったので test も緑のまま**通っていた。store 単体 test（`leaving_agent_drops_out_of_pending`）は元から緑で、これが生き延びた理由。

**PR-A で修正済み**（`state.vpdb` から `WiremsgStore` を都度組む案 (a)）。⚠️ **9-2 より先にやる必要があった** — `wiremsg_store` は PR-3 の削除対象なので、9-2 が先に走ると**バグは「修正」ではなく「削除」で消え、test も記録も残らなかった**。

---

## 1. 目標 — 2 型に分ける

役を型の境界として引き直す。

| 型 | 中身 | 状態 |
|---|---|---|
| **`DaemonState`** | 既存 **19 field** + 新設 6 | `daemon/server.rs:32`。`Default`（`:148`）と `new()` + **builder 10 本**（`:177` 以降）を既に持つ |
| **`RepoState`** | `AppState` − daemon 分 = **14 field** | `AppState` を改名 |

### 実測が支える成立条件（nightly `a9f2205d`）

| 項目 | 値 |
|---|---|
| `AppState` の field | **29**（`state.rs:119-240`）。構築箇所 **3**、`new` / `Default` / setter なし、`&mut` **0 件** |
| **両役で共有される production コード** | **0 本** |
| daemon 役 `AppState` の渡り先 | **`build_daemon_router(state.clone())`（`repo/server.rs:946`）だけ**。他は `run_daemon` 内の field 取り出し |
| **repo 役 path から daemon 専用 field を読む箇所** | **0 件**（PR-A で最後の 1 件が外れた） |
| daemon 専用 10 field の読み手 | **全部 `repo/http/health.rs` / `repo/http/update.rs`** の中だけ |
| `DaemonState` に新設が要る field | **6**（`update` / `hub_status` / `hub_nodes` / `hub_auth` / `shutdown_token` / `actor_registry`） |
| `DaemonState.started_at: Instant` の production 読み手 | **0**（test `daemon/server.rs:3086` のみ） |
| `build_test_app_state*` の `daemon` 引数 | **全 65 site で `None`** = 既に dead |

**全 field は構築時に確定し、以後の変更は内部可変性だけを通る。** context を分けても「途中で差し替わる」経路を心配しなくてよい。

### なぜ `machine_capabilities` は新設不要か

`health.rs:206` の daemon 分岐が読むのは `machine_capabilities.devices` だけで、`DaemonState` は **既に `devices` field を持っている**。新しい field ではなく既存の handle に載せ替える。

### `LaneState` は作らない

3 分割を検討したが lane 層は成立しない。**費用ではなく構造の問題**。

1. **lane の動詞が repo field を要る。** 例えば `delete_lane_orchestrated`（`repo/lane/lifecycle.rs`）は本体で `state.repo_dir` / `state.lane_pool` / `state.reconcile_terminal_pumps` を触り、helper 経由で `state.vpdb` にも降りる（PR-A で読んだ通り）。lane 層を切っても動詞がそこに住めない。
2. **`reconcile_lane` が `topic_router` を要るのは構造**。「今その topic を誰が見ているか」が reconcile の入力（doc 53 の edge → level）。
3. **`system_event_tx` は lane scope ではない**。`LanesProjectionChanged` は ledger 操作（`vpdb` + `repo_dir`）が撃つ。
4. **key の型が揃っていない**。`lane_pool` は `LaneAddress`、`terminal_pumps` / `replay_flights` は生 `String`。しかも producer が 2 系統。**束ねる前に key を揃えるのが先**（§7）。
5. **doc 61 §2 が既に別の割り方を規定**。「所有領域は state の field で決まる」として `lane_pool` = lane / `terminal_pumps` = terminal / `replay_flights` = conversation_replay と **3 module に割り当て済み**。`LaneState` はこの割り当てを跨ぐ。
6. **doc 53 §2.5 が pump / replay を「session が持つもの」に置いている**。
7. **`LaneState` は既存 enum と名前衝突**（`lane/info.rs:27`、`crates/` 全体で 99 参照）。
8. **lock 粒度が既に設計の中心議題**。`LanePool` に `terminal_pumps` を吸収すると `reconcile_lane_pumps_inner` の 3 分割 lock が 1 本になり、pump spawn 中に全 lane が止まる。

> ⚠️ **9-2 では lane の格納構造を変えない。** ただし**操作用 context の分離は将来可能**で、そちらは lane の器ではなく「操作が守る契約」で切る（§6 段階 2）。この doc は `LaneState` という**格納の器**を否定しているだけで、lane を扱う context を永久に閉じてはいない。

---

## 2. 3 段階 — 検収を独立させる

| 段階 | 軸 | 完了条件 | 今回やる範囲 |
|---|---|---|---|
| **1** | 実行範囲（daemon 役 / repo 役） | **役の混在が消え、既存の結線と HTTP 契約が保たれた** | PR-0 〜 5（全部） |
| **2** | 操作に渡す依存 | **leaf が全体 State を知らずに使える** | 最初の 1 PR だけ |
| **3** | 所有と寿命（停止責任） | **停止を要求した仕事の終わりまで確認できる** | 別 PR、段階 2 を待たない |

⚠️ **3 つを同じ PR の完了条件にしない。** 段階 1 で field 数が 29 → 14 になっても、board の読み取りに lane pool を渡せる状態は残る。それは段階 2 の仕事。段階 1 の合格を段階 2 の未達で止めない。

---

## 3. field 移行表 — 生成元 → 共有先 → 最後の旧読み手 → 削除 PR

**「最後の旧読み手」と field は必ず同じ PR で消す。** `AppState` は `pub(crate)` なので、間に「初期化するだけの field」を残すと rustc の `dead_code` が「field is never read」を出し、CI の `-D warnings` が落ちる（§6）。

### PR-2c で削除（10）

| field | 生成元 | 共有先 | 最後の旧読み手 |
|---|---|---|---|
| `daemon` | `repo/server.rs:693` | `DaemonState.daemon_cap` / `MachineCapabilities` / roto / autostart / lane watcher | `health.rs:241` |
| `update` | `repo/server.rs:694` | `MachineCapabilities` / 24h poller | **`health.rs:260`**（PR-2a で update route が移った後に残る） |
| `machine_capabilities` | `repo/server.rs:706` | — | `health.rs:150` / `:206`（midi） |
| `hub_status` | `repo/server.rs:760` | `run_hub_federation`（**writer は向こう**） | `health.rs:276` |
| `hub_nodes` | `repo/server.rs:763` | `run_hub_federation` | `health.rs:247` |
| `hub_auth` | `repo/server.rs:766` | `run_hub_federation` | `health.rs:278` |
| `creo_actions` | `repo/server.rs:769` | `DaemonState` / 30s poller | `health.rs:266` |
| `terminal_token` | ctor（daemon 役は `"DAEMON_DISABLED"`） | — | `health.rs:112` / `:115` / `:119`（sentinel） |
| `started_at` | ctor | — | `health.rs:274` |
| **`canvas_senders`** | ctor（空 Vec、**書き手 0**） | — | **`health.rs:123`** — 到達不能な repo 分岐だが**静的には読み手** |

⚠️ **`canvas_senders` は PR-4 ではなく PR-2c。** 到達不能でも rustc から見れば読み手なので、`health.rs:119-200` を消す PR と同じ PR でしか落とせない。

⚠️ **`repo_dir` は `RepoState` に残るが、`health.rs:272` が読んでいる。** PR-2c で `HealthResponse.repo_dir` ごと消える（消費者の全数確認は §7）。

### PR-3 で削除（4、wire 系）

| field | 生成元 | 共有先 | 最後の旧読み手 |
|---|---|---|---|
| `wiremsg_store` | `repo/server.rs:746` | `DaemonState` / `DeliveryActor` / hub relay | `repo/server.rs:917` / `:997` / `:1054` |
| `wire_notifier` | ctor | `DaemonState` / hub relay | `repo/server.rs:998` / `:1055` |
| `delivery_notify` | ctor | `DeliveryActor` / `DaemonState` / hub relay | `repo/server.rs:924` / `:999` / `:1056` |
| `delegation_store` | `repo/server.rs:754` | reconcile loop / `DaemonState` | `repo/server.rs:934` / `:1000` |

⚠️ **PR-A で `lifecycle.rs` の参照が外れたので、残る読み手は上記の `run_daemon` だけ**（実測で確認済み）。**PR-4 まで残さない。**

### PR-4 で削除（1）

| field | 生成元 | 共有先 | 最後の旧読み手 |
|---|---|---|---|
| `port` | ctor（daemon 役は実 port、repo 役は 0） | — | `state.rs:367`（tracing log の引数 1 つ） |

### 残る 14 = `RepoState`

```
repo identity : repo_dir, repo_name
repo resources: hub, shutdown_token, topic_router, vpdb, file_watchers,
                process_registry, editor_pending, actor_registry
lane runtime  : lane_pool, terminal_pumps, replay_flights, system_event_tx
```

29 − 10 − 4 − 1 = **14**。

---

## 4. 不変条件 — 触らないもの

| # | 不変条件 | 根拠 |
|---|---|---|
| 1 | **lane の格納構造を変えない** | §1 の 8 点。**`state.lane_pool` の出現 58**（`.lane_pool` の行は prod 59 + test 40、`lane_pool` を含む行は 125）が 1 つも動かないこと |
| 2 | **保存済み DB key を黙って変えない** | key の系が既に 2 つある。board は `state.repo_dir` の**生文字列**を渡し（`repo/board.rs:93` / `:146` / `:160`）、`db/board.rs:402` の `load_board` は文字列一致で読む。runtime registry は `normalize_path_key`（`repo_registry.rs:130`）。**型を付けることと永続 key の正規化を分ける** |
| 3 | **`bind_dual_stack`（`repo/server.rs:950`）→ `write_pid_file`（`:955`）の相対順** | バインド前に PID を書くと、失敗時に既存 daemon の PID を上書きして制御不能になる。PR-3 で動かすのは `let app`（`:946`）1 行だけ（`axum::serve` は `:1304` なので後ろへ動かせる） |
| 4 | **破棄責任を変えない** | `lane_pool` → `PtySlot::drop`（`daemon/pty_slot.rs:547`）が lane の子プロセス回収の**唯一の経路**。副次的に「repo 役の drop で子が死ぬ」契約が型から読めるようになる |
| 5 | **`HealthResponse` の `status` / `version` / `pid`** | **crate 内の `crate::cli::HealthResponse`（`cli.rs:12-19`）がこの 3 つを `#[serde(default)]` 無しで宣言している**。呼び手は全部 `.json().ok()?` で error を握り潰すので、1 つでも消すと **生きている daemon が「不在」に見える**（`daemon/process.rs:66` の pidfile 復元 / `commands/daemon.rs:120`・`:173`・`:268`）。⚠️ VP 外の消費者ではない（§7） |
| 6 | **`RepoState` に `Arc<DaemonState>` を足さない** | 強参照循環と全機能アクセスが復活する。近道を作らない |

---

## 5. PR 列と完了条件

| PR | 内容 | これが緑なら次へ |
|---|---|---|
| **A** ✅ | wire 離脱バグの修正（#1099） | 新 test が fix 前に**赤**、fix 後に緑。`delete_lane_clears_persisted_rows` / `leaving_agent_drops_out_of_pending` が緑のまま |
| **0** | 本 doc（材料表 → 設計） | doc review |
| **0.5** | `/api/health` の daemon 形 characterization test | **`HealthResponse` の全 17 key を、存在だけでなく値と省略条件で固定する**（`status=="ok"` / `version` / `pid` / `repo_dir==""` / `started_at` / `hub=="disabled"` / `hub_nodes==[]` / `auth_targets` の key 集合 / **`processes==[]`** / `update_available==false` / `latest_version` omit / `actions==[]` / `actions_rev==0` / `terminal_token` omit / `services` の有無 / `idle_timeout_minutes`）。⚠️ **`processes` を外さない** — `health.rs:241` の `state.daemon` が producer で、PR-2c が `DaemonState.daemon_cap` に載せ替える**まさにその field**。`repo_dir` も §3 で削除対象なので baseline に要る。**PR-2c で fixture を差し替えるだけで通ること**が進行条件。assert を 1 つでも緩めたら不合格 |
| **1** ✅ | `DaemonState` に 6 field 追加 + `started_at` を `String` 化 + **組み立て口を 1 つに**（§5.1） | drift test 3 本の**本文が触られずに**緑。**共有実体テスト**が通る（§6） — `assemble_projects_the_given_instances`、mutation 3 本で赤を実測 |
| **1.5** ✅ | `build_test_app_state*` から dead な `daemon` 引数を落とす（65 site） | `git diff` の追加行 ≒ 削除行。変更が `(None)` → `()` と fixture 定義だけ |
| **2a** ✅ | `/api/update/*` 7 route → `Arc<DaemonState>`。**`AppState.update` は残す** | drift test の**本文（route 表と assert）が 1 文字も変わらず**緑。**`daemon_router_keeps_update_routes` を `assert_ne!(NOT_FOUND)` から実応答の固定へ強化**（§5.2） |
| **2b** ✅ | `/api/shutdown` → `Arc<DaemonState>` | **組み立て口に渡した token** が cancel される。handler 内で新設した token を cancel する test では不足（その mutation で赤を実測）。`DaemonState.shutdown_token` は PR-1 で `pub` で新設済み |
| **2c** | `/api/health` → `Arc<DaemonState>`。到達不能 block（`health.rs:119-200`）削除 + **10 field 同時削除** | PR-0.5 の test が fixture 差し替えのみで緑。旧 `health_handler_returns_200_with_stands_field` は**書き換えでなく削除**（書き換えると「repo 分岐を守っている test」の見た目だけが残る）。`services.devices` が daemon 分岐で出続ける |
| **3** | daemon 役 `AppState` を**構築ごと**削除（`repo/server.rs:772` の全 field — 現 HEAD で 29、**PR-2c 後は 19** + `hub` / `topic_router` local）+ **wire 系 4 field も同時削除** | 不変条件 3 を守る。`git diff` が `repo/server.rs` の外に出ない。**`--no-default-features` でも `mise run check` を 1 回** |
| **4** | `port` だけ削除 | field 数を**先に宣言してから** diff を見る（15 → 14） |
| **5** | `AppState` → `RepoState` 改名**だけ** | GitNexus `rename` を使う。改名前後を**名前だけ正規化して内容比較**。`grep -rn "AppState" crates/ \| wc -l` == **0**（**doc コメント含む**） |
| **6**（任意・別枠） | `repo/http/{health,update}.rs` + `build_daemon_router` を `daemon/` へ移設 | 初手の必須条件にしない。9-2 の仕上げとして推奨 |

全 PR 共通: `cargo fmt --all -- --check` / `mise run check` / `cargo clippy --workspace --all-targets -- -D warnings` / `mise run test` / **`mise run claims`** / Moody Blues / `gh pr create --base nightly` / nightly CI。

### 5.1 PR-1 — 組み立て口を 1 つに

**必須引数を増やすだけでは、同じ型の別実体を渡す余地が残る。** 関連 handle は**既存の所有元から一括して取り出す**。

- `MachineCapabilities` が既に `repo_manager` / `update` / `devices` を束ねている（`machine_capabilities.rs:40-51`）。**別々の引数として再選択させない**
- repo の各 view は同じ `RepoManagerCapability` の `running_processes_ref` / `repos_ref` / `lane_registry_ref` / `process_presence_ref`（`repo_manager_capability.rs:303-324`）から取り出す。**空 map を constructor 内で補わない**
- `lane_change_tx` / `canvas_routers` / `control_channels` は現在の共有関係を維持。**`FromRef` は既存 handle の clone だけを行い、cache や channel を新設しない**
- **`actor_registry` は handler の依存ではない。** PR-1 では保持先を移すだけで、最終的には段階 3 の task 所有側へ

```
PR-1 前: router 構築 server.rs:946 → DaemonState builder（new() + with_* 10 本）:968 → Arc 化 :1020
PR-1 後: router 構築 → `DaemonState::assemble(DaemonAssembly { 16 field 全部必須 })` → Arc 化
       副作用なし（socket / device / poller を起動しない）なので test が実物の結線を通せる
```

必須 field を持つ名前付き引数用 struct（`DaemonAssembly`）で入れた。**`Default` で穴を埋める仕組みには戻さない** — `Option` は「DB 接続失敗で無い」3 つ（`vpdb` / `wiremsg_store` / `delegation_store`）だけ。`Default` / `new()` 自体は test と `daemon/process.rs::run_daemon`（workspace 内に呼び手なし）のために残した。bind・PID 書き込み・外部サービス起動の順序は動かしていない（不変条件 3）。

router 構築（`let app = build_daemon_router(state.clone(), daemon_state.clone())`）は **PR-2a で assemble の後ろへ動かした**（`build_daemon_router` が `Arc<DaemonState>` も取るため。`axum::serve` まで使わないので後ろへ動かせる。`bind_dual_stack` → `write_pid_file` は動かしていない、不変条件 3）。

### 5.2 drift net の現状 — 3 本ある

`repo/server.rs` の inline test に route 登録を固定する網が **3 本**ある（`route_status` fixture は `:1569`）。

| test | 行 | 見ているもの | 強度 |
|---|---|---|---|
| `daemon_router_keeps_health_and_shutdown` | `:1622`（PR-2a 後） | health / shutdown が **`OK`** | 強 |
| `daemon_router_drops_removed_control_routes` | `:1641`（PR-2a 後） | 撤去済みの 18 組（`(path, method)`、distinct path は 15）が **`NOT_FOUND`** | 強 |
| `daemon_router_keeps_update_routes` | `:1682`（PR-2a 後） | ~~update 7 route が `NOT_FOUND` でない~~ → **PR-2a で強化**: `update: None` の `DaemonState` で 7 route 全部 503 + `DaemonState.update` だけ `Some` で param 検証 4 route が 400（`AppState.update` を読み続けていれば 503） | **強** |

⚠️ **「update 7 route に drift net が無い」は誤り**（v2 までの記述を訂正）。網は在ったが `assert_ne!` で**登録の有無しか見ていなかった**。PR-2a で強化した。check / apply は `Some` だと GitHub API に出る、restart は `Some` だと `restart_self` が本当に走るので、この 3 本は 503 層でしか叩かない。CORS は `daemon_router_applies_cors_to_both_state_groups` を新設（2 群から 1 route ずつ preflight）。

⚠️ **`route_status` fixture 自体は PR-2a で変わった。** `build_daemon_router` が `Arc<DaemonState>` も取るため（test 用は `daemon/server.rs` の `build_test_daemon_state()`、production と同じ `assemble` を通す）。完了条件の「触られずに緑」が指すのは**各 test の本文（route 表と assert）**であって fixture ではない。

---

## 6. 検証 — `-D warnings` は主軸にならない

CI は `cargo clippy --workspace --all-targets -- -D warnings`。**`AppState` は `pub(crate)` なので rustc の `dead_code` が「field is never read」を出す**（`DaemonState` は `pub` なので出ない。ここが非対称）。だから **「HTTP を移す」と「field を消す」は同じ PR でやるしかない** — 移した瞬間に 10 field が一斉に write-only になる。逃げ道は **`axum::Router::merge`**（`Router<S>::with_state(s)` が `Router<()>` を返すので、state の違う 2 本を合流できる）。

**が、これは孤児 field しか検出できない。同じ型の別実体は検出できない。** 新しい `HubFederationStatus::new()` を作って渡しても compile は通り、health は初期値を永遠に返す — ①②③ が生んだのと同じ形のバグを、型を分けた後にもう一度作れてしまう。

| 契約 | 壊し方を検出する検証 |
|---|---|
| **HTTP 登録と middleware** | production の router builder を `oneshot` で通す。9 route の method・想定応答・**CORS**。update の download / restart を実行しない fixture。CORS の test は PR-2a 以前 0 本だった → `daemon_router_applies_cors_to_both_state_groups` を新設 |
| **共有実体**（最重要） | 組み立てに渡した cache を**非初期値へ変更**し、HTTP が同じ値を返す。ACTIONS は items と rev、hub は status / nodes / auth、update は available と version、presence は代表例。`Arc` 同一性の比較は補助 |
| **停止 signal** | 組み立て口に渡した token が HTTP shutdown で cancel。update restart も同じ token |
| **health の契約** | 正常 daemon / DB なし / midi 有無。**`started_at` は構築時に 1 度確定し、複数回の health で不変**。`repo_dir` 等の削除は「承認済み差分」として baseline と分ける |
| **repo 間の分離** | 同じ DB を使う 2 repo で A の board 更新と B の取得が混ざらない。A 停止後も B が利用可能。topic router 養子縁組と lane 通知の同一実体も維持 |
| **静的検査** | CI 相当 clippy + **server crate の `--no-default-features`**（`crates/vantage-point/Cargo.toml:14` が「Daemon-only build (Linux の ALSA 無し環境で通す)」と明記。PR-3 で `machine_capabilities` local が midi 無効側で完全に未使用になる） |

**`Router::merge` の作法**: 各群を `Router<()>` に揃えてから merge し、**共通 CORS は合流後に掛ける**。

### 実機確認（mako、kitty から）

- **PR-A**: `vp lane rm` の後。⚠️ **`vp wire inbox` は合格条件にしない** — `/api/wire/unread-count` を叩くだけで、`wiremsg_store.rs:736` が「cursor (agent_cursor) とは独立 — recv 済でも ack されるまで載り続ける」と明記。**未読 0 は未 ack 0 を意味しない**。回帰テストが主証拠
- **PR-2c / PR-3 の後**: `VP_SWAP_RESTART_DAEMON=1 mise run app:swap`。vp-app sidebar の health 由来（Hub 行 / 更新ボタン / ACTIONS / repo presence）。`curl -s localhost:32000/api/health | jq -S 'keys'` の前後比較

---

## 7. 落とし穴

- **`-D warnings` の field 孤児化**（§6）。PR 列の最大の制約。
- **`started_at` は「型変換」ではなく「dead な `Instant` を `String` に置換」**。`Instant` を残して `Utc::now() - elapsed()` で再計算する形は**避ける** — sleep をまたぐと wall clock とずれ、health が 5s 周期で叩かれるので vp-app 側から起動時刻が動いて見える。
- **`services.devices` は prose で設計判断の根拠**（`tests/vp_daemon_kdl.rs:176`）。`devices/midi` を agent に露出しない判断の唯一の根拠なので、`with_devices` の cfg gate が health の `machine_capabilities.devices.is_some()` と同値であることを明示的に確認する。
- ⚠️ **`HealthResponse` の field 削除は「消費者を全数確認した」が成り立つまで安全と言えない。** v1 の全数確認は **crate 内の最も脆い消費者を落としていた**。
  - **耐える側**: vp-app の `DaemonHealthInfo` は 13 field 全てに `#[serde(default)]`（`vp-app/src/daemon_wire.rs:137-183`、`repo_dir` はそもそも未宣言）。`.mise/tasks/app/swap` は 2xx だけを見る（`:107`）。Swift agent は `json["pid"] as? Int ?? 0`（`apple/VantagePointAgent/Sources/InstanceScanner.swift:56`）で、**読むが使わない** — `VpInstance` の identity は `port` で、`pid` は `apple/` 全体で他に参照が無い。
  - **耐えない側**: **`crate::cli::HealthResponse`（`crates/vantage-point/src/cli.rs:12-19`）** は `status` / `version` / `pid` を `#[serde(default)]` 無しで宣言する。呼び手は `daemon/process.rs:66`（port fallback の pidfile 復元）と `commands/daemon.rs:120` / `:173` / `:268` で、**全部 `.json().ok()?`**。1 field 消すと deserialize が silent に失敗し、**生きている daemon が「不在」扱い**になって `vp daemon status` / `restart` / `stop` が壊れる。compile も test も緑のまま。不変条件 5 の 3 field がこの parser と一致しているのは偶然ではない。
  - producer 側（`HealthResponse`、`repo/http/health.rs:39-101`）は `#[derive(serde::Serialize)]` のみで `#[serde(default)]` は 0 件（deserialize 用の attribute なので当然）。省略を制御しているのは `skip_serializing_if` の付いた **6 field**（`terminal_token` `:45` / `services` `:50` / `hub_auth` `:63` / `auth_targets` `:74` / `processes` `:78` / `latest_version` `:85`）。

- **`AppState` は `pub(crate)` だが `DaemonState` は `pub`**。9 route を移すと daemon handler の state が crate 外から構築可能になる。「crate 外アクセス 0 件」は `RepoState` 側でしか成立しない。
- **段階 2 の context に `Arc<RepoState>` を隠したり `Deref` で全 field を公開したりしない。** 同期呼び出しには借用 context、spawn 先には必要な handle の clone。

---

## 8. 9-2 に混ぜない follow-up（台帳へ）

- ⚠️ **`thread_participant` の `left` 行が恒久で、同名 lane の再作成が復帰できない**（**PR-A が dormant → live に変えた**）。`leave_all_threads`（`wiremsg_store.rs:568`）は `status: 'left'` の行を **CREATE するだけで削除経路が無い**（crate 全体で 0 件）。しかも `thread_participant_uniq` が (thread, agent) の UNIQUE index（production は `db/schema.rs:421`。`wiremsg_store.rs:1177` は `#[cfg(test)]` fixture 側）。`vp lane rm sub` → `vp lane new sub` で agent address 文字列は同じなので、**再作成した lane はその thread に戻れない**。PR-A 前は離脱自体が起きなかったので潜在だった。
- **`WiremsgStore::new` は leave には過剰**。`math::max(local_seq)` を読んで**独立した採番器**を作る（`wiremsg_store.rs:246`）。leave は採番しないので成立するが、**この局所 store を send に流用しない**（第 2 writer ができる）。共用の離脱操作を wire module に置く案は最小修正の後の別 PR。
- **`service_status` table**。⚠️ **「PR-2c で dead 化する」は不正確** — `upsert_service_status` の唯一の非 test 呼び手は `health.rs:191` で、**到達不能 block の中**。つまり **production では既に dead**で、PR-2c が消すのは最後の**静的な**読み手。`list_service_status` は非 test 呼び手が元から 0。doc 62 §6 の `cut-before-fix` 対象。
- **`terminal_pumps` / `replay_flights` の key を `LaneAddress` に揃える**。今は生 `String` で producer が 2 系統。`parse_address` は旧形（旧 2 分節形 `<repo>/<name>`、旧予約名 `conductor` / `lead`）を受理して canonical に正規化するので、canonical 以外の文字列が key に入ると demand 経路と動詞経路が別 entry を触る。**実害の有無は未確認**。terminal / replay の再編より先にやる。
- **lane runtime の bundle を named struct に**。4 つ組 `{lane_pool, terminal_pumps, topic_router, system_event_tx}` を手で受け取るのは `LaneSpawnActor::new`（`spawn_actor.rs:121`）**だけ**で、`reconcile_lane`（`reconcile.rs:114-119`）は `system_event_tx` を取らない 3 つ + `addr`。**束ねる根拠は「2 箇所が同じ形」ではなく「呼び出しごとに手で選び直している」**方に置く。**名前は `LaneState` にできない**（§1 の 7）。
- `ensure_and_submit_chat` の lock 範囲（台帳項目 8、未測定）。
- **doc 12 の `notify` service は実在しない**（`spawn_service` の呼び手は lane-spawn と delivery の 2 本）。新しい context に移さない。doc 01 の `ProcessMessage::Show` も型名が現行 `RepoMessage` と不一致（stale）。

### 段階 3 で拾う既知の穴 2 件

- **① 長期 runner が repo stop で止まらない**（静的確認のみ、実機再現は未実施）。`process_runner.rs` に `CancellationToken` の参照が **0 件**。`:325` の task が registry の Arc を保持し、`:430` は専用 shutdown receiver を待つ。
- **② Editor の呼び出し中断で pending が残る**（Codex の probe で再現、`.vp/reports/probes/editor-cancel/`）。`pending_after_future_drop_and_3_1s=1` / `pending_after_late_response=0` / `pending_after_normal_timeout=0`。**本番の通信経路でこの中断が起きるかは未検証**なので、GUI の実機バグとしては扱わない。

停止契約に加える 3 点: ① **受付を閉じる場所を spawn の境界まで届かせる**（`RepoRuntimes::dispatch`（`repo_registry.rs:265`）は State の Arc を取得してから実行するので、map から除去しても in-flight は残る。`is_cancelled()` の一度読みでは検査直後の窓が残る）② **lock 内で停止対象を取り出し、lock 外で通知・終了待ち**（runner の終了処理は `process_runner.rs:438` / `:445` で registry lock を取る。`stop_all` が同じ lock を保持したまま join すると行き詰まる）③ **親 task と、その中から作る task の両方を回収**（`:325` の外側に加え stdout / stderr の `:404` / `:416`）。

⚠️ **Tokio の `JoinHandle` は drop すると detach する**ので、保持場所を変えるだけでは停止保証は増えない。`RepoTasks` は既存 `ActorRegistry` の役割を整理して接続し、**並行する新しい台帳を増やさない**。

---

## 9. 材料 — field ごとの所有表（実測、nightly `50582832`、2026-09-09）

設計の根拠。PR ごとの答え合わせに使う。

### 9-1. repo と daemon の両方で生きているもの（= `RepoState` に残る芯）

| field | 型 | 生成者 | 正本 | 共有実体 | 書き手・読み手 | 破棄責任 |
|---|---|---|---|---|---|---|
| `hub` | `Hub`（値だが内部 `broadcast::Sender`） | 各 ctor が `Hub::new()` | 自分 | **ctor ごとに独立** | prod 9 / test 5、5 file | Sender 落ちで自然閉塞 |
| `shutdown_token` | `CancellationToken`（値だが内部 Arc） | repo = `RepoRuntimes::start`（`repo_registry.rs:141`）/ daemon = `run_daemon` | **外**（`AppState` は借りている側） | repo は `RepoRuntime.shutdown` と 2 者 / daemon は全 task | 読み 3（すべて `cancel()`） | `RepoRuntimes::stop` / `shutdown_all` / health・unison・update |
| `topic_router` | `Arc<TopicRouter>` | repo = 養子縁組 or 新規 / daemon = 新規 | repo ごと | 養子縁組時のみ `DaemonState.canvas_routers` と同一 Arc | prod 17（+ daemon 1）/ test 17、8 file | Arc drop |
| `lane_pool` | `Arc<RwLock<LanePool>>` | repo = `with_root` / daemon = `new()`（空） | 自分 | 独立 | **`.lane_pool` の行 = prod 59 / test 40、7 file（最大）** | **`PtySlot::drop` が子プロセス回収の唯一の経路** |
| `terminal_pumps` | `Arc<RwLock<TerminalPumps>>` | ctor が空 map | 自分 | 独立 | prod 5 / test 10、3 file | **`JoinHandle` に `Drop` 無し** |
| `system_event_tx` | `broadcast::Sender<SystemEvent>` | ctor（capacity 64） | 自分 | 独立 | prod 7 / test 2、3 file | 閉じ手なし |
| `actor_registry` | `Arc<RwLock<ActorRegistry>>` | ctor が `new()` | 自分 | 独立 | prod 2 | **task の abort / await 経路が無い**（`spawn_service`（`actor_registry.rs:146`）は `task: Some(..)` を保持するが、`abort()` の呼び手は crate 全体で 0 件） |
| `replay_flights` | `ReplayFlights`（内部 `std::sync::Mutex`） | `Default` | 自分 | 独立 | prod 3 / test 3、1 file | 無し |
| `editor_pending` | `Arc<Mutex<HashMap<_, oneshot::Sender<_>>>>` | `Default` | 自分 | 独立 | prod 3 / test 2、1 file | 登録側 timeout remove / 解決側 remove（idempotent） |
| `file_watchers` | `Arc<Mutex<FileWatcherManager>>` | ctor | 自分 | 独立 | prod 3、2 file | **`shutdown_repo` が明示的に片付ける唯一の field** |
| `process_registry` | `Arc<Mutex<ProcessRegistry>>` | ctor | 自分 | 独立 | prod 7、2 file | Arc drop |
| `repo_dir` | `String` | ctor（引数 clone） | 自分 | **値複製** | 読み **33、7 file** | — |
| `repo_name` | `String` | ctor | 自分 | 値複製 | 読み 8、2 file（うち 7 が `wire_relay.rs`） | — |
| `vpdb` | `Option<SharedVpDb>` | `repo/server.rs:648`（daemon が開いた唯一の handle） | 外 | **daemon / `DaemonState` / 全 repo が同一 Arc** | 読み 19、6 file | — |

### 9-2. daemon だけが生きているもの（= 移す 15）

| field | 型 | 共有実体 | repo 側の姿 |
|---|---|---|---|
| `daemon` | `Option<Arc<RwLock<RepoManagerCapability>>>` | **6 経路に配布** | `None` |
| `update` | `Option<Arc<RwLock<UpdateCapability>>>` | 3 経路 | `None` |
| `machine_capabilities` | `Option<Arc<MachineCapabilities>>` | **`daemon` / `update` と同一 Arc の二重保持**（field doc が「意図的 HACK」と自認） | `None` |
| `wiremsg_store` | `Option<WiremsgStore>` | `DaemonState` と同一 | `None` |
| `delegation_store` | `Option<DelegationStore>` | `DaemonState` と同一 | `None` |
| `hub_status` / `hub_nodes` / `hub_auth` | newtype（内部 `Arc<Atomic>` / `Arc<RwLock>`） | `run_hub_federation` と同一（**writer は向こう**） | **常に `Some` だが書き手が居ない死んだ実体** |
| `creo_actions` | `CreoActionsCache` | `AppState` / 30s poller / `DaemonState` の 3 者 | 同上 |
| `wire_notifier` | `WireNotifier` | `DaemonState` / hub relay と同一 | 構築側のコメントが `delivery_notify` と併せて daemon 専用と説明（`repo/server.rs:235-236`） |
| `delivery_notify` | `Arc<Notify>` | `DeliveryActor` / `DaemonState` / hub relay の 3 者 | field doc に「repo では未使用」（`repo/state.rs:197`） |
| `canvas_senders` | `Arc<Mutex<Vec<mpsc::Sender<_>>>>` | 独立 | **書き手 0 / 読み手 1** |
| `terminal_token` | `String` | 値複製 | repo = `generate_terminal_token()` |
| `started_at` | `String` | 値複製 | 同じく `Some` |
| `port` | `u16` | 値複製 | repo 役では常に 0 |

⚠️ **`canvas_senders` を切っても production の応答は変わらない**（v1 §6-a の前提誤りを訂正）。`canvas_clients` は到達不能 block の中でしか作られないので、**最初から応答に出ていない**。doc 45 §5.3 の「消すと health の応答形が変わる」は fold-in 前の記述で、現 HEAD では成立しない。

### 9-3. 「値に見えて同一実体」

struct の見た目が `Arc<...>` でなくても、`Clone` が同一実体を指す型がある。台帳の言う「値の複製と同一 Arc の共有を区別した上で」で**最も間違えやすい所**。

- **見た目は値だが同一実体**: `hub` / `hub_status` / `hub_nodes` / `hub_auth` / `creo_actions` / `wire_notifier` / `wiremsg_store` / `delegation_store` / `shutdown_token` / `system_event_tx`
- **正真正銘の値複製**: `repo_dir` / `repo_name` / `terminal_token` / `started_at` / `port`

### 9-4. 破棄責任の現状

- **`AppState` に `Drop` は無い。** `vantage-point` crate の `impl Drop` は **5 件**（`StateDirGuard` / `PtySlot` / `TermAttach` / `ComGuard` / `ChatEngineSlot`。`crates/` 全体では vp-app の 2 件を足して 7）で、**field の型から間接的に効くのは 2 系統だけ**（残り 3 件は test guard / CLI local / attach 単位）。
  - `lane_pool` → `LanePool` → slot → **`PtySlot::drop`**（`daemon/pty_slot.rs:547`）: flush task abort → replay の disk final flush → `child.kill()` + `wait()`
  - `lane_pool` 内の chat engine → **`ChatEngineSlot::drop`**（`conversation/engine.rs:229`）: `host.stop()` + `pump.abort()`
- **`terminal_pumps` の `JoinHandle` に `Drop` は無い。** tokio の `JoinHandle` は drop で detach。明示 abort は reconcile の撤去経路（`terminal_pump.rs:311`）だけ。`AppState` を drop しても pump は止まらず、source（`PtySlot` の broadcast）が閉じることで自然終了する。
- **`shutdown_repo`（`repo/server.rs:547-555`）が明示的に片付けるのは `file_watchers.stop_all()` の 1 件だけ。** 残りは Arc drop 任せ。
- ⚠️ **順序依存が暗黙**: `state.clone()` を capture した spawned task（`repo/server.rs:401` / `:476`）が生きている間は refcount が 0 にならない。`cancel()` → task 終了 → 最後の Arc drop → `PtySlot::drop`、という順序に依存しているが、**await で待っていない**（段階 3）。

---

## 10. doc の pin

| doc | 節 | 内容 |
|---|---|---|
| [doc 44](44-world-one-process.md) | §5.0 | 「World と SP は既に同じ `AppState` 型を共有。mode 差はフィールドを `Some`/`None` で出し分けているだけ」。⚠️ 「当時 30 field」は誤読 — doc 44 `:148` は「per-project 14 / global 12 / dead 4（**dead は P1 露払いで削除済**）」なので当時の live は **26**。現 HEAD の 29 と直接は比べられない |
| doc 44 | §10.6 | 「`AppState` に `lane_change_tx` が無い」← 現 HEAD でも field ではなく `start_repo` の引数のまま |
| [doc 45](45-transport-consolidation.md) | §3.1 / §5.3 | `canvas_senders` は populate されない / 書き手ゼロのまま残す。⚠️ §5.3 の「消すと応答形が変わる」は**現 HEAD では成立しない**（§9-2） |
| [doc 61](61-repo-runtime-layout.md) | §0 / §3 / §4 | `reconcile_lane` / `reconcile_terminal_pumps` を `impl AppState` に（PR-8 実施済）/ `replay_flights` の合流を不変条件として固定 |
| [doc 62](62-db-module-layout.md) | §6 | `service_status` / `prompts` / `notifications` の `cut-before-fix` 候補 |
| [doc 12](12-stand-architecture.md) | 表 422- | `AppState.actor_registry` の owner に `notify` service を挙げるが**実在しない**（stale） |
| [doc 01](01-architecture.md) | 102 | `AppState.hub.broadcast(ProcessMessage::Show)` ← 型名が現行 `RepoMessage` と不一致（stale） |

`docs/design/53` と `54` は `AppState` の言及 **0 件**。

## Status log

- 2026-09-09: 材料を確定（29 field / 3 構築箇所 / 後注入 0 / `&mut` 0 / 同一 Arc の配布経路 / 破棄責任 / mode 差の 3 表現）。設計は判断待ち。
- 2026-09-10: **設計へ書き換え（PR-0）。** 2 型（`DaemonState` / `RepoState`）+ 3 段階 + field 移行表 + PR 列を確定。Codex 2 巡のレビューを反映。

  **散文の誤りを 2 段の網で 25 件つぶした。** `mise run claims`（容疑者を並べる）で 7 件、Moody Blues（実物に当てる）で 18 件。
  ⚠️ **claims が拾って自分で「訂正した」と判断した項目の中に、さらに 3 件の誤りが入っていた**（`/api/health` の消費者、`skip_serializing_if` の数、`pid` を守る理由）。
  **容疑者を並べる道具は、当てる工程を省略できない。**

  claims が拾った 7 件 —
  ① `DaemonState` は **19 field**（`pub(crate)` の `control_channels` / `delegation_store` を数え落としていた）
  ② `canvas_senders` を切っても production の応答は**変わらない**（v1 §6-a の前提誤り）
  ③ **update 7 route の drift net は既に在る**（`repo/server.rs:1641`）。PR-2a は新設ではなく**強化**
  ④ **`service_status` は production では既に dead**（唯一の呼び手が到達不能 block 内）
  ⑤ `vantage-point` crate の `impl Drop` は **5 件**（v1 の「7 件」は誤り）
  ⑥ `#[serde(default)]` は producer ではなく**消費側**の attribute
  ⑦ `lane_pool` の件数が自 doc 内で矛盾していた（本文 57 / §9「prod 60 / test 38」）

  Moody Blues が拾った主なもの —
  ⑧ **`crate::cli::HealthResponse`（`cli.rs:12-19`）が全数確認から漏れていた。** `status` / `version` / `pid` を `#[serde(default)]` 無しで宣言し、呼び手は `.json().ok()?`。1 field 消すと**生きている daemon が「不在」に見える**。不変条件 5 の本当の根拠はこれ
  ⑨ **Swift agent は `pid` を読むが使っていない**（`VpInstance` の identity は `port`）。「VP 外の消費者が使っている」は誤り
  ⑩ `skip_serializing_if` は **6 field**（`processes` と `latest_version` を落としていた）。§5 の PR-0.5 が `latest_version` の omit を検収条件に挙げていたので**自 doc 内で矛盾**していた
  ⑪ `vp-cli/src/main.rs:1041` は `/api/health` ではなく `lane_delete` の応答（`pid` という key が一致しただけの**近い方への帰属**）
  ⑫ `reconcile_lane` は 4 つ組を取らない（3 つ + `addr`）。bundle 化の根拠を書き換え
  ⑬ `DaemonState.shutdown_token` は**存在しない**（PR-1 で新設する 6 のうちの 1 つ）
  ⑭ PR-3 の「29 field 全部」は PR-2c 後には **19**
  ⑮ 行ずれ 5 件（`build_daemon_router` の定義行 / `WiremsgStore::new` / doc 01 / doc 12 / doc 44 の「30 field」）
  ⑯ PR-0.5 の検収列挙に **`processes` が無い** — PR-2c が載せ替える当の field
  ⑰ 再現不能だった「31%（16/52）」を落とし、`actor_registry` / `wire_notifier` の出典を実物へ
- 2026-09-10: **PR-0.5（#1102）。** daemon 形 `/api/health` の characterization 6 本 + fixture `build_test_daemon_app_state()`。層 1（key 集合と既定値）+ 層 2（渡した実体の投影 / `started_at` の不変）。mutation 3 本で赤を実測。production 変更 0 行。
- 2026-09-10: **PR-1。** `DaemonState` は **19 → 25 field**（`update` / `hub_status` / `hub_nodes` / `hub_auth` / `shutdown_token` / `actor_registry`）、`started_at` は `Instant` → `String`（production の読み手 0、test 1 本だけが `elapsed()` を見ていた）。
  **組み立て口は `DaemonState::assemble(DaemonAssembly)` の 1 つ**（§5.1）。builder 10 本（`with_running_processes` / `with_daemon_cap` / `with_creo_actions` / `with_control_channels` / `with_canvas_routers` / `with_lane_change_tx` / `with_vpdb` / `with_devices_event_bus` / `with_devices` / `with_wire`）を撤去。repo の 4 view は `machine_capabilities.repo_manager` の `*_ref()` から、`update` / `devices` も同じ container から取り出す。
  `run_daemon` は両方（daemon 役 `AppState` と `DaemonState`）に渡す部品（`actor_registry` / `started_at` / `wire_notifier` / `delivery_notify`）を **1 度だけ作る**よう hoist した — `AppState` の中で inline に `new()` すると `DaemonState` に同じ実体を渡す手段が無い。
  **共有実体テスト**: `assemble_projects_the_given_instances`（渡した側を非初期値へ動かして state 側から見える + `Arc::ptr_eq` 補助）+ `assemble_shares_devices_from_machine_capabilities`（midi）。mutation 3 本（`hub_status` を `new()` / `shutdown_token` を `new()` / `process_presence` を空 map）で**この test だけが赤**、health 6 本と drift 3 本は緑のまま（HTTP はまだ `AppState` を読む）。
  drift test 3 本（§5.2）は本文どころか fixture も触っていない。health 6 本も同じ。
- 2026-09-10: **PR-1.5。** `build_test_app_state(daemon)` → `build_test_app_state()`、`build_test_app_state_with(repo_dir, vpdb, daemon)` → 2 引数。65 site（`(None)` 59 + `_with(…, None)` 6）が全部 `None` だったことを base `adfd7a8a` で再確認。production 0 行、test 1350 不変。`TestStateParams.daemon` は daemon 役 fixture が使うので残す。
- 2026-09-10: **PR-2a。** `/api/update/*` 7 handler の state を `Arc<DaemonState>`（読むのは `update` と `shutdown_token`、どちらも同名 field）。`build_daemon_router(state, daemon_state)` は群ごとに `with_state` してから `merge`、CORS は合流後に 1 回。`let app` を assemble の後ろへ。`AppState.update` は health が読むので残る。test: `daemon_router_keeps_update_routes` を 2 層（503 全 7 / 400 param 検証 4）に強化、CORS test 新設、fixture `build_test_daemon_state()`（`assemble` 経由）。drift 2 本（health+shutdown / 撤去 18 組）は本文無変更。
- 2026-09-10: **PR-2b。** `shutdown_handler` の state を `Arc<DaemonState>`、route を `daemon_state_routes` 群へ（`AppState` 群に残るのは `/api/health` だけ）。test `shutdown_handler_cancels_shutdown_token` は `build_test_daemon_state()` で、`assemble` に渡した token が cancel されることを見る（handler が新設した token を cancel する mutation で赤）。`AppState.shutdown_token` は `repo/unison_server.rs:219` が読むので残る。
