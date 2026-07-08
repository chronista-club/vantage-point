# 30 — vp-app Window State 永続化 ＋ Sidebar State（per-window UI state 契約）

> Status: **Draft / 設計契約**（2026-07-08）。
> 親: doc 11（vp-app refactor — 本 doc がサイドバー記述を置換）, doc 24（vp-spine — presence=order の Model Q）,
> doc 27（apple-surface-lifecycle — multi-window / focus Model B / Model D）, `tmux-decoupling.md`（lane host モデル）。
> 実装 SSOT: `crates/vp-app/src/session_state.rs`（Window State コンテナ本体）。
> この doc は **「vp-app の 1 window が起動を跨いで復元する UI state」** を 1 枚に畳む。 中核は
> **Window State = per-window に永続化される UI state のコンテナ**であり、**Sidebar State はその 1 プロパティ**
> だという構造。 現行実装（as-built）を写し取り、実装との乖離を解消方針とともに列挙し、新設計（表示モード永続化 /
> sidebar tab の per-window 化）を末尾に置く。

## 0. 一行で

**Window が別ならサイドバーの中身も変わる。** vp-app の各 window（= OS プロセス = instance）は自分専用の
`SessionState`（`session.json` / `session.<N>.json`）に UI state を永続化する。 window geometry（位置・サイズ・
monitor・**将来: 表示モード**）と sidebar state（accordion 開閉 / 並び順 / active lane / **将来: tab**）は
すべて **per-window のプロパティ**であり、window 間で共有しない。

## 1. 背景・問題

発端は dogfooding 報告「サイドバーが `📡 loading lanes…` で止まる」。 調査の副産物として 2 つが判明した:

1. **サイドバー専用の spec/design doc が存在しない**（`find -iname "*sidebar*"` は 0 件）。 現行サイドバーの
   仕様は 15+ の doc に断片散在し、その多くが実装より古い語彙・構造（doc 11 は `SIDEBAR_HTML` vanilla JS +
   "Worker" 語彙）で書かれている。 as-built の SSOT が無い。
2. **window 永続化の入れ物（`SessionState`）は既に在る**が、その中身が「window geometry」と「sidebar 由来の
   UI state」に混在しており、**per-window であるべき state の一部（sidebar tab）が webview の `localStorage` に
   漏れている**。

これに対し user が 3 つの設計原則を提示した（本 doc の骨格）:

1. 新規 Window 起動時にその Window の状態を永続化し、再起動・次回起動で **① モニター ② 表示モード
   （fullscreen / windowed）③ 描画範囲（位置・サイズ）** を復元する。
2. **sidebar state は window state の 1 プロパティ**である。
3. **Window が別ならサイドバーの中身も変わる**（= sidebar state を含む UI state は per-window）。
4. **window の追加時・削除時・state 更新時**には、永続化層の state を更新する（write-through、§3.4a）。
5. **window の activate 時**には、描画を refresh する（§3.4c / §6.4）。

## 2. 責務の切り分け（どの state をどこが持つか）

`session_state.rs:7-15` の分類を SSOT とする。 3 層を混ぜない。

| 層 | 権威 (SSOT) | 例 | 保管先 |
|---|---|---|---|
| **Process state** | TheWorld daemon（daemon-canonical, Model Q） | running/dead/port、SP presence、per-project active lane | daemon（DB） |
| **Window State（= UI state, per-window）** | **各 vp-app instance（この doc）** | window geometry、accordion 開閉、Currents 並び、直前 active lane | `session.json` / `session.<N>.json` |
| **User preference** | `vp-app.toml`（`Settings`） | developer_mode、default_project_root | config zone |

> なぜ UI state を daemon に載せないか（`session_state.rs:13-15`）: secondary instance（`VP_APP_SECONDARY=1`）が
> 同 World に向かう時、「私はこの Lane を見る」「私はあの Lane」が両立できなくなる。 UI state は client（window）
> ごとに独立であるべき。 **これが原則 3「Window が別ならサイドバーの中身も変わる」の実装上の根拠**。

## 3. Window State モデル（as-built）

### 3.1 コンテナ = `SessionState`（per-instance）

各 vp-app instance は自分専用の session file を持つ（`session_state.rs:17-32`）。 共有 1 file 時代の
「2 process が同 file を全体書き戻し → 互いの slot を clobber」race を根治した設計。

- instance 0（primary）: `~/.local/state/vp/session.json`
- instance N（N≥1、Cmd+N で spawn した secondary）: `~/.local/state/vp/session.<N>.json`

`open` flag（`session_state.rs:132-135` + `default_open()`）が「起動時にこの window を開くか」を表す。 primary は
起動時に `session.<N>.json` を走査し `open==true` の instance を child process として auto-spawn する
（`app.rs:1798-1814`）。 clean close（`CloseRequested`）で `open=false`、強制 kill では `open=true` のまま残る
——「明示的に閉じた window は復活しない / kill された window は復元される」という直感的挙動。

### 3.2 プロパティ一覧（`session_state.rs:111-147`）

| プロパティ | 型 | 意味 | 種別 |
|---|---|---|---|
| `window_geometry` | `Option<WindowGeometry>` | 位置・サイズ・monitor（§3.3） | window |
| `open` | `bool` | 起動時に開くか（auto-spawn signal） | window |
| `projects` | `HashMap<String, ProjectUiState>` | project path → `{ expanded }`（accordion 開閉） | **sidebar** |
| `currents_order` | `Option<Vec<String>>` | Currents の project 表示順（D&D で書込） | **sidebar** |
| `active_lane_address` | `Option<String>` | 直前 active Lane（Display 形 `"<project>/conductor"` 等） | **sidebar** |
| `instance_index` | `usize`（`#[serde(skip)]`） | 自 instance 番号（file 名で表現、非永続） | — |

migration: 旧 multi-slot format `window_geometries: Vec`（PR #459）は `load()` 内で自 instance slot を
`window_geometry` に移植して読み込む（`session_state.rs:137-141, 188-197`）。 save では出さない。

### 3.3 `WindowGeometry`（`session_state.rs:81-109`）

| field | 内容 |
|---|---|
| `width` / `height` | inner size（**LogicalPixel** = DPI 補正後） |
| `x` / `y` | outer position（OS screen 全体での top-left） |
| `monitor` | `Option<String>` = tao `MonitorHandle::name()`（例 "Built-in Retina Display"） |

- **単位は LogicalPixel**。 raw physical pixel だと HiDPI 切替で破綻するため、保存時 `to_logical(scale)`、復元時
  `with_position` + `with_inner_size`（`session_state.rs:74-76`）。
- `is_valid()`（`session_state.rs:98-108`）: `width ≥ 720 && height ≥ 480 && 全て finite`。 破損・異常極小値を
  弾き、invalid なら default に fallback。 `app.rs` の `MIN_WINDOW_WIDTH/HEIGHT`（720×480）と整合。
- monitor 消失（multi-screen 切断→再接続）時は primary monitor 内に clamp して復元。

### 3.4 Window ライフサイクル契約（persist / restore / refresh）

Window の生涯にわたり、**3 つの契約**が永続化層と描画を一貫させる。

#### 3.4a persist — write-through 契約（window 追加時 / 削除時 / state 更新時に永続化層を更新）

**原則: window の追加・削除・あらゆる state 更新は、その場で `SessionState` に write-through する**（次回起動で
そのまま復元できる状態を常に disk 上に保つ）。 現行の write-through 点:

| ライフサイクル事象 | 契機 | 書く内容 | コード |
|---|---|---|---|
| **window 追加** | instance spawn（起動 / Cmd+N） | `open=true` を予約 save（連打時の index 衝突防止も兼ねる） | `app.rs:1755-1756` / `session_state.rs:329` |
| **window 削除** | `CloseRequested`（明示 close） | `open=false` + geometry | `app.rs:2008-2054` |
| **geometry 更新** | `Resized` / `Moved` | `{inner_size, outer_position, monitor}`（500ms throttle） | `app.rs:2056-2128` |
| **sidebar state 更新** | IPC（`process:toggle` / `process:reorder` / `lane:select` 等） | `expanded` / `currents_order` / `active_lane_address` | `handle_sidebar_ipc`（`app.rs:1414-`）→ `session.save()` |

書き込みは常に自 instance file への atomic write（tmp→rename、`session_state.rs:240-251`）。 geometry は
`{inner_size, outer_position, current_monitor().name()}` を `WindowGeometry` に詰めて `set_window_geometry` → `save()`。
§6.3 で tab を移設したら、tab 切替も本契約の write-through 点に加わる。

#### 3.4b restore — 起動時復元（`app.rs:1747-1788`）

`SessionState::load(instance_index)` を **WindowBuilder より前**に読み、`window_geometry()`（invalid を None に畳む
filter 付き）が Some なら `with_inner_size` + `with_position`、None なら default（`DEFAULT_WINDOW_WIDTH/HEIGHT`）。
monitor 復元は EventLoop 走行後の最初の `Resized` で `available_monitors()` を確認し、消失していれば primary 内に
clamp（macOS state restoration の async race 対策、`app.rs:1762-1774` / #428）。 sidebar state（`expanded` /
`currents_order`）は load 時に、`active_lane_address` は起動後の最初の `LanesLoaded` で実在 lane と照合して復元。

#### 3.4c refresh — window activation 時に描画を refresh ［新規、§6.4］

**原則: window が activate（focus 獲得）した時、描画を refresh する。** 現状 `Focused` ハンドラ（`app.rs:2133-2158`,
Model B）は focus 獲得時に active_lane を daemon canonical へ**報告するのみ**で、描画 refresh は行わない。 提案は、
focus 獲得時に既存の描画 refresh プリミティブ `AppEvent::LanesEnsureAll`（`app.rs:2515-2532` = 各 live lane の
`ensure_lane` 再発行 + active lane の再 `show_lane`）を発火し、加えて `push_sidebar_state` で最新 state を再注入する。
背景で window を放置している間に stale 化した表示（例: `loading lanes` 滞留、xterm 未 attach）を activate 契機で
回復させる。 詳細は §6.4。

## 4. Sidebar State（= Window State の 1 プロパティ）

サイドバー（左カラム = **CURRENTs**）の実体は SolidJS webview バンドル（`crates/vp-app/webview/src/sidebar/*`）で、
`#sidebar-root` にマウントされ、単一 WebView 内で CSS flex `280px sidebar | main` に分割される。 サイドバーが扱う
state のうち **どれが per-window に永続化され、どれが server 由来の live state か**を切り分けるのが本節の要点。

### 4.1 永続（per-window）vs live（非永続）

| 分類 | state | 保管 |
|---|---|---|
| **永続（Window State のプロパティ）** | `expanded`（project 毎）/ `currents_order` / `active_lane_address` | `SessionState` |
| **永続だが漏れている（§5-1）** | `tab`（稼働中 / 一時停止中） | ⚠ `localStorage("vp.sidebar.tab")`（window 間共有） |
| **live（永続しない、再 fetch / 再購読）** | `processes` / `lanes_by_project` / `activity(presence)` / `awaiting_input` / `session_titles` / `lane_inboxes` / `bastet_devices` | `SidebarState`（in-memory、`pane.rs:109-179`） |

> live state を永続化しない理由: `lanes_by_project` は SP `/api/lanes`（World 集約 "lanes" channel）から起動時に
> 再購読され、`presence` は `/api/health` polling で毎回埋まる。 disk に持っても起動直後に上書きされるため
> 意味が薄い（`pane.rs` の該当 field コメント）。

### 4.2 描画構造（`Shell.tsx`）

上から下へ: **CURRENTs header**（`+` = `process:add`）→ **project accordion list**（`ProjectAccordion` × N、
空なら「プロジェクトなし」）→ **稼働中 / 一時停止中 タブ**（`localStorage` 永続、§5-1）→ **WorldWidget footer**
（TheWorld status + Hub federation 行 + Bastet devices row）→ overlays（ContextMenu / FileExplorer / LanePicker /
CommandPalette / delete-hint / lane-select-hint）。

### 4.3 表現の定義（原文ママ = 実装の SSOT）

**SP presence dot（4-state）** — `ProjectAccordion.tsx:258-267`。 出所は `/api/health` の `processes[].presence`
（= `ProcessPresenceState::as_str()`, `process_manager_capability.rs:123-133`）。 entry 不在は `"unregistered"`。

| 値 | 色 / 挙動 | 意味 |
|---|---|---|
| `connected` | 緑 | SP register 済 + QUIC 生存 |
| `connecting` | warning + pulse | 再起動 in-flight（register 待ち） |
| `disconnected` | 赤 | QUIC 切断検知（respawn 待ち） |
| `unregistered` | dim（opacity .5） | 未登録 / graceful unregister 済 |

> ⚠ この presence は **SP 接続健全性**であって、doc 24/27 の presence（= order / active-lane の
> daemon-canonical, Model Q）とは**別概念**。 §5-4 参照。

**Lane connector（2 文字）** — `ProjectAccordion.tsx:44-69`。 左 1 文字 = ツリー構造、右 1 文字 = 線種。

| lane | text | cls | 駆動値 |
|---|---|---|---|
| conductor | `├─` / `└─` | `conn-conductor`（幹 = solid） | — |
| performer inactive（`pid==null`） | `┈` | `conn-dead`（休眠 = dotted, dim） | `lane.pid` |
| performer awaiting | `─` | `conn-hitl`（人を待つ = warn 色） | `awaiting_input[addr]` |
| performer 自走 | `〜` | `conn-auto`（control 手放し = info 色, 波線） | 上記いずれでもない |

> ⚠ コメントは「control surrender FSM の表現」と書くが、実駆動値は `awaiting_input`（OSC 99 boolean）であり、
> `flow.rs` の 5-state FSM ではない。 §5-5 参照。 なお `conn-run`（初期値）は 3 分岐が必ず上書きするため
> 返らない dead default（§5-6）。

**hintFor 状態機械** — `ProjectAccordion.tsx:76-86`。 SP state を先に gate し、running まで来て初めて lane 供給を見る。

```
!s || "stopped"  → expanded ? "⏳ SP starting…" : "💤 SP stopped — open to spawn"
"starting"       → "⏳ SP starting…"
"stopping"       → "⏳ SP stopping…"
"error"          → "⚠️ SP error — restart で復帰"
laneCount === 0  → "📡 loading lanes…"        ← loading lanes の滞留点（§5-3）
else             → null（= Lane 行を描画）
```

**LaneRow の要素** — `LaneRow.tsx`。 stand icon（§4.4）/ session title（`custom-title` or fallback）/
performer_status（`↑ahead ↓behind NM(dirty)`）/ awaiting dot（`!inactive && !active && awaiting_input`）/
mailbox（`lane_inboxes` unread）/ file picker ボタン。 row class = `active` / `inactive`(pid null) / `performer`。

### 4.4 stand kind ↔ 命名の対応表

サイドバーは `LaneInfo.stand` の **wire 値**を消費する（`lane.ts:15-24`）。 これと命名 SSOT
`crates/vantage-point/src/stands.rs` の対応（legacy を明示）:

| wire 値（`lane.ts`） | icon | 表示名 | stands.rs canonical | 備考 |
|---|---|---|---|---|
| `echoes` | `ph:chats-teardrop` | Echoes 💬 | id=`agent` | Coding Assistant |
| `hd` | 同上 | Echoes | — | **legacy alias**（旧 Heaven's Door） |
| `shell` | `ph:terminal-window` | Shell | id=`shell`（The Hand ✋） | 唯一 id 一致 |
| `tmux` | `ph:presentation` | Tmux | — | **retired**（tmux decoupling で撤去進行） |
| `paisley_park` | `ph:compass` | Paisley Park 🧭 | id=`canvas` | Information Navigator |
| `gold_experience` | `ph:plant` | Gold Experience 🌿 | id=`runner` | Code Runner |
| `hermit_purple` | `ph:plug` | Hermit Purple | — | **retired**（Bastet 🧲 に World 座を継承） |
| `bastet` | `ph:magnet` | Bastet 🧲 | id=`bastet` | External Control |
| （無し） | — | Justice 🌫️ | id=`justice` | **STAND_ICON に欠落**（per-lane device I/O） |

### 4.5 IPC 契約

- **inbound（sidebar → Rust）**: `crates/vp-app/schema/vp-sidebar.kdl`（protocol `"sidebar"` 1.0.0、channel
  `"ipc"`、envelope `t`、transient = response 無し）。 生成型 `IpcEnvelope` enum で typed dispatch
  （`handle_sidebar_ipc`, `app.rs:1414-`）。 15 種: `process:{toggle,reorder,restart,stop,delete,add}` /
  `lane:{select,delete,restart,add_performer}` / `stands:fetch` / `stand:select` / `project:clone:pickFolder` /
  `files:{list,open}`。
- **outbound（Rust → sidebar）**: KDL 対象外。 `SidebarState` 全体を `window.renderSidebarState(json)` に注入する
  一本道（`push_sidebar_state`, `app.rs:1333-1345`）。 files/stands の結果は個別 JS 関数 push で戻す。
  → **inbound は typed schema、outbound は非対称に丸ごと再 serialize** である点を仕様として明記する。

## 5. 実装との乖離（divergence）と解消方針

### (A) 原則・状態モデルの乖離

**5-1. tab が `localStorage`（全 window 共有）→ 原則 3 違反 ［最優先］**
`Shell.tsx` の tab signal は `localStorage["vp.sidebar.tab"]` に保存される。 localStorage は origin 単位で
全 vp-app window が共有するため、window A で「一時停止中」に切り替えると window B も変わる ——「Window が別なら
中身も変わる」に反する。 **解消**: tab を `SessionState` の per-window プロパティ（sidebar state の一部）へ移す。

**5-2. `active_lane_address` の二重ソース**
`SessionState.active_lane_address`（per-window 復元、起動後の最初の `LanesLoaded` で実在 lane と照合）と、
daemon-canonical な per-project active lane（Model Q, `/api/world/projects`）の両方が active lane を触る。 per-window
原則と「1 project の active lane は daemon 唯一」の両立を明文化する。 focus = 操舵ポインタ（Model B, doc 27）——
「今 focus している window だけが switch_lane broadcast を適用する」との関係も spec に書き、
**「active lane の選択は per-window、ただし ROTO 等の外部操舵は focus window に効く」**という二層を確定する。

**5-3. `loading lanes…` の状態表現**
SP state gate（`hintFor` の running 判定）を先に抜けるため、lane 供給（World 集約 "lanes" channel →
`AppEvent::LanesLoaded`）が **空 snapshot** または **接続失敗（`LanesError` 時は `lanes_by_project` を更新しない、
`app.rs:2533-2543`）** の間、`laneCount === 0` のまま `📡 loading lanes…` が滞留する。 **解消（spec 側）**: 状態機械に
「lane 供給の可用性」を SP state とは独立の軸として書き、**(a) lane 0 本で確定 / (b) 空 snapshot 待ち / (c) 接続失敗**
を区別する表示を提案（現状は 3 者が同じ文言に潰れている）。

### (B) 語彙衝突（spec で用語を分離・確定する）

**5-4. "presence" が 2 概念**
| 語 | 意味 | SSOT |
|---|---|---|
| presence（doc 24/27） | project の order + per-project active lane（daemon-canonical, Model Q） | doc 24 |
| **SP presence（サイドバー dot）** | SP の接続健全性（Unregistered/Connecting/Connected/Disconnected） | `process_manager_capability.rs:111-133` |

同じ語で別物。 本 doc は後者を **「SP presence（接続健全性）」** と明記して区別する。 `ProjectAccordion.tsx:91` の
コメントが SP presence dot に「daemon-canonical」を借用しており実装側でも混線 → 訂正記述を添える。

**5-5. "control surrender" が 2 系統**
| 系統 | 実体 | 駆動 | SSOT |
|---|---|---|---|
| dev-flow FSM（正） | 5-state `Idle / Working / HitlPending / Completed / Stuck` + `control_surrender: bool` | **wire message** から derive | `flow.rs:28-72` |
| サイドバー connector | 2値表現（awaiting か否か） | `awaiting_input`（OSC 99 boolean） | `ProjectAccordion.tsx:44-69` |

connector のコメントは「control surrender FSM の表現」と主張するが、実駆動は OSC 99 boolean であり `flow.rs` の
5-state FSM とは無関係。 spec は connector を **「awaiting_input 由来の 2値表現」** と正しく記し、両者を別物として扱う。

### (C) 実装内の小さな不整合（記録して将来剪定候補に）

- **5-6.** connector `conn-run` は dead code（performer 3 分岐が必ず上書き）。
- **5-7.** awaiting の非対称: connector 色は active/dead lane でも `awaiting_input` を反映しうるが、awaiting **dot**
  は `!inactive && !active` でしか出ない（`LaneRow.tsx`）。
- **5-8.** `lanes_by_project` 逆引きが `actions/handlers.ts`（`resolveProjectPathFromAddress`）と
  `FileExplorer.tsx`（`resolveProjectPath`）に重複実装。
- **5-9.** stand icon マップの legacy/retired 残存（§4.4）: `hd` / `tmux` / `hermit_purple` が残り、`justice` が欠落。

## 6. 新設計アイデア

### 6.1 表示モード（fullscreen / windowed）の永続化 ［原則 1-② の主要新規、**実装済**］

> Status: **実装済**（本 doc と同 PR、branch `mako/window-display-mode`）。 実装:
> `WindowGeometry.display_mode` 追加 + `persist_window_geometry`（`app.rs`）+ build 後 fullscreen 復元。

実装前の `WindowGeometry` は位置・サイズ・monitor のみで表示モードを保存も復元もしなかった。 実装内容:

- `WindowGeometry` に `display_mode: DisplayMode`（enum `Windowed | Fullscreen`、`#[serde(default)]`=Windowed で
  旧 session.json 後方互換。 将来 `Maximized`/zoom も候補）を追加。
- **save**: `CloseRequested` / `Resized` / `Moved` の 3 経路を共通 helper `persist_window_geometry` に集約。
  `window.fullscreen().is_some()` で分岐し、全画面時は `set_display_mode`（mode + monitor のみ更新、windowed
  座標 x/y/w/h は保持）、通常時は `set_window_geometry`（全 geometry + `display_mode=Windowed`）。
- **restore**: windowed 座標で `WindowBuilder` build 後、保存が Fullscreen なら
  `window.set_fullscreen(Some(Fullscreen::Borderless(None)))` で全画面化（`Borderless(None)` = current monitor =
  復元位置の display）。 windowed frame を base に残すので全画面解除で元の窓サイズへ戻せる。
  monitor 精密指定（保存 monitor の `MonitorHandle` を引き当て）は `available_monitors()` race を避けるため
  **§6.2 の将来課題**に切り出した。

**受け入れ基準**: fullscreen で閉じて再起動 → fullscreen 復帰（current monitor） / windowed で閉じて再起動 →
前回の位置・サイズで復帰 / 旧 session.json → Windowed に degrade。 windowed 永続化は実機確認済、fullscreen の
GUI 往復は dogfood で確認。

### 6.2 monitor 復元の厳密化（可能なら）

現状は絶対 `x,y` 保存 + 同名 monitor の available 確認 + 消失時 primary clamp。 提案: 同名 monitor の
`MonitorHandle` を `available_monitors()` から引き当て、**monitor 相対**で位置復元する（マルチディスプレイの
再アタッチ・配置変更に強くする）。 **受け入れ基準**: 外部ディスプレイを抜き差ししても window が画面外に消えない。

### 6.3 sidebar state の per-window 統一

原則 2「sidebar state は window state の 1 プロパティ」を構造で表現する。 `tab`（§5-1）を皮切りに、`SessionState`
内に **`sidebar: { tab, ... }` サブ構造**を設け、将来の per-window 化候補（scroll 位置、per-project lane 並び等）を
そこへ集約する。 これにより「Window State のプロパティとしての Sidebar State」が型の上でも一目になる。

### 6.4 window activation → 描画 refresh ［原則: activate 時に描画を refresh］

現状 `Focused`（focus 獲得）は active_lane を daemon へ報告するのみ（Model B, `app.rs:2133-2158`）。 提案:
**focus 獲得時に描画 refresh を発火する**。

- **発火**: `Focused(true)` の分岐で、既存の `AppEvent::LanesEnsureAll`（`app.rs:2515-2532`）と `push_sidebar_state`
  を実行する（前者 = live lane の `ensure_lane` 再発行 + active lane の再 `show_lane`、後者 = 最新 `SidebarState` 再注入）。
- **狙い**: 背景放置で stale 化した表示（`loading lanes` 滞留 / xterm 未 attach / 消えた pane）を activate 契機で
  回復。 §5-3（loading lanes）の自己修復手段も兼ねる。
- **冪等性**: `ensure_lane` は既存なら no-op、`push_sidebar_state` は現 state の再注入なので副作用なし。 focus-loss
  （`false`）では refresh しない（無駄な再描画を避ける）。

**受け入れ基準（実装時）**: 別 window を長時間放置 → 再 activate で 1 フレーム以内にサイドバー・console が最新化する /
`loading lanes` で放置した window が、lane 供給復帰後に activate すると lane 行へ更新される / 連続 focus で二重描画・
ちらつきが起きない。

## 7. データモデル（before / after）

```
# 現状（as-built）
SessionState {
  window_geometry: { width, height, x, y, monitor }   // ← 表示モード無し
  open: bool
  projects: { <path>: { expanded } }                  // sidebar
  currents_order: [<path>...]                          // sidebar
  active_lane_address: "<project>/conductor"           // sidebar
}
// tab は SessionState の外（localStorage）に漏れている

# 拡張案（本 doc の提案）
SessionState {
  window_geometry: {
    width, height, x, y, monitor,
    display_mode: "windowed" | "fullscreen"            // ★ 6.1 追加
  }
  open: bool
  sidebar: {                                            // ★ 6.3 sidebar state を 1 まとまりに
    tab: "running" | "paused"                          // ★ 5-1 localStorage から移設
    projects: { <path>: { expanded } }
    currents_order: [<path>...]
    active_lane_address: "<project>/conductor"          // per-window 選択（5-2 で daemon と役割分担）
  }
}
```

## 8. 関連 doc / SSOT（supersede map）

- **本 doc が置換**: `docs/design/11-vp-app-refactor.md` のサイドバー記述は全面陳腐化（`SIDEBAR_HTML` vanilla JS +
  "Worker" 語彙 → 現行 SolidJS TSX + "Performer"、rename 2026-06-07）。 サイドバー & window state の as-built SSOT は
  本 doc とする。
- **信頼できる整合 doc（引用元）**: `tmux-decoupling.md`（lane = PtySlot 直ホスト / conductor・performer /
  `--resume`）、`18-shortcut-convention.md`（f/l/n/s/d directive + overlay 群）、`09-osc-notification-capture.md`
  （awaiting dot の実挙動、ただし CSS 名 `vp-lane-pinged` → 現行 `vp-lane-awaiting`）。
- **語彙衝突の相手**: `24-vp-spine.md`（presence = order/active-lane, Model Q）/ `27-apple-surface-lifecycle.md`
  （multi-window / focus Model B、「SidebarState を World RetainedStore に寄せる」は将来方向）。 §5-4 を相互参照。
- **scope 限定**: `07-lane-as-process.md` / `24` の cockpit（4-region）/ autonomy L0–L3 / lane lifecycle FSM
  （provisioning/ready/…）は **未実装**（`07-lane-as-process.md:79` が自ら宣言）。 本 doc は**現行 flat 構造**
  （Project accordion + Lane 行 + overlays）に限定し、cockpit 構想は将来参照に留める。
- **命名 SSOT**: `crates/vantage-point/src/stands.rs`（TheWorld 👑 / Star Platinum ⭐ / Echoes 💬 / Paisley Park 🧭 /
  Gold Experience 🌿 / Bastet 🧲 / Justice 🌫️）。 サイドバー wire 値との対応は §4.4。

## 9. スコープ外（本 doc で扱わないこと）

- §6.1（表示モード永続化）は本 doc と同 PR で**実装済**。 残る §6.2（monitor 相対復元）/ §6.3（tab の
  per-window 化）/ §6.4（activate → 描画 refresh）の**実装**は別タスク・別 lane。
- `loading lanes…` バグ自体の**修正**。 本 doc は状態表現の spec 化まで（原因は空 snapshot / `LanesError` 滞留と特定済、§5-3）。
