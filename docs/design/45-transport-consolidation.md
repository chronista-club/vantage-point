# doc 45 — control plane transport の Unison 統一（HTTP route の棚卸し）

> **status**: 段 1 + 段 2 + 段 3 着地（2026-07-22）／段 4-5 未着手。doc 44 P1（fold-in）の dogfood 中に
> 「HTTP と Unison が二重化したままでは」という mako の指摘で顕在化した。
> **doc 44 とは独立**（fold-in の後始末ではなく transport 層の設計判断）。
>
> ⚠️ 段 3 まで済んだ現在は **同じ操作に HTTP と Unison の 2 経路がある中間状態**。これは
> 意図した設計で（§5 の順序 = 新面が動くことを確かめてから旧面を落とす）、HTTP route の
> 撤去は段 4 で行う。実装は共有関数に畳んであるので、2 経路が別々の答えを返すことはない。
> **HTTP 側は既に消費者ゼロ**（CLI = 段 2 / vp-app = 段 3 で移設済、`/api/health` は除く）。

## 0. 一言で

**World の control plane を Unison(QUIC) に寄せ、HTTP は `/api/health` と `/api/shutdown`
の 2 本だけ残す。** 28 route → 2 route。

## 1. なぜ寄せるか

transport が 2 つあること自体より、**HTTP 側には Unison 側にある足場が無い**ことが問題。

| | Unison | HTTP |
|---|---|---|
| schema | `crates/vantage-point/schema/vp-daemon.kdl`（typed request / returns） | 無し（手書き axum handler + ad-hoc JSON） |
| drift 検出 | あり（`tests/vp_daemon_kdl.rs`、source 突き合わせ） | **無し** |
| MCP tool 化 | unison-mcp が自動合成 | されない |

VP は AI ネイティブ開発環境なので、**Unison に乗せた面はそのまま agent が触れる面になる**。
これは副次効果ではなく本筋の利得。

### 実害（2026-07-21 時点）

- **processes 一覧を取る経路が 3 つ**ある: Unison `registry.list` / Unison
  `world-process.list` / HTTP `GET /api/world/processes`。同じ情報に 3 実装。
- doc 44 PR3 の `vp ps` 実装で、当初この HTTP 版を選んで 4 つ目の依存を足しかけた
  （commit `f1dea10` で Unison に書き直した）。面が 1 つなら起こらない間違い。

## 2. なぜ 0 本ではなく 2 本残すか

`/api/health` と `/api/shutdown` は **VP 外に消費者がいる**:

- `.mise/tasks/app/swap`（Ruby）: 「`/api/health` の 200 を単一の真実源にする」
- `apple/VantagePointAgent/Sources/InstanceScanner.swift`（Swift menu bar agent）

これらを Unison に寄せると Ruby / Swift に Unison client を持たせる必要が出る。さらに
**health は「他が壊れている時に動いてほしい」probe** で、Unison 層が wedge した時に
health も Unison だと診断手段ごと失う。**意図的に鈍い外殻**として HTTP を残すのは
統一の失敗ではなく設計。`/api/shutdown` も同じ（緊急停止は最も単純な経路であるべき）。

## 3. route の行き先

| route 群 | 本数 | 行き先 | 備考 |
|---|---|---|---|
| `/api/world/projects*` | 7 | **Unison `world-control`** | projects CRUD。CLI は既に world-control、vp-app は HTTP |
| `/api/world/processes*`（start/stop/restart/pointview） | 4 | **Unison** | lifecycle。`projects/start\|stop` は doc 44 で移設済 |
| `/api/world/lanes*` | 2 | **Unison** | `lanes/list` は `f1dea10` で world-control に新設済 |
| `/api/canvas/*` | 2 | **撤去**（訂正） | 段 1 の実測で消費者ゼロと判明 → 下記 |
| `/api/world/{port_for,refresh}` | 0 | — | **doc 44 P1 で撤去済**（この行は起票時の見落とし） |
| `/api/update/*` | 7 | **Unison**（優先度低） | self-update。churn が低いので後回しでよい |
| `/api/health` `/api/shutdown` | 2 | **HTTP 維持** | §2 |

### 3.1 起票時の見立てからの訂正（段 1 実装時の実測、2026-07-22）

- **`/api/world/{port_for,refresh}` は既に存在しない**。どちらも doc 44 P1（fold-in）で
  撤去され、`process/server.rs` / `routes/world.rs` にその旨のコメントだけが残っている。
  「要精査」は「精査したら 0 本だった」で決着。
- **`/api/canvas/*`（layout / switch_lane）は end-to-end で dead**。
  `switch_lane` の宛先である `AppState.canvas_senders` は**どこからも populate されない**
  （旧 localhost browser Canvas の WS 撤去で書き手が消えた）ので、常に 0 client に送っている。
  `layout` の `load/save_canvas_layout` も呼び出し元がこの 2 handler だけ。
  CLI / MCP の `switch_lane` は process-proxy 経由の別経路で、この route を通らない。
  → **Unison に移すと「読み手のいない書き込み」を新設することになる**（doc 45 §1 が避けたい
  dead tool そのもの）。移設先ではなく段 4 の撤去対象に置く。

> 教訓: 「HTTP にある = 誰かが使っている」ではない。移設の前に**消費者を数える**。
> 数えた結果 0 なら、移設は仕事を増やすだけで面は 1 本も減らない。

## 4. 波及の大きさ

- ✅ **vp-app の REST client 12 method のうち 10 が消える**（`crates/vp-app/src/client.rs`）。
  vp-app は既に Unison を 36 箇所で使っているので、二重 transport を抱える理由が消える。
  残る 2（`world_health` / `ping`）は §2 の health 面で、実際は 1 に減った（§5.2）。
- ✅ CLI の `commands/config.rs` / `commands/daemon.rs` / `discovery.rs` の HTTP 呼び出しを
  Unison client に差し替え。
- ✅ `WorldControlClient` に不足 method を追加（既存の projects/* と同じパターン）。

## 5. 進め方（案）

fold-in（doc 44）が nightly に落ち着いてから着手。route 群ごとに小 PR に割る:

1. ✅ `world-control` に不足 RPC を出す（server + client + KDL + drift テスト）
2. ✅ CLI の HTTP 呼び出しを Unison に差し替え（`vp ps` は `f1dea10` で完了済み）
3. ✅ vp-app の `client.rs` を Unison に差し替え（`app.rs` の既存 Unison 経路と統合）
4. HTTP route を撤去（`/api/health` `/api/shutdown` を除く。`/api/canvas/*` は §3.1 によりここ）
5. `apple/` の InstanceScanner は既に機能停止（SP-portless 以降 port scan が常に空）
   なので、health 単発 probe だけ残して port scan は撤去（UI 判断とセット）

各段階で「HTTP を消す前に Unison 経路が実機で動く」ことを確認してから旧経路を落とす
（doc 44 で確立した「新面が動く → 旧面撤去」の順序）。

### 5.1 段 1 + 段 2 の着地（2026-07-22）

**新設した world-control RPC**（すべて `handle_world_control` + `WorldControlClient`）:

| request | 旧 HTTP route |
|---|---|
| `projects/update` | `POST /api/world/projects/update` |
| `projects/sync` | `POST /api/world/projects/sync` |
| `projects/reload` | `POST /api/world/projects/reload` |
| `projects/restart` | `POST /api/world/processes/{name}/restart` |
| `projects/pointview` | `POST /api/world/processes/{name}/pointview` |
| `lanes/create` | `POST /api/world/lanes` |
| `lanes/set_active` | `POST /api/world/lanes/active` |
| `lanes/list`（filter/sort parity を追加） | `GET /api/world/lanes` |

**入口は 2 つ、実装は 1 つ**: route 層にしか無かった orchestration は `routes::world` の
`apply_project_update` / `collect_lanes` / `resolve_create_lane_args` に括り出し、HTTP と
Unison の両方がこれを呼ぶ。移行の正しさ（新旧が同じ答えを返す）は
`daemon/server.rs` の parity テスト群が固定する。

**KDL の扱い**: world-control に描くのは read-safe な request だけ、という既存の
キュレーション方針を維持した（mutation は手で叩くと状態を壊すので unison-mcp に露出させない）。
新設分は全て mutation なので `tests/vp_daemon_kdl.rs` の `WORLD_CONTROL_OMITTED_BY_DESIGN` に置く。
併せて **drift 検出の逆方向**を追加した — 従来は「KDL ⊆ source」しか見ておらず、handler に
method が増えても KDL に書き忘れれば素通りしていた（= 露出させるか否かを誰も判断しないまま
面が増える）。新テストは「全 method は KDL に記述されるか、omission list に明示されるか」を要求する。

**移行のついでに直った取りこぼし**: `vp daemon status` の Processes 一覧は、
`{"processes": [...]}` を `json.as_array()` で受けていて常に None に落ち、**一度も表示されて
いなかった**。port / pid は fold-in で無意味（port=0 / pid=World 自身、doc 44 §5.3）なので
表示を project path に差し替えた。

### 5.2 段 3 の着地（2026-07-22）

**vp-app の REST client を Unison に寄せた**。`client.rs` の 12 method のうち 10 を移設し、
新設 module `crates/vp-app/src/world_control.rs`（`WorldControl`）に集約した。
**server 側の RPC 追加はゼロ** — 段 1 で出した面がそのまま足りた。

| vp-app の method | 旧 HTTP | 新 Unison |
|---|---|---|
| `list_projects` | `GET /api/world/projects` | `world-control.projects/list` |
| `list_processes` | `GET /api/world/processes` | `registry.list` |
| `add_project` | `POST /api/world/projects` | `world-control.projects/add` |
| `start_process` | `POST /api/world/processes/{name}/start` | `world-control.projects/start` |
| `stop_process` | `POST /api/world/processes/{name}/stop` | `world-control.projects/stop` |
| `restart_process` | `POST /api/world/processes/{name}/restart` | `world-control.projects/restart` |
| `remove_project` | `POST /api/world/projects/remove` | `world-control.projects/remove` |
| `reorder_projects` | `POST /api/world/projects/reorder` | `world-control.projects/reorder` |
| `set_active_lane` | `POST /api/world/lanes/active` | `world-control.lanes/set_active` |
| `create_performer_lane` | `POST /api/world/lanes` | `world-control.lanes/create` |
| `world_health` | `GET /api/health` | **HTTP 維持**（§2） |
| `ping` | `GET /api/health` | **撤去**（消費者ゼロ、`daemon_launcher` が別に probe を持つ） |

`client.rs` は空にならず **`/api/health` 1 本 + wire 型**が残る。これは §2 の設計判断どおりで、
「統一の取りこぼし」ではない。wire 型（`ProjectInfo` / `LaneInfo` 等）は transport 非依存なので
置き場を変えていない（読み手は Unison client / QUIC 購読 / sidebar push の 3 者）。

**接続は増やさない**: RPC は `SharedWorldConn`（F1b の 1 QUIC connection）上に**call ごとに
stream を開いて閉じる**。1 本の stream に相乗りさせない理由は、World 側の world-control
handler が **1 stream につき逐次**（recv → handle → send を直列）だから — 数秒かかる
`projects/restart` と 5s 周期の poll が同じ stream に載ると poll が待たされる
（head-of-line blocking）。stream を分けると旧 HTTP の「1 リクエスト = 1 独立した往復」が
そのまま保たれる。`close()` は必須（drop 任せは recv task と stream のリーク）。

**picker thread の使い捨て runtime を廃止**: `project_dialog.rs` は blocking な rfd picker /
`git clone` のために専用 OS thread を立て、その中で使い捨ての tokio runtime を回していた。
共有 QUIC connection は app の shared runtime で駆動されているので、別 runtime から触れない。
async 部分は `Handle::spawn` で shared runtime に渡す形に変えた（blocking 部分は thread に残る）。

**parity の固定**（新面が旧面と同じ答えを出す）:

- server 側（`daemon/server.rs`）: `registry_list_matches_http` / `world_control_add_remove_matches_http` /
  `world_control_reorder_matches_http` を追加。段 2 の 3 本と同じ骨格（同一 capability を
  2 入口から叩く / 同一初期状態の 2 capability を別入口で操作して突き合わせる）。
- client 側（`vp-app/src/world_control.rs`）: **wire shape の差**を decode テストで固定。
  `projects/list` は裸配列、旧 HTTP は `{"projects": [...]}` という**包み 1 枚**の差があるので、
  旧 shape の struct をテストに残して両方が同じ `Vec<ProjectInfo>` に落ちることを見る。

**ついでに畳んだもの**: `registry.list` の応答は `RunningProcess` を手書き object で写していた
（HTTP route は derive Serialize）。同じ map の写し方が 2 実装ある状態だったので、
`registry_process_snapshot()` に括り出して `serde_json::to_value` 1 本にした。
field を足した時に片方だけ古いまま、が構造的に起きなくなる。
