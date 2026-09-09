# doc 61 — repo runtime の受付と所有領域（`repo/unison_server.rs` の分離）

> **Status**: 設計確定（2026-09-08、mako）。実装は PR-0（本 doc）→ PR-1〜8 → PR-B7
> **Date**: 2026-09-08
> **Owners**: server crate（`crates/vantage-point/src/repo/`）
> **台帳**: `.vp/reports/refactor-audit-2026-09-07.md` 項目 7。姉妹 doc: [doc 60](60-vp-app-layout.md)（vp-app 側、同じ判定基準）
> **関連**: [doc 45](45-transport-consolidation.md) §5.2（単一 stream 逐次）/ [doc 44](44-world-one-process.md) §5.2（DB handle）/ [doc 40](40-conversation-ssot.md) §4（conversation の漏斗）/ [doc 52](52-board-redesign.md) §5（board item の identity）/ [doc 53](53-lane-reconcile.md) §2.3（demand は level 読み）

## 0. 何を決めたか

- `crates/vantage-point/src/repo/unison_server.rs`（5,691 行 = 本体 2,718 + test 2,973）を、**受付 1 枚 + 所有領域**に分ける。
  この file には QUIC の accept loop は無い（それは `repo/server.rs`）。あるのは `dispatch_repo_method`（72 method の match 1 枚）と、その arm が呼ぶ handler 群。
  handler が board 永続化 / editor bridge / file・process・Ruby / terminal demand / conversation replay・submit・session / lane ops / wire relay を全部抱えているのが問題で、
  「board の変更理由」と「replay の順序の変更理由」が同じ file に落ちる。
- 判定基準は doc 60 と同じ（mako 2026-09-08）: **変更時に触る場所がまとまる** / **state と resource の所有者が明確** / **file を開いた瞬間に何の module か分かる**。行数は観測であって目標にしない。test を追い出して行数を減らすことはしない（台帳 §6）。
- **移設と挙動変更を分ける**。PR-1〜7 は順序付き diff で本文一致の移設だけ。DB の順序・event の順序・error 表現の統一は §6 の follow-up として別建て。
- **決定 3 点**（mako 2026-09-08、AskUserQuestion）:
  1. `repo/routes/` の改名（HTTP は health / shutdown / update だけで、lanes / wire / delegation / agents は Unison から呼ばれる domain fn）は **7b として分割の後に**。移設 PR に path 書き換えのノイズを混ぜない。`rename-all-at-once`、`pub use` shim は作らない。
  2. `reconcile_lane` / `reconcile_terminal_pumps`（全群が呼ぶ収束点）は **`impl AppState` の method に**（PR-8）。
  3. b-7（window 間の repo 並び順の伝播、doc 60 §8）は **`daemon-repo` channel に `ProcessLifecycleEvent::ReposChanged`** を流す（§5）。

## 1. 目標 tree（`crates/vantage-point/src/repo/`）

命名 rule: **裸の名前 = owner**（logic がそこに住む）/ **`*_ops.rs` = Unison method の handler で、owner は別**（payload を剥がして owner を呼び、JSON を返すだけ）。
file を開いた瞬間に「ここが本体か、受付の続きか」が名前で分かるようにする。

```
crates/vantage-point/src/repo/
├── unison_server.rs        受付（薄い）: dispatch_repo_method（match 1 枚、72 arm）/ payload_session_key /
│                             handle_process_message（pane ops の generic relay）/ QUIC_PORT_OFFSET
│  ── owner（logic がここに住む）──
├── editor_bridge.rs        GUI editor との往復（handle_editor_command / handle_editor_result、EDITOR_BRIDGE_TIMEOUT）。
│                             逆方向の editor_result も同じ editor_pending を消費するので同居
├── board.rs                board の永続化と配信（board_key / content_to_parts / extract_stack / broadcast_board /
│                             handle_canvas_command / handle_board_* / seed_boards）
├── conversation_replay.rs  attach 時の replay 合流（handle_conversation_demand_start / replay_once / splice_session_init /
│                             replay_with_in_flight / handle_conversation_demand_stop / route_conversation）。owner 不在だった logic
├── wire_relay.rs           repo 側の wire relay（normalize_agent_addr / handle_wire_* 7 本）。transport は daemon_wire、store は daemon/wire_ops（7b）
│  ── _ops（受付の続き。owner は別）──
├── conversation_ops.rs     submit / nudge / respond / interrupt / permission_mode / session_* / set_mode / now / set_model
│                             （owner = lane/state の facade + conversation::engine）
├── terminal_ops.rs         terminal demand / write / resize（owner = terminal_pump + lane/state）
├── process_ops.rs          watch_file / unwatch_file / process_* / ruby_*（owner = process_runner + file_watcher）
│  ── 既存（変えない）──
├── state.rs                AppState（+ #[cfg(test)] の共有 fixture: build_test_app_state / default_test_shell / insert_test_lane）
├── server.rs               QUIC accept loop / run_daemon / seed の呼び出し
├── repo_registry.rs        daemon → repo の in-process dispatch（dispatch_repo_method の唯一の呼び手）
├── lane/                   repo 側の lane runtime。identity・registry の SSOT は `crate::lane`（disk）
│   ├── address.rs          値: LaneAddress / ROOT_LANE_NAME / LANE_SEGMENT + parse_address（9-1b / 9-1c。
│   │                         LaneId は identity の SSOT がある crate::lane::lane_id へ = 9-1d）
│   ├── info.rs             値: LaneInfo / LaneState / LaneLifecycle / Diff / SystemEvent / LaneSessionsView（9-1b）
│   ├── enrich.rs           投影: 値に disk（session registry）と engine catalog の事実を写す。
│   │                         refresh_engine_session_id / apply_session_activity は**対で呼ぶ**（9-1c）
│   ├── pool.rs             runtime: LanePool（PtySlot / chat engine / pump / lock）+ deliver_nudge +
│   │                         idle_teardown_after_*（settings.kdl を読む runtime の調整値）
│   ├── reconcile.rs / cmd.rs / spawn_actor.rs / lifecycle.rs / ops.rs（9-1a で prefix を落として集約）
│   └── mod.rs              facade `pub use address::{…} / info::{…} / pool::{…}`（外から使う item だけ）
├── terminal_pump.rs / conversation_pump.rs
├── http/                   health / update（axum handler はこれだけ。Router は server.rs）— 7b
├── agents.rs               agent 静的 table + agents_list — 7b（旧 routes/agents）
│  （旧 routes/{daemon,delegation,wire} は daemon/{control_ops,delegation_ops,wire_ops} へ。repo の AppState に依存せず呼び手が daemon 側だけ）
└── delegation.rs / daemon_wire.rs / hub.rs / topic.rs / topic_router.rs / retained.rs / process_runner.rs / agent_spawner.rs / …
```

結果の観測（目標ではない）: `unison_server.rs` ≈ 520 行（match ≈ 145 + 残す fn 3 + test 2 本）、移動 ≈ 5,170 行。test 50 本の行き先: editor 3 / board 2 / replay 6 / conversation_ops 11 / terminal 9 / lane_ops 14 / routes/agents 1 / wire 2 / 受付 2。

### 受付に残すもの

| item | 残す理由 |
|---|---|
| `dispatch_repo_method`（≈ 145 行） | **match table は 1 枚のまま**（群ごとの sub-dispatch にしない）。唯一の呼び手（`repo_registry.rs:276`）と test 50 本が signature に依存する。`_ => Err("不明なメソッド")` の fallback を 1 箇所に保つ。method 文字列の集合が 1 file から grep できる（将来の repo-proxy drift test は `tests/vp_daemon_kdl.rs` と同じ形で書ける） |
| `payload_session_key` | 14 呼び出し（conversation 9 = ops 8 + replay 1 / lane 3 / terminal 2）。**`None` の解決先が群ごとに違う**（chat = focused / slot = root / report = Unspecified）ので doc ごと 1 箇所に置く。統一は §6 |
| `handle_process_message` | pane ops（show / toggle_pane / split_pane / close_pane）の generic relay 22 行。owner は hub |
| `QUIC_PORT_OFFSET` | `mcp.rs:748` が参照 |

arm の形は `"show" | "clear" => board::handle_canvas_command(state, payload).await,`。routing の説明コメント（「どの method がどこへ」）は arm に残し、handler の説明（`///`）は fn と一緒に動かす（orphan doc の規律、memory `orphaned-doc-comment-on-delete`）。

## 2. 依存 rule

- `unison_server` → `*_ops` / owner。`*_ops` → owner + `state`。**owner は `unison_server` を呼ばない**（例外: `payload_session_key`。`_ops` と `conversation_replay` からの参照を、§6 の統一で置き場が決まるまで許す）。
- **単一 stream 逐次**（doc 45 §5.2）: dispatch は 1 stream につき recv → handle → send。arm の中で `spawn` しない。分割で並行化しない。
- **所有領域は state の field で決まる**: board = `vpdb` の board 系 + hub broadcast / editor_bridge = `editor_pending` / conversation_replay = `replay_flights` + `replay_log` + `topic_router` / terminal = `terminal_pumps` + PtySlot / lane = `lane_pool` + ledger + `system_event_tx` / process = `file_watchers` + `process_registry` / wire = `repo_name` のみ（stateless relay）。
- `pub mod process_runner` は公開 API。`process_ops.rs` から再輸出しない。
- test の fixture（`build_test_app_state[_with]` / `default_test_shell` / `insert_test_lane`）は `state.rs` の `#[cfg(test)] pub(crate)`。移設した test は `src/` 配下の inline `mod tests`（`tests/` へは出せない）。module ごとの lock は作らない（`crate::test_env::state_dir()` が crate 唯一）。

## 3. 変えないもの（移設で verbatim に保つ不変条件）

| 領域 | 不変条件 | 守り手（test） |
|---|---|---|
| replay | `SessionInit` は `ReplayStart` の直後（`splice_session_init`）。flight 中の demand は `AppState::replay_flights` で合流し rerun を予約する。`replay_with_in_flight` は commit 世代 `seq` を読み前後で検算する。codex は buffered log を replay | `session_init_goes_after_replay_start` ほか 6 本 |
| board | append → broadcast の順。`handle_board_update` は read-modify-write。cursor の freshness | board test 2 本 + db 側 |
| lane | `lane_origin_set` / `lane_order_set` は ledger 書き → `system_event_tx` の順。`lane_session_changed` は record → emit | lane test 14 本 |
| terminal | demand は level 読み（doc 53 §2.3）。reconcile は sibling slot を触らない | terminal test 9 本（flaky 2 本は §7） |
| conversation | 書き込みは漏斗（doc 40 §4）。`record_user_message_if_transcriptless` の条件 | conversation_ops test 11 本 |
| 受付 | `dispatch_repo_method` の signature と fallback。error は `Result<Value, String>` のまま | `payload_session_key_validates_additive_param` / `delegation_dispatch_validates_before_proxy` |

## 4. PR 列（各 PR = 移設のみ）

順序は台帳どおり editor / board → conversation → 残り。test 数（`cargo test -p vantage-point -- --list`）は各 PR で不変（path だけ変わる）。

| PR | 内容 | 移動行 | 検証の要点 |
|---|---|---|---|
| PR-0 | 本 doc + 台帳 link | 0 | doc review |
| PR-1 | `editor_bridge.rs` | ~160 | test 3 本 |
| PR-2 | `board.rs`（`seed_boards` は `pub` → `pub(crate)`、`repo/server.rs` の path） | ~600 | test 2 本。`seed_boards` は def 1 + call 1 |
| PR-3 | `conversation_replay.rs`（`init_ev` 込み。`insert_test_lane` → `state.rs`） | ~600 | test 6 本。splice の順序 test が走ること |
| PR-4 | `conversation_ops.rs` | ~1,360 | test 11 本。`#[cfg(unix)]` 1 本が移ること |
| PR-5 | `terminal_ops.rs`（reconcile 糖衣 2 本も verbatim で一旦ここへ。`default_test_shell` → `state.rs`、`routes/lanes.rs` の path 2 箇所） | ~1,000 | test 9 本。flaky 2 本は isolation で判定 |
| PR-6 | `lane_ops.rs`（+ `handle_stands_list` → `routes/agents.rs`） | ~1,230 | test 15 本（lane_ops 14 + `stands_list_returns_stands_array` は `routes/agents.rs` へ）。`#[cfg(unix)]` 2 本 |
| PR-7 | `process_ops.rs` + `wire_relay.rs`（`handle_wire_*` 7 本は既に `pub(crate)`、そのまま移設） | ~350 | test 2 本。Windows `cargo check --all-targets` |
| PR-8 | reconcile 糖衣 → `impl AppState { async fn reconcile_lane / reconcile_terminal_pumps }`（本体 byte 一致、呼び手 10 箇所 + test 5 箇所） | 0（書き換え） | full test。`lane_reconcile::reconcile_lane` との名前衝突は method 化で消える |
| PR-B7 | §5。PR-1〜7 と独立、並行可 | ~250 新規 | capability の発火 test 2 本 + 実機 |
| 7b | `routes/` の改名（HTTP と domain の分離、daemon 側 core も `daemon/` へ）。別項目 | — | — |

### 照合 recipe（doc 60 §5 を server crate に流用）

1. `git show <base>:crates/vantage-point/src/repo/unison_server.rs` を base として保存
2. 新 file の top-level item 列（fn / const / struct / test fn）が base の item 列の**部分列**（順序保持）
3. item ごとに base 本体と新本体を dedent → `rustfmt --edition 2024` → diff。許容 = `use` / 可視性 / `super::` → `crate::repo::` の修飾 / section banner（`// ====`）→ `//!` module doc / rustfmt
4. test 数が base と同じ。`#[cfg(unix)]` の付き先が同じ（terminal 3 / lane 2 / conversation_ops 1）
5. module 単位の test（`cargo test -p vantage-point repo::<module>::`）→ clippy `-D warnings` → `mise run test` → Moody Blues → `gh pr create --base nightly` + auto-merge → nightly CI
6. 完了時に `unison_server::` の外部参照が `dispatch_repo_method`（repo_registry）/ `QUIC_PORT_OFFSET`（mcp.rs）/ `payload_session_key`（`_ops`）だけになる

## 5. b-7 — window 間の repo 並び順の伝播（`ReposChanged`）

### 事実（2026-09-08）

- daemon の真実源は `capability/repo_manager_capability.rs` の `repo_order`。変更の書き手（reorder / add / remove / rename / set_enabled / reload〈vpdb が Some の時〉/ sync〈remove 経由〉）は `persist_repos()` を通る。boot の `load_config` は `repo_order` を直接書いて persist を通らない = `ReposChanged` は起動時に発火しない（subscriber 不在なので実害なし）。
- 変更の broadcast は無い。vp-app の再 fetch（`daemon/pollers.rs`）は online 復帰 / 稼働数 / 登録数の変化だけなので、**order / rename / enabled は他 window に永久に届かない**。DnD は自 window だけ再 fetch。
- `daemon-repo` channel は `process_lifecycle_tx`（runtime の start / stop）を流すだけで vp-app は未使用（CLI `vp daemon --watch` 用）。`vp-daemon.kdl` には無い（`DAEMON_CHANNELS` const のみ）ので drift test は不変。
- 受け手 `on_lanes::repos_loaded` は prev を path で merge するので、push 経由で `ReposLoaded` を通しても expanded / panes / port は保たれ、auto-expand も暴発しない（`is_initial_load` = false）。

### 設計

**daemon 側**
- `daemon/protocol.rs`: `ProcessLifecycleEvent::ReposChanged`（unit variant、serde tag `repos_changed`）。exhaustive match の追従（catch-all 無し）は `daemon/server.rs` の event_log feed（`ReposChanged => continue`）と `commands/daemon.rs` の `--watch`（1 行 print）。
- `persist_repos()` の末尾、DB / KDL の書き込み成功後に **1 回**発火。注入済の `process_lifecycle_tx`（`set_process_lifecycle_tx`、`repo/server.rs` で配線）を使う。`None` なら no-op。
- unit test: tx を配線 → `reorder_repos` → `ReposChanged` を受ける。2 本目: `rename` / `set_enabled` でも発火（poll が拾えなかった case）。
- channel loop は variant を serialize するだけなので不変。

**vp-app 側**
- `daemon/subscriptions.rs::spawn_repos_subscription`: `device_subscription_loop` を雛形に（指数 backoff 500 ms → 16 s、give-up はしない）。`wait_client → open_channel("daemon-repo") → subscribe → recv loop`。`ReposChanged` を受けたら 50 ms の間に続く event を drain してから `conn.control()` → `fetch_repos_with_ports` → `AppEvent::ReposLoaded` を 1 回。daemon 側は broadcast が Lagged した時に `ReposChanged` を 1 発送って取り直しを促す（client に Lagged 分岐は無い）。
- `app/boot.rs` の `spawn_device_subscription` の隣で起動。
- doc 60 §4 の表に `repos` 行、§8 の `currents_order` から「生きている window 間は同期しない」を落とす。`pollers.rs` の count ベース再 fetch は fallback として残す（撤去は soak 後、§6）。
- 自 window の echo は無害: DnD → 楽観 order → event → fetch → prev-merge。`note_repo_order` は同じ順なので save なし。
- 互換: 旧 CLI の `vp daemon --watch` が新 daemon に繋ぐと `repos_changed` を deserialize できず stream 終了で終わる。同一 binary の install なので許容（PR 本文に明記）。

**実機**（mako、`VP_SWAP_RESTART_DAEMON=1 mise run app:swap`）: 2 window（`VP_APP_INSTANCE=1`）で window 1 の DnD → window 2 が即時に並び替わる / CLI `vp repos reorder` → 両 window / rename・enabled → 両 window（従来は届かなかった）/ `vp daemon --watch` に新しい行。

## 6. 移設に混ぜない follow-up（台帳へ）

- `Result<Value, String>` の共通 error helper（今は全 handler が `Err(format!(...))` 手書き）
- `payload_session_key` の群ごとの `None` の意味の統一（doc 40 §4 の `ReportTarget::Unspecified` を test 先行で）。統一と一緒に置き場を決めて §2 の例外を消す
- flaky PTY test の readiness を時間でなく観測に（台帳 項目 10）
- `daemon/server.rs::handle_daemon_control`（440 行 match）の分割 = 項目 7 の範囲外、7b の後に別項目
- `route_conversation` の `conversation_pump.rs` への移動 / `repo/lane/` subdirectory への集約（`lanes_state` / `lane_reconcile` / `lane_cmd` / `lane_spawn_actor` / `lane_ops` / 旧 `routes/lanes`）
- b-7 soak 後に count ベースの再 fetch（`pollers.rs`）と、自 window の操作直後の再 fetch（`on_sidebar.rs` ×4 / `flows/repo_dialog.rs` ×2。push の self-echo と二重）を撤去
- `persist_repos()` の vpdb=Some 分岐で DB 全置換が成功して kdl mirror の export だけ失敗した時、今は Err で `ReposChanged` も出ない（SSOT は変わっているのに window が古いまま）。mirror 失敗を warn に落として DB 成功で発火するかは別 PR で（既存の error 意味論を変えるため）

## 7. 落とし穴

- 日本語コメント / `///` は item と一緒に動く。section banner は `//!` に（許容差分として列挙する）。`mod` 宣言の挿入で隣の doc を付け替えない。
- `#[cfg(unix)]` は Windows build を守る gate（CLAUDE.md）。順序付き diff に attribute を含める。
- `mise run test` は 1 PR ≈ 5 分。flaky 2 本（`reconcile_touches_only_the_swapped_slot…` / `terminal_demand_start_routes_pty_output_then_stop`）は `--test-threads=1` で 3 回 isolation して判定する。
- server crate の変更は `.app` 差し替えでは効かない。実機は `VP_SWAP_RESTART_DAEMON=1`（lane が全部落ちる）で、VP の lane の外（kitty）から。
- b-7: `#[serde(tag)]` enum への variant 追加は旧 CLI との互換を切る（§5）。

## Status log

- 2026-09-08: 設計確定（mako）。決定 3 点は §0。次は PR-1（editor_bridge）。
- 2026-09-09: 9-1d 着地。`LaneId` の型を `crate::lane::lane_id`（永続の実装がある側）へ移し、`crate::lane` の `ROOT_LANE_NAME` 参照 4 箇所を定義元の `vp_paths` 直参照に。**`crate::lane` → `repo::lane` の code 依存が 0 本**になり循環が切れた（doc link は残る）。**9-1 完了**。
- 2026-09-09: 9-1c 着地。値 module から disk / engine を切り離した: `LaneInfo::refresh_engine_session_id` / `LaneSessionsView::from_registry` / `apply_session_activity` を `enrich.rs` の自由関数へ（対で呼ぶ契約を module doc に）、`LanePool::parse_address`（`&self` を取らない純パーサ）を `address.rs` へ（呼び手 53）。値（address / info）は disk / engine / lock を触らない状態になった。
- 2026-09-09: 9-1b 着地。`lane/state.rs` を `address`（値: 名前）/ `info`（値: 帳簿）/ `pool`（runtime）に分割。本文一致、test 50 本の置き場だけ変わる。値 module は disk / engine / lock を触らない（`refresh_engine_session_id` / `from_registry` / `idle_teardown_after_*` は pool に一時同居 → 9-1c で `enrich.rs`）。
- 2026-09-09: 9-1a 着地。`repo/lane/` に 6 file を集約（`lanes_state` → `lane/state`、`lane_*` の prefix を落とす）、facade `pub use` は外から使う item だけ。本文不変。次は 9-1b（state.rs の値 / runtime 分割）。
- 2026-09-09: 7b 着地。`repo/routes/` を解体: HTTP 2 file → `repo/http/`、`lanes` → `repo/lane_lifecycle.rs`、`agents` → `repo/agents.rs`、daemon 側 3 file → `daemon/{control_ops,delegation_ops,wire_ops}.rs`。rename-all-at-once、`pub use` shim なし、test 22 本は file ごと移動。
- 2026-09-09: PR-B7（b-7）着地。`persist_repos()` 末尾で `ReposChanged`、vp-app は `daemon-repo` 購読 → 50 ms drain → `repos/list` 再 fetch。実機は mako（2 window）。
- 2026-09-09: PR-1〜7（#1075 / #1076 / #1077 / #1078 / #1079 / #1080 / #1081）着地。`unison_server.rs` 5,691 → 344 行、外部参照は §4 の 3 symbol だけ。PR-8 で `reconcile_lane` / `reconcile_terminal_pumps` を `impl AppState` の method に（呼び手 10 + test 5）。残りは PR-B7 と 7b。
