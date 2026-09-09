# doc 63 — `AppState` の domain context 化（棚卸し 項目 9-2）

> **Status**: 材料の確定のみ（2026-09-09）。**設計は未確定** — §6 の判断待ち
> **Date**: 2026-09-09
> **Owners**: server crate（`crates/vantage-point/src/repo/state.rs`）
> **台帳**: `.vp/reports/refactor-audit-2026-09-07.md` 項目 9。姉妹 doc: [doc 60](60-vp-app-layout.md) / [doc 61](61-repo-runtime-layout.md) / [doc 62](62-db-module-layout.md)
> **関連**: [doc 44](44-world-one-process.md) §5.0（World と SP は既に同じ `AppState` を共有）/ [doc 45](45-transport-consolidation.md) §5.3（`canvas_senders` は書き手ゼロのまま残す）/ [doc 61](61-repo-runtime-layout.md) §3（`replay_flights` の不変条件）

## 0. この doc の位置づけ

台帳の記述はこう:

> `repo/state.rs:121` の AppState は daemon 専用と repo 専用の Option を多数抱え、DaemonState と同じ Arc を複数の入口へ渡す。値の複製と同一 Arc の共有を区別した上で、domain context を明確化する。**いきなり全 state を置換しない**。

Codex review はこれに「**field ごとに 生成者 / 正本 / 共有実体 / 書き手・読み手 / 破棄責任 の表を先に作れ**」を足した。本 doc は **その表**であって、まだ設計ではない。§6 に判断が要る点を挙げてある。

⚠️ 台帳の行番号はずれている。現 HEAD で `pub(crate) struct AppState {` は **`state.rs:119`**（121 は第 1 field の行）。

## 1. 規模（実測、nightly `50582832`）

| 項目 | 実測 |
|---|---|
| `repo/state.rs` | 611 行 |
| `AppState` の field | **29**（うち `Option` が 6） |
| `impl AppState` の method | 5（`state.rs` 3 + `terminal_ops.rs` 2） |
| 構築箇所 | **3**（repo ctor / daemon ctor / test fixture）。`new` も `Default` も**無い** |
| 後注入 method（`set_*` / `with_*`） | **0** |
| `&mut AppState` | **0 件**（crate 全体） |
| crate 外からのアクセス | **0 件**（`pub(crate)`。統合 test も別 crate なので届かない） |

**全 field は構築時に確定し、以後の変更は内部可変性だけを通る。** これは 9-2 にとって good news で、context を分けても「途中で差し替わる」経路を心配しなくてよい。

対照的に `DaemonState` には builder が **10 本**ある（`daemon/server.rs:185` 〜 `:294`）。

## 2. field 表

### 2-1. repo と daemon の両方で生きているもの（共通の芯）

| field | 型 | 生成者 | 正本 | 共有実体 | 書き手・読み手 | 破棄責任 |
|---|---|---|---|---|---|---|
| `hub` | `Hub`（値だが内部 `broadcast::Sender`） | 各 ctor が `Hub::new()` | 自分 | **ctor ごとに独立**（daemon と各 repo で別物） | prod 9 / test 5、5 file | Sender 落ちで自然閉塞 |
| `shutdown_token` | `CancellationToken`（値だが内部 Arc） | repo = `RepoRuntimes::start`（`repo_registry.rs:141`）/ daemon = `run_daemon`（`server.rs:622`） | **外**（AppState は借りている側） | repo は `RepoRuntime.shutdown` と 2 者 / daemon は全 task | 読み 3（すべて `cancel()`） | cancel は `RepoRuntimes::stop` / `shutdown_all` / health・unison・update の 3 経路 |
| `topic_router` | `Arc<TopicRouter>` | repo = 養子縁組 or 新規 / daemon = 新規 | repo ごと | 養子縁組時のみ daemon の `canvas_routers` と同一 Arc | prod 17（+ daemon 1）/ test 17、8 file | Arc drop |
| `lane_pool` | `Arc<RwLock<LanePool>>` | repo = `with_root` / daemon = `new()`（空） | 自分 | 独立 | **prod 60 / test 38、7 file（最大）** | **`PtySlot::drop` が子プロセス回収の唯一の経路**（§4） |
| `terminal_pumps` | `Arc<RwLock<TerminalPumps>>` | ctor が空 map | 自分 | 独立 | prod 5 / test 10、3 file | **`JoinHandle` に `Drop` 無し**（§4） |
| `system_event_tx` | `broadcast::Sender<SystemEvent>` | ctor（capacity 64） | 自分 | 独立 | prod 7 / test 2、3 file | 閉じ手なし（Sender 落ちで暗黙） |
| `actor_registry` | `Arc<RwLock<ActorRegistry>>` | ctor が `new()` | 自分 | 独立 | prod 2（repo `server.rs:299` / daemon `:919`） | **task の abort 経路が無い**（§4） |
| `replay_flights` | `ReplayFlights`（内部 `std::sync::Mutex`） | `Default` | 自分 | 独立 | prod 3 / test 3、1 file | 無し |
| `editor_pending` | `Arc<Mutex<HashMap<_, oneshot::Sender<_>>>>` | `Default` | 自分 | 独立 | prod 3 / test 2、1 file | 登録側 timeout remove / 解決側 remove（idempotent） |
| `file_watchers` | `Arc<Mutex<FileWatcherManager>>` | ctor | 自分 | 独立 | prod 3、2 file | **`shutdown_repo` が明示的に片付ける唯一の field** |
| `process_registry` | `Arc<Mutex<ProcessRegistry>>` | ctor | 自分 | 独立 | prod 7、2 file | Arc drop |
| `started_at` | `String` | ctor | 自分 | **値複製** | 読み 1（health） | — |

### 2-2. daemon だけが生きているもの

| field | 型 | 生成者 | 正本 | 共有実体 | 書き手・読み手 | repo 側の姿 |
|---|---|---|---|---|---|---|
| `daemon` | `Option<Arc<RwLock<RepoManagerCapability>>>` | `server.rs:693` | 外 | **6 経路に配布**（AppState / DaemonState / `MachineCapabilities` / roto / autostart / lane watcher） | 読み **1**（`health.rs:241`） | `None` |
| `update` | `Option<Arc<RwLock<UpdateCapability>>>` | 外 | 外 | 3 経路 | 読み 8、2 file | `None` |
| `machine_capabilities` | `Option<Arc<MachineCapabilities>>` | `server.rs:706` | 外 | **`daemon` / `update` と同一 Arc の二重保持**（field doc が「意図的 HACK」と自認） | 読み 2（どちらも `#[cfg(feature="midi")]` 内） | `None` |
| `wiremsg_store` | `Option<WiremsgStore>` | `server.rs:745` | 自分 | DaemonState と同一 | 4（うち 3 は clone 配布） | `None` |
| `delegation_store` | `Option<DelegationStore>` | `server.rs:753` | 自分 | DaemonState と同一 | **2、どちらも clone 配布のみ** | `None` |
| `hub_status` / `hub_nodes` / `hub_auth` | newtype（内部 `Arc<Atomic>` / `Arc<RwLock>`） | ctor | 自分 | `run_hub_federation` と同一（**writer は向こう**） | 読み各 1（health のみ） | **常に `Some` だが書き手が居ない死んだ実体** |
| `creo_actions` | `CreoActionsCache` | `server.rs:770` | 自分 | AppState / 30s poller / DaemonState の 3 者 | 読み 1（health） | 同上 |
| `wire_notifier` | `WireNotifier` | ctor | 自分 | DaemonState / hub relay と同一 | **2、どちらも clone 配布のみ** | field doc に「repo では未使用」 |
| `delivery_notify` | `Arc<Notify>` | ctor | 自分 | DeliveryActor / DaemonState / hub relay の 3 者 | **3、すべて clone 配布のみ** | 同上 |

### 2-3. repo だけが生きているもの / 値

| field | 型 | 生成者 | 共有実体 | 書き手・読み手 | daemon 側の姿 |
|---|---|---|---|---|---|
| `repo_dir` | `String` | ctor（引数 clone） | **値複製** | 読み **33、7 file** | `String::new()` |
| `repo_name` | `String` | ctor | 値複製 | 読み 8、2 file（うち 7 が `wire_relay.rs`） | **空文字列**（sentinel） |
| `terminal_token` | `String` | repo = `generate_terminal_token()` | 値複製 | 読み 3（health のみ） | **`"DAEMON_DISABLED"`**（sentinel） |
| `port` | `u16` | ctor | 値複製 | **読み 1**（`state.rs:367` の tracing log 1 引数） | 実 port |
| `canvas_senders` | `Arc<Mutex<Vec<mpsc::Sender<_>>>>` | ctor が空 Vec | 独立 | **書き手 0 / 読み 1** | 同じく空 |
| `vpdb` | `Option<SharedVpDb>` | `server.rs:648`（daemon が開いた唯一の handle） | **daemon / DaemonState / 全 repo が同一 Arc** | 読み 19、6 file | 同じく `Some` |

⚠️ **`vpdb` の `None` は mode 差ではない** — daemon / repo とも通常 `Some` で、`None` は **DB 接続失敗**の degrade（`repo/server.rs:648-662`）。他 5 つの `Option` と意味が違う。

## 3. 「値に見えて同一実体」の落とし穴

台帳の言う「値の複製と同一 Arc の共有を区別した上で」で**最も間違えやすい所**。struct の見た目が `Arc<...>` でなくても、`Clone` が同一実体を指す型がある。

- **見た目は値だが同一実体**: `hub` / `hub_status` / `hub_nodes` / `hub_auth` / `creo_actions` / `wire_notifier` / `wiremsg_store` / `delegation_store` / `shutdown_token` / `system_event_tx`
- **正真正銘の値複製**: `repo_dir` / `repo_name` / `terminal_token` / `started_at` / `port`

## 4. 破棄責任

- **`AppState` に `Drop` は無い。** crate 全体の `impl Drop` 7 件のうち、field の型から間接的に効くのは 2 系統だけ。
  - `lane_pool` → `LanePool` → slot → **`PtySlot::drop`**（`daemon/pty_slot.rs:547`）: flush task abort → replay の disk final flush → `child.kill()` + `wait()`。**AppState の drop が lane の子プロセス回収を担う唯一の経路**。
  - `lane_pool` 内の chat engine → **`ChatEngineSlot::drop`**（`conversation/engine.rs:229`）: `host.stop()` + `pump.abort()`。
- **`terminal_pumps` の `JoinHandle` に `Drop` は無い。** tokio の `JoinHandle` は drop で detach（abort しない）。明示 abort は reconcile の撤去経路（`terminal_pump.rs:311`）だけ。AppState を drop しても pump は止まらず、source（PtySlot の broadcast）が閉じることで自然終了する。
- **`actor_registry` の task は abort 経路が無い**（`actor_registry.rs:23` が TODO を明記）。停止は `shutdown_token` 依存。
- **`shutdown_repo`（`repo/server.rs:547-555`）が明示的に片付けるのは `file_watchers.stop_all()` の 1 件だけ。** 残りは Arc drop 任せ。
- ⚠️ **順序依存が暗黙**: `state.clone()` を capture した spawned task（`server.rs:401` / `:476`）が生きている間は refcount が 0 にならない。`cancel()` → task 終了 → 最後の Arc drop → `PtySlot::drop`、という順序に依存しているが、**await で待っていない**。

## 5. mode 差の表現が 3 種類に割れている

**これが 9-2 の本体**だと考えている。「daemon 専用 / repo 専用」を 3 通りの方法で表していて、どれを見ればよいかが型から分からない。

| 方法 | 使っている field | 問題 |
|---|---|---|
| ① `Option` の `Some` / `None` | `daemon` / `update` / `machine_capabilities` / `wiremsg_store` / `delegation_store`（+ 意味の違う `vpdb`） | `vpdb` だけ「接続失敗」の意味で混ざる |
| ② **常に `Some` だが片方では書き手が居ない死んだ実体** | `hub_status` / `hub_nodes` / `hub_auth` / `creo_actions` / `wire_notifier` / `delivery_notify` / `canvas_senders` | 型が「在る」と言うのに中身が永遠に初期値。読み手は分岐すら書けない |
| ③ sentinel 文字列 | `terminal_token == "DAEMON_DISABLED"` / `repo_name == ""` | 文字列比較が mode 判定になっている |

③ の実害が既に出ている。`/api/health` は `terminal_token != "DAEMON_DISABLED"` を repo mode 判定に使うが、**`/api/health` は `build_daemon_router`（`server.rs:572-574`）にしか mount されていない** = 常に daemon-mode の `AppState`。よって `health.rs:119-200` の block は **production では到達不能**で、test fixture（`terminal_token = "test"`）だけが通っている。

## 6. 判断が要る点（mako）

### 6-a. `canvas_senders` を切るか

書き手 **0 件**、読み手 1 件（`/api/health` の `canvas_clients`、常に 0）。doc 45 §5.3 は「**読み手が health にまだ 1 つあり、消すと health の応答形が変わるため据え置く**」と明記している。

切ると `/api/health` の応答から 1 field 消える。**health の応答形を変えてよいか**が判断の中身。`cut-before-fix` に従って先に問う。

### 6-b. health の到達不能 block（§5 の ③）をどうするか

`health.rs:119-200` は production で到達しない。3 択:
1. **切る**（`canvas_senders` / `process_registry` / `vpdb` の一部読み手が消える）
2. **repo mode 判定を sentinel から型に変える**（= 9-2 の本体を先にやる）
3. **据え置く**（doc に到達不能と書くだけ）

### 6-c. context の分け方

`AppState` を分けるとしたら 3 案:

| 案 | 形 | 触る範囲 |
|---|---|---|
| **A. enum で mode を型に** | `AppState { core: Core, mode: Mode }`、`Mode::Repo { .. } / Mode::Daemon { .. }` | ①②③ が 1 か所に畳まれる。読み手の分岐は `match` に。**29 field 全部の参照が動く** |
| **B. daemon 側を 1 struct に括る** | `daemon: Option<DaemonContext>` に ①② の daemon 系 9 field を入れる | `Option` の意味が 1 つに揃う。repo 側は無変更。触る範囲は daemon 読み手のみ |
| **C. 表を doc に残すだけ** | 型は変えない | 0 |

台帳が「いきなり全 state を置換しない」と言っているので **A は初手にしない**。**B が段階的で、しかも ②（死んだ実体）を `Option` に統合できる**ので初手の候補と考えている。

## 7. doc の pin

| doc | 節 | 内容 |
|---|---|---|
| [doc 44](44-world-one-process.md) | §5.0 | 「World と SP は既に同じ `AppState` 型を共有。mode 差はフィールドを `Some`/`None` で出し分けているだけ」。当時 30 field（per-project 14 / global 12 / dead 4）→ **現 HEAD は 29** |
| doc 44 | §10.6 | 「`AppState` に `lane_change_tx` が無い」← 現 HEAD でも field ではなく `start_repo` の引数のまま |
| [doc 45](45-transport-consolidation.md) | §3.1 / §5.3 | `canvas_senders` は populate されない / 書き手ゼロのまま残す（§6-a の根拠） |
| [doc 61](61-repo-runtime-layout.md) | §0 / §3 / §4 | `reconcile_lane` / `reconcile_terminal_pumps` を `impl AppState` に（PR-8 実施済）/ `replay_flights` の合流を不変条件として固定 |
| [doc 12](12-stand-architecture.md) | 表 416-427 | `AppState.actor_registry` の owner に `notify` service を挙げるが、**現 HEAD の `spawn_service` 呼び手は 2 箇所のみで `notify` は未発見**（doc の stale か別名かは未確認） |
| [doc 01](01-architecture.md) | 51 / 102 | `AppState.hub.broadcast(ProcessMessage::Show)` ← 型名が現行 `RepoMessage` と不一致（stale） |

`docs/design/53` と `54` は `AppState` の言及 **0 件**。

## 8. 未確認（推測で埋めていない）

- doc 12 の `notify` service が現 HEAD に存在するか。`spawn_service` の呼び手は `server.rs:299`（LaneSpawnActor）と `:919`（DeliveryActor）の 2 箇所しか見つからない。
- `MachineCapabilities` の field 定義本体（`daemon/machine_capabilities.rs`）は未読。`daemon` / `update` と同一 Arc であることは `server.rs:711/722` の `clone()` から確認しただけ。
- 各 field が実行時に本当に `Some` / `None` になるかは静的読解のみ（`cargo` は回していない）。
- `topic_router` の養子縁組経路と新規生成の実行時比率。

## Status log

- 2026-09-09: 材料を確定（29 field / 3 構築箇所 / 後注入 0 / `&mut` 0 / 同一 Arc の配布経路 / 破棄責任 / mode 差の 3 表現）。**設計は §6 の判断待ち**。
