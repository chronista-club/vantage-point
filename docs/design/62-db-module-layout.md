# doc 62 — 永続層の domain 分割（`db/mod.rs` の分離）

> **Status**: 設計確定（2026-09-09）。実装は PR-0（本 doc）→ PR-1〜5
> **Date**: 2026-09-09
> **Owners**: server crate（`crates/vantage-point/src/db/`）
> **台帳**: `.vp/reports/refactor-audit-2026-09-07.md` 項目 9-3。姉妹 doc: [doc 60](60-vp-app-layout.md)（vp-app）/ [doc 61](61-repo-runtime-layout.md)（repo runtime）— 判定基準は 3 つとも同じ
> **関連**: [doc 44](44-world-one-process.md) §5.2（DB handle は単一 + repo 次元は列）/ [doc 24](24-vp-spine.md) §4.6（lane lifecycle = 軽量 WAL、heal は寛容）/ [doc 52](52-board-redesign.md) §5（board item の identity と cursor の server 昇格）

## 0. 何を決めたか

- `crates/vantage-point/src/db/mod.rs`（3,230 行 = 本体 1,987 + test 1,243）を、**接続 1 枚 + domain 別の永続 module**に分ける。
  `db/` 配下は今この file 1 本しか無く、`impl VpDb` の 1 block（122〜1,662 行）に schema・node id・process・repos・lane・帳簿・board・service status が全部入っている。
  「board の cursor の変更理由」と「lane address の移行の変更理由」が同じ file に落ちるのが問題。
- 判定基準は doc 60 / 61 と同じ（mako）: **変更時に触る場所がまとまる** / **state と resource の所有者が明確** / **file を開いた瞬間に何の module か分かる**。行数は観測であって目標にしない。
- **新規 crate は作らない**。`crates/vantage-point/Cargo.toml:142-144` に一次証拠がある — 旧 `vp-db` crate を本 crate に物理 merge した理由が「vp-db を触ると cascade compile 3m31s、module 内なら ~15s」という実測。同じ分割を再導入しない。
- **`VpDb` と接続は共有したまま `impl` を割る**（Codex review §6）。`VpDb` の field は `db: Surreal<Any>` の 1 つだけで private。子 module は親 module の private field を見られるので、各 domain module は `impl VpDb { … self.db … }` を書くだけでよい。**型も接続も 1 つのまま**。
- **外部への diff は 0 行**。`crate::db::` の外部参照は 5 symbol しか無く（`VpDb` 21 / `SharedVpDb` 22 / `Action` 3 / `db_data_dir_for_machine` 1 / `reclaim_legacy_repo_dbs` 1）、method は `impl` がどの file にあっても `VpDb` に生えるので、5 symbol を `mod.rs` に残せば呼び手は 1 行も変わらない。
- **移設と挙動変更を分ける**。PR-1〜5 は順序付き diff で本文一致の移設だけ。dead method の撤去・`inner()` の閉じ込め・fixture の共有化は §6 の follow-up。

## 1. 目標 tree（`crates/vantage-point/src/db/`）

命名 rule: **`db/<domain>.rs` = `<domain>` の永続層**。対になる logic 側の owner は別階層に居る（`db::board` ⇔ `repo::board` / `db::ledger` ⇔ `host::ledger` / `db::lane` ⇔ `repo::lane`）。同じ domain 語で層が違うだけ、という対応を保つ。

| module | 持つもの（現行の行） | table | 本体 | test |
|---|---|---|---|---|
| **`mod.rs`**（残す = 接続と公開面） | `//!` header / `NS` / `DB_NAME` / `db_root` / `db_data_dir_for_machine` / `reclaim_legacy_repo_dbs{,_in}` / `pub use surrealdb::types::Action` / `struct VpDb` / `type SharedVpDb` / `connect_embedded` / `clear_stale_lock` ×2 / `connect_mem` / `health` / `inner` | — | ~200 | 5 |
| **`schema.rs`** | `SCHEMA_SQL`（1,668–1,982、17 table）/ `define_schema` / `normalize_legacy_lane_addresses` / `normalize_lane_addresses_in` | 全 | ~410 | 4 |
| **`node.rs`** | `load_or_create_node_id` | `node_identity` | ~45 | 2 |
| **`process.rs`** | `upsert_process` / `delete_process` / `list_processes` / `clear_all_processes` / `live_processes` | `processes` | ~85 | 4 |
| **`repos.rs`** | `upsert_repo` / `delete_repo` / `list_repos` / `export_repos` / `import_repos` / `replace_all_repos` | `repos` | ~106 | 0 |
| **`lane.rs`** | `upsert_active_lane` / `list_active_lanes` / `delete_active_lane` / `upsert_lane` / `delete_lane` / `delete_lanes_for_repo` / `replace_lanes_for_repo` / `list_lanes` / `upsert_lane_lifecycle` / `list_lane_lifecycles` / `delete_lane_lifecycle` / `delete_lane_lifecycles_for_repo` | `active_lane` / `lane` / `lane_lifecycle` | ~230 | 3 |
| **`ledger.rs`**（Repo Host 帳簿 ①②③） | `upsert_host_origin` / `get_host_origin` / `delete_host_origin` / `replace_lane_order` / `list_lane_order` / `delete_lane_order_for_repo` / `farewell_row` / farewell 7 本 | `host_origin` / `host_lane_order` / `host_farewell` | ~290 | 1 |
| **`board.rs`** | `upsert_pane_content` / `upsert_board_state` / `load_board_state` / `append_board_item` / `delete_board_item` / `update_board_item` / `set_board_cursor` / `clear_board` / `load_board` / `list_pane_contents` / `clear_pane_contents` | `pane_contents` | ~420 | 15 |
| **`service_status.rs`** | `upsert_service_status` / `list_service_status` | `service_status` | ~52 | 5 |

### 現行 banner とのズレ（移設で直る）

現行 file は section banner と中身が 4 箇所ずれている。移設は banner ではなく **table と domain** に従う。

| item | 今どこに居るか | 行くべき先 |
|---|---|---|
| `clear_all_processes`（1,045） | `// Lane lifecycle` banner の下 | `process.rs`（触る table は `processes`） |
| `delete_host_origin`（845） | `// 帳簿③: 見送りの記録` banner の下 | `ledger.rs`（対の `upsert_host_origin` は 557、288 行離れている） |
| `list_service_status`（1,651） | `// LIVE SELECT` banner の下 | `service_status.rs` |
| `replace_lane_order` 以降（601〜） | `// 帳簿①: 開発起点ポインタ` banner の下 | 帳簿②（`host_lane_order`）。`ledger.rs` に同居 |

### 分けないもの

- **`SCHEMA_SQL` は 1 定数のまま**（17 table 全部）。domain ごとに割らない。理由 2 つ:
  1. SurrealDB に **1 クエリで投げて `.check()` で全 statement のエラーを見る**。分割すると実行順と冪等性の検証が増える。
  2. `wire_messages` / `agent_cursor` / `thread_participant` / `wire_acks` / `delegations` / `prompts` / `notifications` の **7 table は `impl VpDb` に method が 1 つも無い**。CRUD は `daemon/wire_ops.rs` と `capability/delegation_store.rs` が `inner()` 経由で持っており、`SCHEMA_SQL` がこれらの唯一の定義点。domain module に割ると行き場を失う。
- **`VpDb` は 1 型・接続は 1 本**。doc 44 §5.2 の「単一 handle + repo 次元は `repo_path` 列」を変えない。

## 2. 依存 rule

- **`db/*` は runtime を呼ばない**。`repo::` / `daemon::` / `capability::` の**振る舞い**に触らない。借りてよいのは**型と純関数**だけ:
  `repos_file::RepoEntry` / `host::ledger::FarewellEntry` / `repo::lane::LaneInfo` / `repo::lane::parse_address` / `node::NodeId`。
  `parse_address` を借りられるのは **9-1c の成果**で、`repo/lane/address.rs` が disk / engine / lock を触らない値 module になったため。値と runtime を分けた辺が、そのまま永続層の依存を安全にしている。
- **domain module 同士は呼び合わない**。唯一の辺は `schema::define_schema` → 同 module 内の `normalize_legacy_lane_addresses`（起動時 migration）。
- **内部は `self.db`、`inner()` は外向けの escape hatch**。domain module が `self.inner()` を使わない（field が見えるので使う必要が無い）。`inner()` の 8 呼び手は `db/` の外（wire / delegation / hub）で、閉じるのは §6 の follow-up。
- **`mod.rs` は再輸出しない**。method は `impl VpDb` である以上どの file にあっても `VpDb` に生える。`mod.rs` に残るのは free function・型・`pub use Action` だけで、`pub use schema::*` のような facade は作らない（doc 61 と同じ規律）。
- 子 module の可視性は `mod schema;`（private mod）で足りる。`impl` block の中身は型の可視性に従うので、module を `pub` にする必要が無い。

## 3. 変えないもの（移設で verbatim に保つ不変条件）

| 場所 | 不変条件 |
|---|---|
| `connect_embedded` | lock 由来のエラーだけ `250ms * attempt` で最大 8 回 retry。非 lock エラーは即 return |
| `clear_stale_lock` | 非ブロッキング `flock(LOCK_EX\|LOCK_NB)` で live holder 不在を判定し、**flock を保持したまま `remove_file`**（TOCTOU 回避）。`#[cfg(not(unix))]` 版は no-op |
| `define_schema` | **SCHEMA_SQL 実行 → `.check()` → 旧 address 正規化**の順。正規化は戻り値を持たない = best-effort で起動を止めない |
| `normalize_lane_addresses_in` | **1 行の失敗で `?` しない**。UNIQUE 衝突が残り全行を巻き添えにするのを防ぐため warn して続行 |
| `normalize_legacy_lane_addresses` | 3 組 `(lane, address)` `(lane_lifecycle, address)` `(active_lane, lane_address)` — **列名が table ごとに違う**（doc 44 P2 の時点で実際に漏れていた） |
| `upsert_lane` / `upsert_lane_lifecycle` | DELETE + CREATE（UPDATE ではない） |
| `append_board_item` | head push / cursor の昇格 / 容量 eviction の順序（doc 52 §5） |
| `update_board_item` | in-place（read-modify-write）で cursor を動かさない |
| 全 repo 固有 table | `repo_path` 列による scope 隔離 |

## 4. PR 列（各 PR = 移設のみ、test 数 1341 不変）

| PR | 内容 | 移動行 | 検証の要点 |
|---|---|---|---|
| **PR-0** | 本 doc + 台帳 項目 9-3 から link | 0 | doc review |
| **PR-1** | `schema.rs`（`SCHEMA_SQL` + `define_schema` + 正規化 2 本）+ `mod.rs` の `//!` を接続層の実態に書き換え | ~410 | `test_define_schema_mem` / `test_define_schema_idempotent` / 正規化 2 本 |
| **PR-2** | `board.rs` + fixture `mk_item` | ~420 | board test 15 本。cursor / eviction / scope 隔離 |
| **PR-3** | `lane.rs`（descriptor + lifecycle + active_lane） | ~230 | lane 3 本 |
| **PR-4** | `ledger.rs`（帳簿 ①②③） | ~290 | `test_host_origin_round_trip` |
| **PR-5** | `repos.rs` / `process.rs` / `node.rs` / `service_status.rs` | ~290 | process 4 / node 2 / service status 5。repos は `tests/repos_db_poc.rs` が守る |

`//!` header は現行が実装から大きく遅れている（table を 5 つしか挙げていないが実定義は 17）。PR-1 で `mod.rs` は「接続・lock・公開面」を、`schema.rs` は table 一覧を正しく書く。header は doc であって本文照合の対象外なので、移設 PR に混ぜてよい。

### 照合 recipe（doc 61 §4 と同じ 2 層）

1. `git show <base>:crates/vantage-point/src/db/mod.rs` を base として保存
2. **item 単位**（`verify_move.py`）: 新 file の top-level item 列が base の item 列の**部分列**（順序保持）で、本体が dedent → `rustfmt --edition 2024` して一致
3. **line 単位**（`verify_lines.py`）: file 全体を line の多重集合として比較し、item 間の独立コメント（tombstone / `use` の理由 / banner）の落とし物を検出。**9-1b で 24 行落とした穴**がここ
4. `cargo test -p vantage-point -- --list | grep -c ': test$'` が base と同じ
5. fmt / `mise run check` / clippy `--all-targets -- -D warnings` / `mise run test`（1341）
6. Moody Blues review → `gh pr create --base nightly` → squash merge → nightly CI

## 5. test の家

- 39 test（`#[test]` 6 + `#[tokio::test]` 33）は所属 domain の `mod tests` へ一緒に動く。`#[cfg(unix)]` が付く 2 本（`clear_stale_lock_*`）は `mod.rs` に残る。
- 共有 fixture `make_test_db()`（`connect_mem` + `define_schema`）は **`mod.rs` の `#[cfg(test)] pub(crate)`** にして、全 domain module が `crate::db::make_test_db()` で使う。doc 61 で fixture の家を `repo/state.rs` に置いたのと同じ規律。
- `mk_item`（board 専用）は `board.rs` の `mod tests` へ。
- 正規化 2 本の test は `make_test_db` を使わず `connect_mem` + `define_schema` を手書きする（正規化前に旧形 row を仕込む必要があるため）。この形を保つ。
- **`repos` 群の unit test は 0 本**。実体は `tests/repos_db_poc.rs`（統合 test）にあるので、PR-5 では移動する test が無い代わりに統合 test が緑であることを見る。

## 6. 移設に混ぜない follow-up（台帳へ）

- **`make_test_db` の共有化**: 同型の fixture が `db/` の外に **10 file 16 箇所**再実装されている（`host/ledger.rs` / `daemon/wire_ops.rs` / `capability/delegation_store.rs` / `daemon/hub_client.rs` / `repo/board.rs` / `repo/lane/lifecycle.rs` / `capability/repo_manager_capability.rs` / 統合 test 3 本）。`pub(crate)` 化した後に畳む。
- **dead method の棚卸し**（**`cut-before-fix`**: まず「切っていいか」を問う）。外部呼び手 0 が 9 本 — `delete_repo` / `replace_lanes_for_repo` / `list_processes` / `clear_all_processes` / `clear_pane_contents` / `list_service_status` / `upsert_pane_content` / `upsert_board_state` / `load_board_state`。うち `upsert_repo` は `import_repos` から内部で呼ばれる（dead ではない）。移設 PR では消さない（本文一致が崩れる）。
- **`inner()` の閉じ込め**: wire（`daemon/wire_ops.rs`）と delegation（`capability/delegation_store.rs`）と hub（`daemon/hub_client.rs`）が生 `Surreal` を触っている。`db/wire.rs` / `db/delegation.rs` として method 化すれば `SCHEMA_SQL` の 7 孤立 table にも owner が付く。**挙動変更を含むので別項目**。
- `live_processes` の戻り値に `surrealdb::method::Stream` が漏れている（型が transport を露出）。

## 7. 落とし穴

- **子 module から private field が見える**のは Rust の module 階層の性質（親の private item は子孫から見える）。`db/` の外に出したら破綻するので、domain module は必ず `db/` の子であること。
- **`impl` block を跨ぐと rustfmt の indent が変わらない** — 移設先も同じ `impl VpDb {` の中なので、本体は indent も含めて byte 一致で移せる。`verify_move.py` の dedent 正規化に頼らずに済む唯一の楽な点。
- **`#[cfg(unix)]` / `#[cfg(not(unix))]` の対**（`clear_stale_lock`）を割らない。片方だけ移すと Windows build が壊れる（CLAUDE.md）。
- **`pub use surrealdb::types::Action`** を動かすと `crate::db::Action` の 3 呼び手（`repo/server.rs:1242-1244`）が壊れる。`mod.rs` に残す。
- **`SCHEMA_SQL` は raw string `r#"…"#`** で 315 行。移設時に行末空白や `"#` の位置を変えない（`.check()` の挙動は変わらないが diff が読めなくなる）。
- `mise run check` は test を見ない。`clippy --all-targets` を必ず通す（9-1b で `LANE_SEGMENT` が束縛変数に化ける寸前だったのを拾ったのはこれだけ）。

## Status log

- 2026-09-09: 設計確定。探索で `impl VpDb` の全 method（行範囲 / 可視性 / table / 外部呼び手数）と SCHEMA_SQL の 17 table、test 39 本の所属、外部参照 5 symbol を確定。PR-0 として本 doc を起票。
