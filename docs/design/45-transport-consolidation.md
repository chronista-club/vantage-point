# doc 45 — control plane transport の Unison 統一（HTTP route の棚卸し）

> **status**: 段 1〜段 4 着地（2026-07-22）／段 5 は次送り（§5.4）。doc 44 P1（fold-in）の dogfood 中に
> 「HTTP と Unison が二重化したままでは」という mako の指摘で顕在化した。
> **doc 44 とは独立**（fold-in の後始末ではなく transport 層の設計判断）。
>
> ✅ 段 4 で **control plane の HTTP route は全廃**。同じ操作に 2 経路がある中間状態は解消し、
> projects CRUD / lifecycle / lanes を触れる面は Unison `world-control` だけになった。
> HTTP に残るのは `/api/health` `/api/shutdown`（§2 の設計判断）＋ `/api/update/*`（後回し）。

## 0. 一言で

**World の control plane を Unison(QUIC) に寄せ、HTTP は `/api/health` と `/api/shutdown`
の 2 本だけ残す。** 28 route → 2 route（+ `/api/update/*` 7 本は churn が低いので後回し）。

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
4. ✅ HTTP route を撤去（`/api/health` `/api/shutdown` を除く。`/api/canvas/*` は §3.1 によりここ）
5. ⏸ `apple/` の InstanceScanner は既に機能停止（SP-portless 以降 port scan が常に空）
   なので、health 単発 probe だけ残して port scan は撤去（UI 判断とセット）
   → **次送り**（Swift のビルド確認手段が今この repo で成立しないため、§5.4）

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

### 5.3 段 4 の着地 — HTTP route の撤去（2026-07-22）

**撤去した 18 route**（`crates/vantage-point/src/process/server.rs` の Router から登録ごと削除）:

| route | method | 移設先 | 消してよい根拠 |
|---|---|---|---|
| `/api/world/projects` | GET | `world-control.projects/list` | repo 内の呼び出し元ゼロ（CLI = 段 2、vp-app = 段 3） |
| `/api/world/projects` | POST | `world-control.projects/add` | 同上 |
| `/api/world/projects/reorder` | POST | `world-control.projects/reorder` | 同上 |
| `/api/world/projects/update` | POST | `world-control.projects/update` | 同上 |
| `/api/world/projects/remove` | POST | `world-control.projects/remove` | 同上 |
| `/api/world/projects/reload` | POST | `world-control.projects/reload` | 同上 |
| `/api/world/projects/sync` | POST | `world-control.projects/sync` | 同上 |
| `/api/world/processes` | GET | `registry.list` | 同上 |
| `/api/world/lanes` | GET | `world-control.lanes/list` | 同上 |
| `/api/world/lanes` | POST | `world-control.lanes/create` | 同上 |
| `/api/world/lanes/active` | POST | `world-control.lanes/set_active` | 同上 |
| `/api/world/processes/{name}/start` | POST | `world-control.projects/start` | 同上 |
| `/api/world/processes/{name}/stop` | POST | `world-control.projects/stop` | 同上 |
| `/api/world/processes/{name}/restart` | POST | `world-control.projects/restart` | 同上 |
| `/api/world/processes/{name}/pointview` | POST | `world-control.projects/pointview` | 同上 |
| `/api/canvas/switch_lane` | POST | **移設せず撤去** | §3.1: 宛先 `canvas_senders` に書き手がおらず end-to-end で dead |
| `/api/canvas/layout` | GET | **移設せず撤去** | §3.1: `load_canvas_layout` の呼び出し元がこの handler だけ |
| `/api/canvas/layout` | POST | **移設せず撤去** | §3.1: `save_canvas_layout` の呼び出し元がこの handler だけ |

**残した route と理由**:

| route | 理由 |
|---|---|
| `/api/health` `/api/shutdown` | §2。**VP 外に消費者がいる**（`.mise/tasks/app/swap` の Ruby / `apple/VantagePointAgent` の Swift）ことに加え、health は「他が壊れている時に動いてほしい」probe なので Unison 層が wedge した時に診断手段ごと失わないための**意図的に鈍い外殻**。shutdown も同じ（緊急停止は最も単純な経路であるべき） |
| `/api/update/*`（7 本） | §3 のとおり後回し（self-update は churn が低い）。段 4 のスコープ外 |

**「消してよい証拠」の取り方**: repo 内 grep で呼び出し元ゼロを確かめるだけでは足りない
（HTTP は文字列で叩けるので、**repo 外の消費者**が grep に映らない）。`.mise/tasks`（Ruby）/
`apple/`（Swift）/ webview（TS）を横断して `/api/` を数え、`/api/health` `/api/shutdown` 以外に
外部消費者がいないことを確認した上で落とした。`/api/health` が残る理由がまさにこれ。

**「残っていないか」の確認**: 撤去 PR の危険は 2 方向ある —— (a) 残すべきものを巻き添えで落とす、
(b) 消したつもりが登録／handler の片方だけ残る。方向ごとに別の網を張った:

- **route 登録**: Router 構築を `build_world_router()` に切り出し、`world_router_keeps_health_and_shutdown`
  / `world_router_drops_removed_control_routes` / `world_router_keeps_update_routes` が実際に
  oneshot して 200 / 404 を突き合わせる。**登録表そのものがテストされる**ようになった。
- **handler の残骸**: `cargo clippy --workspace --all-targets -- -D warnings`。route を落とすと
  handler は「誰も呼ばない `pub(crate)` 関数」になるので dead_code が拾う。これで
  `save_canvas_layout` / `load_canvas_layout` が dead になったことも発覚し、同 PR で撤去した
  （`CANVAS_LAYOUT_PANE_ID` は残す — 過去に書かれた reserved row を `restore_panes` が
  pane 一覧から除外し続ける必要があるため）。
- **stale な doc 参照**: 撤去した path 文字列を repo 全体に grep し直し、現在形で書かれた
  コメント（`caller が /api/world/processes/{name}/restart を呼ぶ` 等）を Unison method 名に直した。

**parity テストの行き先**: 段 2 / 段 3 では「新面が旧面と同じ答えを返す」を HTTP との突き合わせで
担保していたが、段 4 で突き合わせる相手が消える。**parity が守っていた中身（合成 update の意味論 /
lanes の filter+sort / reorder の並び / snapshot の写し方）は Unison 入口に対して期待値を直接
書き直した** —— 旧面が消えたからといって期待値まで消すと、移行で守ったものが黙って外れる。

**残った尻尾（本 PR のスコープ外）**:

- `AppState.canvas_senders` は書き手ゼロのまま残る。読み手が `/api/health` の
  `stands.paisley_park.clients`（常に 0）にまだ 1 つあり、消すと health の応答形が変わるため。
  health を触らない方針とセットで据え置く。
- `ProcessManagerCapability::open_pointview` は project の port に `POST /api/canvas/open` を
  投げるが、**この route は Router に存在したことがない**（かつ portless で port=0）。
  `projects/pointview` は Unison 面として生きているので、中身の作り直しは別 task。

### 5.4 段 5（`apple/` の port scan 撤去）を次送りにした理由（2026-07-22）

**先に別の壊れが挟まっているため、今は検証しながら直せない。**

`apple/VantagePointAgent/project.yml` は Swift client を sibling repo の
`../../../club-unison/clients/swift` に local path 依存で引いている。club-unison は
**`Package.swift` を repo root へ移した**（club-unison #82 — SPM の url+version 配布は
「repo root の manifest」を要求するため）。VP 側の path は `clients/swift` を指したままなので、
`xcodegen generate` は通るが `xcodebuild` が

```
the package manifest at '.../club-unison/clients/swift/Package.swift' cannot be accessed
```

で package 解決に失敗する。**doc 45 とは無関係の pre-existing な依存 drift**で、段 5 に着手する前に
「path を repo root に向ける」か「#82 で可能になった remote url+version 依存に切り替える」かの
判断が要る（判断そのものが別 PR の粒度）。

ビルドが通らない状態で Swift を編集すると、**壊した所と元から壊れていた所が区別できない**。
`InstanceScanner` の port scan は SP-portless 以降ずっと空を返しているだけで（= 無害な no-op、
menu が常に「稼働中 Process: 0」と出る）、急いで消す理由もない。段 5 は次の PR に送る。

段 5 でやること（変わらず）:

- `InstanceControl.scan()` の port range probe（33000-33015 を 16 並列）を撤去。
  portless 以降 project は listen しないので、この scan は原理的に常に空。
- health 単発 probe は残す。稼働中 project 一覧が要るなら、World の `/api/health` が返す
  `processes[]`（presence 一覧）が既にその答えを持っている。
- menu の「稼働中 Process: N」/「停止」ボタンをどうするかは UI 判断とセット
  （per-project stop は Unison `world-control.projects/stop` が正面）。
