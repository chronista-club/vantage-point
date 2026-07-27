# Guide: VP メッセージング (wire / dev-flow FSM / federation)

> **Status**: 実装同期 doc（2026-07-12 新設 / 2026-07-13 に channel E（#738）+ Wire Inbox V1（#742）を反映、`msg-doc-sweep` lane）。
> **Scope**: agent 間 messaging の全体像を 1 本にまとめた見取り図 — wire 基礎 / dev-flow FSM の投影 / federation（cross-PC）。
> **SSOT は実装**。この doc はコードに追いつくための地図であり、事実は下記の一次資料から採取している。矛盾を見つけたらコードを正とし、この doc を直す。

一次資料（本 doc の全記述の裏付け）:

| レイヤー | ファイル | 何の真値か |
|---|---|---|
| wire store | `crates/vantage-point/src/capability/wiremsg_store.rs` | store / cursor / thread / ack 台帳 |
| repo→daemon transport | `crates/vantage-point/src/process/world_wire.rs` | wire の中央化 transport（QUIC "wire" channel） |
| dispatch | `crates/vantage-point/src/process/routes/wire.rs` / `src/daemon/server.rs` | channel method → store dispatch |
| delivery loop | `crates/vantage-point/src/process/delivery_actor.rs`（repo 受け口 = `unison_server.rs` の `lane_nudge` / `conversation_nudge`） | 未 ack command の再掲示（nudge、`console_mode` で channel C/D/E 分岐） |
| FSM | `crates/vantage-point/src/flow.rs` | FlowState と derive 規則 |
| 投影 | `crates/vantage-point/src/daemon/server.rs`（enrich / "lanes" channel） | flow_state を vp-app へ届ける経路 |
| federation | `crates/vantage-point/src/daemon/{hub_client,dialer}.rs` / `src/daemon/*` | register / discover / direct→relay |

関連 doc（重複を避け、深掘りはそれぞれへリンク）:

| doc | 扱う範囲 |
|---|---|
| [`AGENTS.md`](../../AGENTS.md) | cross-agent の最小 wire 規約（`needs_user` の使い分け） |
| [`dev-flow-primitives.md`](./dev-flow-primitives.md) | `flow_handoff` / `flow_progress` の tool signature・CLI 例・FSM 詳細 |
| [`wire-address-usage.md`](./wire-address-usage.md) / [`spec/wire-address-v3.md`](../spec/wire-address-v3.md) | address 文法（`actor@machine/repo/lane`） |
| [`design/28-agent-delegation.md`](../design/28-agent-delegation.md) | `delegate` / `respond` / `complete`（委譲、wire とは別系統） |
| [`design/tmux-decoupling.md`](../design/tmux-decoupling.md) | lane console の PtySlot 直ホスト（**Tui lane の** nudge 着地先 = channel C。chat lane は channel E で engine に注入、§1.7） |
| [`design/34-wire-act2-delivery.md`](../design/34-wire-act2-delivery.md) | channel E（chat lane への構造化配送）と wire 可視化（Wire Inbox）の epic 設計 |

---

## 0. 全体像（3 レイヤー）

```
[MCP tool]  wire_send / wire_recv / wire_inbox / wire_ack / wire_thread
[CLI]       vp wire send|recv|inbox|ack|thread|watch|discover
   │  どちらも repo or 直結で ↓ の中央 transport に収束
   ▼
world_wire::call  ── QUIC "wire" channel ──▶  daemon :32000
                                                └─ WiremsgStore（唯一の writer / 中央 store）
                                                └─ delivery_actor（未 ack command を nudge → channel C/D/E、§1.7）
                                                └─ enrich_lanes_flow_state（送信時 derive）
                                                     └─ "lanes" channel ──▶ vp-app sidebar
```

要点は 3 つ:

1. **wire store は daemon（:32000）に中央化**されている。repo も CLI も MCP も、`world_wire::call` の QUIC "wire" channel 経由で中央 store を読み書きする（`world_wire.rs` module doc）。**daemon 停止 = wire 停止**（設計決定 D1-c で許容済）。
2. **flow_state は store しない**。performer の状態は wire 活動から毎回 derive され、daemon が vp-app へ snapshot を送る直前にだけ付与される。
3. **federation（cross-PC）は direct → relay の 2 段**。到達できれば direct（QUIC connect race）、届かなければ hub relay という「常に生きている最下段」に降格する。

---

## 1. wire messaging 基礎

### 1.1 中央 store モデル

`WiremsgStore`（`wiremsg_store.rs:185`）は daemon の in-process DB（embedded SurrealDB）を持つ唯一の writer。repo の wire ハンドラも CLI も、`world_wire::call(path, payload)`（`world_wire.rs:124`）で `/api/wire/*` という論理 path を投げ、それが QUIC "wire" channel の method（`wire/send` 等）として daemon に届く。repo は **1 プロセス 1 本の永続 QUIC 接続**を再利用する（per-call 新造は fd leak を起こし、過去に RLIMIT_NOFILE 枯渇で mesh が全滅した経緯がある — `world_wire.rs` module doc）。

store が扱う table:

| table | 役割 |
|---|---|
| `wire_messages` | message 本体（`prev` / `from_addr` / `to_addrs` / `body` / `created_at` / `local_seq`） |
| `agent_cursor` | per-agent 単一の既読 cursor（`agent` / `last_read` = local_seq） |
| `wire_acks` | ack 台帳（`message_id` + `agent` UNIQUE）。cursor とは独立 |
| `thread_participant` | mute/left の sparse 例外表（該当行のみ持つ） |

### 1.2 per-agent 単一 cursor（`local_seq` 厳密単調）

- 既読管理は **per-agent 単一 cursor**（`agent_cursor`、1 agent 1 行）。「未読」は thread ごとの participation ではなく `agent ∈ to_addrs AND local_seq > last_read` で derive する（`fetch_unread`、`wiremsg_store.rs:340`）。
- `local_seq` は **ローカル accumulation の厳密単調 ingestion 順序**。`WiremsgStore` が持つ `Arc<AtomicU64>` の `fetch_add(1)` を INSERT 毎に採番し、起動時に `math::max(local_seq)` で復元する（`wiremsg_store.rs:194`）。
- **なぜ `created_at`（epoch ms）でなく `local_seq` か**: 旧 cursor は `created_at` 比較で、同一 ms 衝突や cross-process clock skew で message を取りこぼした。`local_seq` はこれを構造的に防ぐ（R1、決定 F4-2）。`created_at` は thread 内の表示順のためだけに残り、cursor 比較には使わない。
- `wire_recv` の 1 回で「未読取得 + cursor 前進」を行う（`recv_and_advance`、`wiremsg_store.rs:346`）。cursor は取得済 message の `local_seq` 最大値まで進む。未読が空なら cursor は触らない。

### 1.3 thread = `prev` parent-pointer forest

- thread 構造は **`prev`（親 message id）一本**で表す。`prev = None` が root。`thread_id` は R1 で全廃され、「thread の識別子」が要る場面では `prev` を root まで辿った先の id を使う（`walk_to_root`）。
- reply は **reply-all**: `send_reply`（`wiremsg_store.rs:271`）が返信先 `prev` の参加者集合（`prev.from ∪ prev.to`）を継ぎ、`left` を除外、送信者自身を除外して `to` を展開する。各 reply が親の参加者集合を継ぐので **thread 全走査は不要**（`prev` 1 件で足りる）。

### 1.4 category 意味論（delivery policy）

`body.category` は message の配送ポリシー selector。値域と意味:

| category | 配送 |
|---|---|
| `command` | **受信者が `wire_ack` するまで delivery loop が再掲示（nudge）する**（§1.7）。「送った、読んでほしい」の常用系 |
| `event` | fire-and-forget の FYI（nudge しない） |
| `state` / `data` / `log` | 同じく非 nudge（用途別ラベル） |

- **default = `command`**、ただし **MCP `wire_send` 経路のみ**が注入する（`wire_send_impl` が `body` に `category` を `or_insert("command")`、`mcp.rs:962`）。**CLI `vp wire send` は default を注入しない** — `--category` を明示した時だけ `body.category` が付く（`commands/wire.rs:690`）。この非対称は「CC 限定 scope に default を閉じる」意図的な設計で、delegation やサーバ内部 sender を巻き込まないため（`mcp.rs` コメント）。
- 消費側: daemon の `dispatch_wire("send")` が `body.category == "command"` を見て delivery loop を即 wake する（`routes/wire.rs:361`）。

### 1.5 kind taxonomy

`body.kind` は dev-flow FSM の入力になる自由フィールド。**規約は convention であり enforcement ではない** — `wire_send` の schema は kind を enum 制約せず、default 注入もしない。不明な kind は FSM の fallback で `Working` に倒れる（`flow.rs:257`）。

| kind | direction | 意味 |
|---|---|---|
| `task` | conductor → performer | 初手 handoff spec |
| `question` | performer → conductor | 質問 / decision 依頼（conductor が捌ける相談） |
| `needs_user` | performer → conductor | **ユーザ本人**の意見が要る相談（ack まで `awaiting_user`） |
| `ack` | performer → conductor | 受領 / progress |
| `decision` | performer → conductor | 自己判断表明 |
| `approve` / `modify` / `clarify` | conductor → performer | reply |
| `complete` | performer → conductor | 完了報告 |
| `request` | performer → conductor | action 依頼（dogfood 等） |

実装が実際に `body.kind` をセットするのは今のところ `flow_handoff`（`kind:"task"` を注入、`mcp/lane.rs:526`）くらいで、`needs_user` 等は performer 側の CC が body を手で組む前提（category と違い注入ロジックは無い）。

### 1.6 ack 台帳と read cursor の独立性

**これは wire で最も間違えやすい点**: `wire_recv` で受信して cursor が進んでも、それは **handled（処理済）を意味しない**。

- `wire_acks` table（`ack`、`wiremsg_store.rs:446`）は `agent_cursor` とは**完全に独立**。ack は「この message を処理し終えた」の台帳で、`message_id + agent` UNIQUE の冪等記録（初回 `true` / 再 ack `false`）。
- `command` category の message は、**受信済でも ack されるまで** delivery loop（§1.7）の再掲示対象に載り続ける（`unacked_commands`、`wiremsg_store.rs:510`。ack 済 agent と送信者を除いた宛先が 1 人でも残るものを拾う）。
- したがって wire を扱う agent は「recv して内容に従って処理 → **処理後に `wire_ack`**」を守る。ack を忘れると 10 分ごとに再 nudge される。

### 1.7 delivery loop（未 ack command の nudge）

`delivery_actor.rs` の常駐 actor が、未 ack の `command` を受信者へ nudge する。

- 周期パラメータ: `TICK` 30s、`RENUDGE_AFTER` 600s（= 10 分）、同一 `(message, agent)` への nudge 上限 `MAX_NUDGES` 3（`delivery_actor.rs:50-58`）。
- **配送経路は受信者 lane の `console_mode` で分かれる**（分水嶺は `NudgeTarget::nudge_method()`、`delivery_actor.rs:142`。#738 / doc 34 §3）。pulse ループの `if console_mode == Tui`（`delivery_actor.rs:419`）一箇所が Tui と Chat を切り分ける:
  - **Tui lane** — CC activity poll（`agents --json`）で readiness を判定（R3-a）してから配送:
    - idle / waiting（or poll 不能の degraded）→ **channel C** `lane_nudge` を所有 repo の control channel へ forward（`unison_server.rs:707` `handle_lane_nudge`）→ `deliver_nudge`（`lanes_state.rs:1017`）→ `write_to_lane` が **PtySlot に直書き**（`lanes_state.rs:781`）。**tmux 非依存**（tmux decoupling 後、lane = repo の PtySlot 直ホスト）。
    - busy → 待つ（台帳を進めず、次 pulse で idle 遷移を拾う）。
    - CC interactive session 不在（`Some(None)`）→ **channel D** headless bg dispatch: `claude -p [--resume <cc_session_id>]` を detached 起動して wire を処理させる（`delivery_actor.rs:427-472` / `spawn_bg_dispatch:541`。`BG_REDISPATCH_AFTER` 600s × `MAX_BG_DISPATCHES` 2、別台帳 `bg_ledger`）。lane 不在 / Dead は channel D 対象外（cwd/session の足場が無い）で pending 保持。
  - **Chat lane** — **channel E** `conversation_nudge` を forward（`unison_server.rs` `handle_conversation_nudge`）→ `ensure_and_submit_chat`（`unison_server.rs:547`）が engine（`ClaudeHost`）へ nudge 文言を **1 ターンとして submit**（`lanes_state.rs:957` `submit_chat`）。chat lane は **readiness も channel D も通らない（#738）**: engine は lazy spawn なので Offline が無く、turn 実行中の submit も engine 側が自前 queue するので Busy が無い → 常時 deliverable（doc 34 §3、Step 0 spike ①実測）。そもそも chat lane は PtySlot を持たず `lane_nudge` は構造的に `Err("Lane has no PtySlot")` になる（`lanes_state.rs:785`）ため、この分岐は同時にバグ修正でもある（旧: 30s ごとに無限リトライ）。payload は両 method とも `{lane, text}` 共通。
  - **delegation reconcile も同じ `nudge_method()` 分岐**を使う（`delegation.rs:436`）。delegation record の re-nudge（`respond` / `complete` 待ち、[design/28](../design/28-agent-delegation.md)）も chat lane では engine 注入に載る。
- nudge 回数・最終時刻の台帳（`ledger`）は **in-memory**、channel C と channel E で **共有**（同一 `RENUDGE_AFTER` / `MAX_NUDGES`。channel D だけ別台帳 `bg_ledger`）。daemon 再起動でリセットされ上限回が再付与されるが、**ack されれば pending から消えて止まる**（ack 台帳が真値、nudge 台帳は運用状態）。
- 別チャネル（**channel B**）として、`vp wire hook-check`（claude hook 実体、`commands/wire.rs:136`）が SessionStart 等で未読 wire を `additionalContext` として stdout に出し、会話開始時に未読を気付かせる（fail-open = 失敗は silent 成功で会話を邪魔しない）。gui（chat lane）でも headless の SessionStart hook は走るため有効（doc 34 §2-5）。

---

## 2. dev-flow FSM（`flow_state`）

> tool としての `flow_handoff` / `flow_progress` の signature・CLI 例・emoji ラベル表・cascade の全 test は [`dev-flow-primitives.md`](./dev-flow-primitives.md) §3 が詳しい。ここでは **messaging 視点**（wire → state → sidebar への投影）に絞る。

### 2.1 FlowState 6 variant と derive 規則

各 performer の `flow_state` は **store されない**。3 つの input から毎回 derive される pure function `derive_flow_state`（`flow.rs:185`）の出力:

1. 最新 wire activity（`latest_msg_for_agent` の direction + `body.kind`）
2. performer_status（`dirty_count` / `last_commit` → `dirty` / `has_commit`）
3. 未 ack の `needs_user` wire（`pending_needs_user`、ack 台帳ベースの述語）

| FlowState | 契機 |
|---|---|
| `Idle` | wire activity 一切なし（新規 performer） |
| `Working` | conductor が task 送出 / performer が ack・decision で自走中（control surrender 中） |
| `HitlPending` | performer が `question` を投げ conductor reply 待ち |
| `AwaitingUser` | performer が `needs_user` を投げ **ユーザ本人**の回答待ち（未 ack） |
| `Completed` | performer が `complete` 報告済 |
| `Stuck` | conductor 指示後 dirty 残り commit 無し |

cascade（`flow.rs:185` の実装そのまま）:

```text
if pending_needs_user => AwaitingUser   // 未 ack needs_user は cascade より優先（ack 台帳が SSOT）
match (latest_msg, dirty, has_commit) {
    (None, _, _)                                            => Idle,
    Some(m) if m.from==conductor && kind=="task"            => Working,
    Some(m) if m.from==performer && kind=="question"        => HitlPending,
    Some(m) if m.from==performer && kind=="complete"        => Completed,
    Some(m) if m.from==conductor && dirty && !has_commit    => Stuck,
    Some(m) if m.from==performer && kind∈{ack,decision,request}      => Working,
    Some(m) if m.from==conductor && kind∈{approve,modify,clarify}    => Working,
    _                                                       => Working,   // fallback
}
```

**`AwaitingUser` が cascade より優先される**のがこの世代（2026-07-11）の要。needs_user 送信後に performer が別 wire（ack / decision）を送って latest が変わっても、**未 ack の needs_user が残る限り AwaitingUser のまま**。ユーザ待ちである事実は会話の続きでは消えず、conductor が **ユーザの回答を relay してから `wire_ack` した瞬間に**解消される（ack 台帳が SSOT）。使い分けは `AGENTS.md` の wire 規約に従い、`needs_user` を乱発しない（needs-you signal の希少性を守る）。

`control_surrender`（conductor が control を手放して performer 自走中か）は `state ∈ {Working, Completed} && (last_msg.from == performer || last_msg is None)` で `true`。

### 2.2 `LaneInfo.flow_state` 投影経路（repo → daemon → vp-app）

```
performer の wire 活動
  → WiremsgStore（daemon in-process）
  → [repo]   build_lanes_snapshot（flow_state = None のまま）
             discovery "registry" channel: register / lanes/add|remove|update（heartbeat 15s）
  → [daemon] lane_registry: HashMap<path_key, Vec<LaneInfo>>   ← ここまで flow_state = None
             send_lanes_snapshot → enrich_lanes_flow_state → derive_flow_state   ← ここで付与
             "lanes" channel で send_event("snapshot", LanesSnapshot)
  → [vp-app] "lanes" channel を open（daemon :32000 に接続、repo 直結ではない）
             laneConnector(flow_state) → sidebar connector 描画
```

投影の実装事実:

- **`LaneInfo.flow_state`（`lanes_state.rs:324`）は `Option<FlowState>`**。repo / lane_registry / db では**常に `None`**（「derive できるものは store しない」原則）。付与するのは daemon だけ。
- **付与点 = `enrich_lanes_flow_state`（`daemon/server.rs:557`）**。`send_lanes_snapshot`（`:515`）が snapshot を送る直前に呼ぶ。Performer のみ対象（conductor は None のまま）、`agent<repo>/<name>` を組み、`latest_msg_for_agent` + `pending_needs_user` を **hop なしの in-process store から**引いて `derive_flow_state` する。`vp flow progress` と同一判定。store 未接続時は enrich を skip（field 欠落）。
- **"lanes" channel は unison/QUIC channel**（`register_channel("lanes")`、`daemon/server.rs:1218`）。WebSocket でも SSE でもない。vp-app は **repo ではなく daemon :32000 の集約 channel** に繋ぐ（`vp-app/src/app.rs` の lanes subscription、stall timeout 12s）。
- **再 push は wire 活動が撃つ**（polling 無し）: `wire/send` と `wire/ack` の dispatch 前に関与 repo を集め（`collect_wire_projects`、`daemon/server.rs:618`）、dispatch 成功後に `notify_lane_change_for_projects` が `lane_change_tx`（broadcast）へ path_key を送る（`:644`）。"lanes" channel handler がこれを subscribe していて、当該 repo の snapshot を**再 enrich して再送**する。つまり wire を送る/ack するだけで flow_state の変化が sidebar に届く。
- **sidebar 描画**（`vp-app/webview/src/sidebar/lane.ts` `laneConnector`）: `awaiting_user` → `conn-hitl`（needs-you = magenta diamond）、`working|hitl_pending|stuck` → `conn-auto`（solid cyan）、`idle|completed` → `conn-dead`。**`flow_state` 欠落（旧 daemon）は pid heuristic に fallback**。FlowState の serde は snake_case（`flow.rs:43`、TS 側との契約）。

### 2.3 OSC `awaiting_input` 軸との関係（別軸併存）

`flow_state` の `AwaitingUser` とは**別軸**に、OSC 由来の `awaiting_input` がある。両者は独立に算出され、**vp-app の描画関数で OR 結合されて同じ needs-you 表示に畳まれている**。

| 軸 | 源 | 実体 |
|---|---|---|
| `flow_state: AwaitingUser` | **server**（wire 台帳） | 未 ack `needs_user` wire。§2.1 の derive |
| `awaiting_input` | **vp-app client** | OSC 99/9/777 notification + gui の turn_completed。`SidebarState.awaiting_input`（`vp-app/src/pane.rs:156`、`lane:select` で reset） |

- `laneConnector`（`lane.ts`）は `flow_state === "awaiting_user"` **または** `awaitingInput`（OSC 由来）のどちらかで `conn-hitl`（needs-you / magenta diamond）を返す。OSC 軸は「active console がユーザを待っている」signal で、`AskUserQuestion` 等 console 側の HITL を拾える唯一の経路として意図的に needs-you に残されている。
- **⚠ 未解決 issue（視覚区別なし）**: 両軸は UI 上で**区別されず同一の `conn-hitl` に統合**されている。「server 由来の needs_user 待ち」と「OSC 由来の console 待ち」を sidebar で見分ける手段は現状の実装に無い（意図的な統合だが、源の区別は未実装）。

---

## 3. federation（cross-daemon / cross-PC）

別マシン（別 daemon）の agent へ wire を届ける仕組み。address 文法（`actor@machine/repo/lane`）は [`wire-address-usage.md`](./wire-address-usage.md) を、設計背景は同 spec を参照。ここでは **現状動く挙動**を実装から記す。

> 📝 **doc 状態の注意**: `spec/wire-address-v3.md` / `wire-address-usage.md` は federation を「Phase 3+ の将来計画」と記述しているが、**実装は cross-PC round-trip まで到達済**（v0.42 世代で実弾確認）。本 §3 が現状の正。spec 側の「将来計画」表記は陳腐化として別途 wire 報告済（§5）。

### 3.1 daemon identity と到達性

| 概念 | 実体 |
|---|---|
| `wld_id` | home-daemon の位置独立な安定 id（`daemon/node_id.rs`、`wld_` + base58(UUIDv7)、opaque な routing key、home-daemon に 1 個、db 永続）。direct の SNI にも使う |
| handle | daemon の表示名。**OS hostname から決まる**（`resolve_handle(None)`、`hub_client.rs:220`。override → hostname → fallback `"vp-daemon"`） |
| endpoints | direct 到達候補。**IPv6 GUA（`2000::/3`）のみ**を advertise（`daemon/endpoint.rs`、link-local / ULA / loopback は除外）。connect-trick で OS が選んだ source GUA を読む。**tailnet 非依存**。空なら relay floor に委ねる |
| hub opt-in | `CHRONISTA_HUB_ADDR`（env）> config.kdl `hub-addr`（`hub_client.rs:156`）。**LaunchAgent（launchd）daemon は shell env を持たない**ため、常時 ON にするには config.kdl 側に書く |

hub と話すのは **daemon のみ**。CLI / repo は daemon の wire channel 経由で federation を叩く（SSOT）。

### 3.2 register（自 daemon の登録）

- 常駐タスク `run_hub_federation`（`hub_client.rs:754`）が hub に `{wld_id, endpoints, handle, name}` を Register し、`Disconnected` 検知で backoff 5s 再接続する。
- **credentials は `vp auth login`** が保存する access_token（Creo ID の user-jwt）。federation 接続時に提示する（未ログインなら credential なしで graceful degrade、hub は現状 observe mode）。
- **⚠ 罠（daemon bounce）**: credential は **接続時に一度だけ**読まれ、**live reload の機構が無い**。接続維持中の daemon に新しい `vp auth login` は反映されない。反映させるには **daemon を再起動**（= fresh connect で credentials 再読込）する必要がある。LaunchAgent 常駐下では特に、login 後に daemon bounce を忘れると古い（or 無い）credential のまま。

### 3.3 discover（相手 daemon の lane 列挙）

```bash
vp wire discover --daemon <handle>     # 例: vp wire discover --daemon taro-box
```

- 宛先を知らないとき、相手 daemon の lane 一覧を relay 上の request-response で列挙する（在庫確認）。
- 返るのは lane subset の配列 `{address, kind, name, state}` のみ。**allow-list で絞られ、cwd / pid / git 状態は露出しない**（`process/server.rs` の relay 応答）。
- 一時的な返信受け皿として `wld_disco-*`（§3.6）を使い、10s timeout。

### 3.4 send（cross-daemon 送信）

```bash
vp wire send --daemon <handle> --to <logical addr> --body "..."
# 例: vp wire send --daemon taro-box --to agent@nostos/main --body "hi"
```

- `--daemon` を付けると path が `/api/wire/federate` になり、daemon が hub relay 経由で遠方 daemon へ送る。**`--to` は「その daemon 内部の logical address」**として解釈される（自 daemon の address ではない）。

### 3.5 配送 = direct（HEv2）→ relay floor の 2 段

`federate_wire_send`（`hub_client.rs:631`）が handle → entry（wld_id + endpoints）を解決し、2 段で配送する:

1. **Stage 1: direct**（`dialer.rs`）。`connect_race`（Happy Eyeballs v2 staggered race）で endpoints を並行に叩く。パラメータ: `stagger` 250ms / `relay_handicap` 500ms / `overall_deadline` 1500ms（`federation_race_cfg`、全候補が blackhole でも relay への追加遅延を 1.5s で有界化）。SNI は wld_id。成功なら `via=direct`。
2. **Stage 2: relay floor**（`hub_client.rs:656`）。direct 全滅は**エラーではなく ladder の一段**として扱い、hub relay に降格する。hub は source→target を **opaque に dumb forward**（中身を見ない）する「常に生きている最下段（universal floor）」。target 側は `run_hub_federation` 常駐が relay inbound を受け、ローカル中央 wire store に inject する（遠方 relay を「ローカル送信」に畳む）。`via=relay`。

- **現状の実挙動**: remote の daemon は dev cert のため、非 loopback 宛は System trust で **fast-fail → relay floor に落ちる**（= 正しい degradation）。mesh cert（内部 keypair）が入ると direct が勝ち始める、と実装コメントに明記（`dialer.rs` module doc）。
- kill-switch `VP_FEDERATION_DIRECT=0|false|off` で direct を無効化できる（relay floor は常に生きているので degrade するだけ）。

### 3.6 返信の ephemeral 番地 `wld_disco-*`

discovery は片方向 relay の上に request-response を作る。source を返信可能にするため、`federate_discover_lanes`（`hub_client.rs:673`）が **一時 wld_id `wld_disco-<uuid>`** を生成し、永続 registration を clobber しないよう**別接続で一時 register**する。`lanes-query` の `reply_to` に載せて target が `lanes-reply` をこの番地へ返し、**関数 scope 終了で connection が drop → hub から一時 register が除去**される。`vp wire discover` 実行のたびに 1 個新規生成。

### 3.7 ⚠ 罠: `--daemon` 省略で「同名 repo のローカル宛」に silent 成功

**最重要の落とし穴。** `--daemon` は `Option` で、`None` のとき path が `/api/wire/federate` ではなく `/api/wire/send`（ローカル中央 store）に落ちる（`commands/wire.rs:704`）:

```rust
let path = if let Some(remote) = daemon {
    payload["world"] = serde_json::Value::String(remote.to_string());
    "/api/wire/federate"
} else {
    "/api/wire/send"          // ← --daemon 省略はここ。エラーにならない
};
```

- 遠方 daemon へ送るつもりで `--daemon` を書き忘れると、**エラーにならず**、`--to` が**ローカルの同名 repo 宛**として解釈されて **silent に成功**する。宛先 daemon の実在検証は send 経路に無い。
- 対策候補（未実装、issue 扱い）: origin daemon を継承する federation の `--reply-to` 相当があれば、返信で daemon を書き忘れる事故を防げる。

---

## 4. 入口一覧表（MCP ⇄ CLI ⇄ 内部 channel method）

> ⚠ **「HTTP」列は公開 HTTP API ではない**。かつての `/api/wire/*` axum route は撤去済（`routes/health.rs:49`）。今この文字列は `world_wire::call` に渡す**論理 path** で、`/api/` を剥いだ残り（`wire/send` 等）が **unison QUIC "wire" channel の method** になる。3 者はすべて同じ下層 `WiremsgStore`（daemon）に収束する。

### wire family

| 操作 | MCP tool | CLI | channel method | 備考 |
|---|---|---|---|---|
| 送信 | `wire_send` | `vp wire send` | `wire/send` | root（`reply_to` 無）/ reply（有） |
| 受信 | `wire_recv` | `vp wire recv` | `wire/recv` | long-poll、cursor 前進。CLI default timeout 5s（`watch` は 25s の継続版） |
| 未読在庫 | `wire_inbox` | `vp wire inbox` | `wire/unread-count` | read-only（cursor 不触り） |
| ack | `wire_ack` | `vp wire ack` | `wire/ack` | ack 台帳、冪等 |
| 系譜 | `wire_thread` | `vp wire thread` | `wire/thread` | read-only、root-first |
| 継続 watch | —（`wire_recv` の loop 相当） | `vp wire watch` / `watch-supervised` | `wire/recv` loop | Monitor の subscription source |
| hook | —（hook 実体） | `vp wire hook-check` | `wire/unread-count` + `delegation/poll` | claude hook、fail-open |
| GUI 履歴 / ack | — | —（vp-app sidebar Wire Inbox panel） | `wire/history` + `wire/unread-count`（fetch）→ `wire/ack` | **#742 / doc 34 §4 V1**。read-only 履歴（`inbound` / `acked` flag 付き）。**cursor 不触り**（`wire/recv` を使わず lane claude の未読を横取りしない）。ack は `wire/ack` を再利用し「ack → 再 fetch」を 1 往復に畳む。LaneRow の mailbox badge click → daemon "wire" channel を直接 open（`vp-app/src/app.rs` の `wire_fetch_payload`） |
| federation 送信 | — | `vp wire send --daemon` | `wire/federate` | §3.4（daemon 層が hub relay へ） |
| federation 探索 | — | `vp wire discover --daemon` | `wire/discover-lanes` | §3.3 |

FSM derive を支える read-only method（`flow_progress` / enrich が使う。直接叩く CLI/MCP は無い）: `wire/latest-msg`、`wire/needs-user-pending`。

### flow / delegation family

| 操作 | MCP tool | CLI | 備考 |
|---|---|---|---|
| handoff | `flow_handoff` | `vp flow handoff` | performer 作成 + 初手 wire_send + nudge を atomic に（[dev-flow-primitives.md](./dev-flow-primitives.md)） |
| progress | `flow_progress` | `vp flow progress` | 全 lane の status + FSM を 1 view（read-only） |
| 委譲 | `delegate` / `respond` / `complete` | —（観測系のみ: `deleg-thread`） | delegation record 系統（wire とは別、[design/28](../design/28-agent-delegation.md)） |

### MCP と CLI の差（重要）

- **MCP は repo "process" channel を 1 段挟む**: `SelfLane`（conductor = `agent<repo>`、performer = `agent@<parent>/<name>`）から `from` / `agent` を注入し、`normalize_agent_addr` で防御する。repo 未解決の conductor は fail-closed。
- **CLI は daemon 直結**（`world_wire::call`）で、address は qualified 前提（`from` の default は `"vp-cli"`）。
- **category default の非対称**（§1.4）: MCP `wire_send` は `command` を注入、CLI `vp wire send` は注入しない。
- 共通下層: writer = `WiremsgStore`（daemon、local_seq を AtomicU64 採番）、transport 集約点 = `world_wire::call`、dispatch 分岐 = `handle_wire_channel`（`wire/` vs `delegation/`）。

---

## 5. 既存 doc との関係・発見した drift

本 doc は messaging の見取り図として新設した。既存 doc へは重複記述せず、上記の相互リンクで繋ぐ。sweep で以下の drift を発見した:

1. **`dev-flow-primitives.md` の `5-state` 表記残存**（status 行 / §2 見出し / §2 コメント）。`AwaitingUser` 追加で **6-state** が正。→ 記述の事実誤りとして最小修正（本 lane で対応）。
2. **`spec/wire-address-v3.md` + `wire-address-usage.md` が federation を「Phase 3+ 将来計画」と記述**。実際は cross-PC round-trip まで実装済（v0.42 世代で実弾確認）。→ 大きい矛盾のため spec 全書き換えはせず、本 doc §3 を現状の正とし、spec 側にリンク注記を足す + conductor へ wire 報告する方針。

---

_この doc に古い記述を見つけたら、コード（上表の一次資料）を正として直してください。_
