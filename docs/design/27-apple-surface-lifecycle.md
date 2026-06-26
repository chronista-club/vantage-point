# 27 — Apple Surface Lifecycle ＋ L0 単一 endpoint 契約（SP-portless）

> Status: **Draft / 設計契約**（2026-06-26）。bottom-up rebuild の L0 spec。
> 親: doc 24（vp-spine）, doc 25（Apple Platform Architecture）, doc 26（Swift Unison Client）。
> 設計 SSOT memory: SP-portless plan + L0 comm map `mem_1CcRLbsPMKXsBkt1Pk77fB` /
> agent-native substrate `mem_1CcRMhGKFX3WCp6NJPUJ39` / 北極星「単一 topic 空間」`mem_1Cb7iV6ZBczuqiBbiYQpvm`。
> この doc は **lifecycle matrix（誰がいつ生き死にするか）** と **L0 単一 endpoint 契約（全通信が
> World :32000 に集まる）** を 1 枚に畳む。両者は「surface = World の client」という Model D（doc 25）の
> 表裏なので分けない。

## 0. 一行で

**全 surface（daemon / SP / vp-app / macOS agent / visionOS / agent=Claude）は World :32000 の単一
Unison endpoint に繋ぐ pure client。SP は listen port を持たず outbound-only に退く。新しい通信を
ad-hoc な port / HTTP endpoint / WebSocket で足さない ── 足すなら World の topic/channel に足す。**

## 1. 背景・問題

doc 25 で **World = Mac daemon が唯一の権威・各 surface は Unison/QUIC の native client**（Model D）を
決めた。だが「各 surface がいつ生まれ・いつ死に・どう presence を持つか」の **lifecycle 契約**と、
「client がどの endpoint に繋ぐか」の **transport 契約**が未文書だった。これが 2 つの具体問題を生む。

1. **remote surface は per-SP port に届かない**。Vision Pro / 他デバイス / cloud agent は World の
   1 endpoint（`<host>:32000`）にしか到達できない。SP が `:33000-33011` を 12 個 listen し vp-app が
   そこへ直結する現状は、**LAN/remote に出た瞬間に破綻する**。→ SP-portless は「v1.1 の簡素化」ではなく
   **Apple platform / remote 到達性の前提条件**に格上げ（`mem_1CcRLbsPMKXsBkt1Pk77fB`）。
2. **通信が 6 種に割れていた**（北極星 `mem_1Cb7iV6ZBczuqiBbiYQpvm`）。ad-hoc に endpoint を足す
   たびに state 二重化と型追従漏れ（PR #378 クラス）が増える。単一 topic 空間に寄せる規律が要る。

> 方針（user 指示・ultrathink、`mem_1CcRLbsPMKXsBkt1Pk77fB`）: 「一時的に VP 利用不能 OK、下のレイヤー
> からゴリっと」。shippable 維持を捨て **foundation-first の bottom-up rebuild**。claude on kitty を
> decoupled cockpit に、`mr dev` で VP を使い捨て test target に。breakage 許容。

## 2. Surface lifecycle matrix

VP の surface と、その生死・presence・OS 統合・到達性を 1 表に固定する。**presence は daemon-canonical**
（doc 24 vp-spine / Model Q「presence in DB」）── 「誰が生きているか」の権威は常に World が持ち、
surface は自分の生死を World に**報告**するだけで、port scan で**当てに行かない**。

| surface | プロセス / 言語 | 起動・所有 | lifetime | World 接続 | OS 統合 | teardown / heal | 現在地 → 目標 |
|---|---|---|---|---|---|---|---|
| **TheWorld daemon** 👑 | Rust（tokio + club-unison server） | home base・常駐 | always-on | — （自身が endpoint :32000） | なし（headless） | crash → 再起動で自律復帰 | `vp daemon start` → **LaunchAgent always-on**（L1, SMAppService） |
| **SP（Star Platinum）** ⭐ | Rust（per-project core） | World が spawn + reconcile | project を開いている間 | **outbound-only client**（registry QUIC で自己登録） | なし | World への接続断 = 即 presence 除去（Push）。Pull port scan は**廃止**へ | 現状 `:33000+` を listen（HTTP/WS + QUIC）→ **listen port 全撤去**（L1） |
| **vp-app** | Rust（wry/tao webview） | `vp app start`（window ごと別 OS プロセス） | window が開いている間 | **pure client**（World :32000 へ subscribe） | window / Dock（mac） | window close で exit、auto-restore で再 spawn | lanes/canvas/control = World 経由化済 → terminal 移行後に **M3 で退役**（統合 Swift Mac App へ） |
| **macOS menu bar agent**（VantagePointAgent） | **Swift**（LSUIElement, login item） | login item / `mr agent` | 常駐（menu bar） | native Swift Unison client（doc 26） | **CoreMIDI hot-plug / menu / 通知 / Shortcuts**（main run loop 保有） | daemon 不在時 degrade → auto-reconnect（caller 責務、doc 26 §5） | M1/M2 出荷済（#587-590）→ OS 統合の正規の手 |
| **visionOS app** | **Swift**（SwiftUI / RealityKit） | App 起動 | foreground | native Swift Unison client（network 越し） | 空間入力 / hand・eye / RealityKit（local） | 接続断 = 再接続 | 未着手（M3 後、L3） |
| **agent（Claude）** 🤖 | 外部プロセス（MCP / CLI / cloud） | session 起動 | session | **first-class surface**（§5）。MCP = topic への thin bridge | なし | — | bolted-on MCP → World topic の peer subscriber/publisher へ |

**lifecycle の不変条件**:

- **presence は World が canonical**。surface は「生きてます」を World に push する（SP=registry QUIC /
  agent=device channel / vp-app=接続そのもの）。World が `running_processes` を正規化パスで保持する
  （CLAUDE.md「Reconciliation」）。**port scan による生死判定（Pull）は L1 で廃止**し、Push 一本にする。
- **daemon だけが always-on**。他 surface は「来ては去る」client であり、落ちても World の state は残る
  （durable lane_registry → 次回 vp-app 起動で「前回の続き」が出る。#591 で実証済）。
- **言語境界 = wire 境界**（doc 25 §3）。Swift surface は Rust を一切リンクしない（FFI 無し）。

## 3. L0 単一 endpoint 契約（SP-portless）

### 3.1 契約

> **全 surface 間通信は World :32000（QUIC, 単一 endpoint）の Unison channel / topic を経由する。
> surface は World 以外に繋がない。SP は listen を持たず、World への outbound 接続だけで存在する。**

remote（Vision Pro / 他デバイス / cloud agent）から見て「到達可能な場所」が World :32000 ただ 1 つに
なる（doc 25 §7 の transport/discovery/auth はこの 1 endpoint に集約）。

### 3.2 comm map — 現在地（#591/#592 後、コード検証済 2026-06-26）

**World :32000 channels**（`crates/vantage-point/src/daemon/server.rs` の `register_channel`）:

| channel | 役割 | 出自 |
|---|---|---|
| `session` / `terminal` / `system` | daemon PtySlot・system（vp hd / Console attach 旧経路） | 既存 |
| `world-process` | process lifecycle event broadcast | 既存 |
| `world-control` | CLI projects 操作の権威 | 既存 |
| `world-device` / `device` | Bastet 🧲 device event（daemon→client）/ device 報告（agent→daemon） | #588/#590 |
| `registry` | SP の自己登録（Push presence） | 既存 |
| **`lanes`** | per-project lane snapshot を vp-app に集約配信（SP 直結の World 集約版） | **#591** |
| **`canvas`** | Paisley Park topic を per-project TopicRouter に route して vp-app に配信 | **#591** |
| **`process-proxy`** | 外部 client → World → SP の **reverse-routing**（`project_path`→`normalize_path_key`→SP `control` channel 逆引き） | **#591/#592** |

**SP 側に残る listen（= まだ portless でない holdout）**:

| listen | 内容 | 状態 |
|---|---|---|
| `:33000` WS `/ws/terminal`（`process/server.rs:363`） | **vp-app Lane terminal の raw PTY I/O** | ⏳ **最後の holdout**（§4） |
| `:33000` WS `/ws/lanes`（project_feed） / HTTP `/api/lanes,stands,show,tmux/*,ruby/*,wire/*,pp/state,pane-ops` | SP HTTP API surface | ⏳ World 経由化 or fold 待ち |
| `:33000+` QUIC `process` / `terminal` / `control` channel（`process/unison_server.rs`） | `process`=旧直結 / `terminal`=PTY / `control`=reverse-routing の SP 側受け口 | `control` は #591 で稼働、`process` 直結は process-proxy へ移行中 |

### 3.3 移行台帳（L0 → portless）

- ✅ **vp-app lanes 購読** → World `lanes` channel（#591、`app.rs:272,359,2063`）
- ✅ **vp-app canvas 購読** → World `canvas` channel（#591、`app.rs:428,507,2071`）
- ✅ **MCP control（process method）** → World `process-proxy` channel（#592、`mcp.rs`。stale-port self-heal `rediscover_process_port` を QUIC 経路から撤去）
- ⏳ **terminal** → World 経由の Unison channel（§4、Phase B 終盤・要 perf spike）
- ⏳ **SP HTTP API residual**（`/api/lanes,stands,tmux,ruby,show` + `/ws/lanes`）→ World channel へ吸収 or CLI を process-proxy に寄せる
- ⏳ **SP listen port 全撤去** + presence via World（Reconciliation Pull port scan 廃止）── terminal 片付け後（L1）

## 3.4 Transport 哲学 — なぜ QUIC / 場 × 動詞 × 規律 / 1 conn・N streams・1 protocol

> 起源と概念の合意形成（user × Claude dialogue 2026-06-26、ultrathink）。北極星「単一 topic 空間」と
> SP-portless（§3）の **なぜ** を固定する。code から導けない設計 rationale で、放置すると再導出される
> （user が QUIC 採用理由を二度「思い出した」）ので明文化して錨にする。

### 3.4.1 なぜ QUIC — 原点は live streaming の QoS 隔離

QUIC 採用の原点は「**ライブ配信で position / 音声 / 3D・大アセットを、それぞれ別 stream で混ぜずに流す**」
要求（HTTP/TCP では不可能）。HOL-blocking 回避は *手段* で、本質は **QoS の違うデータを 1 本の順序付き
byte パイプに同居させない**こと。

| データ | 配送規律（QoS） | 同居できない理由 |
|---|---|---|
| **position** | 最新勝ち・lossy・coalesce・超低遅延 | 古い座標は無価値。bulk の後ろで待たせたら死ぬ |
| **音声** | 順序・低遅延・frame 単位 drop 可 | asset に HOL されると音が割れる |
| **3D / 大アセット** | 確実・順序・bulk・遅延寛容 | でかい。共有 stream に乗せたら全部 HOL する |

stream = 1 本の順序付き byte パイプなので、「古い position を捨てる」と「asset を確実に届ける」を同時に
満たせない＝ **QoS を分けるには物理的に stream を分けるしかない**。QUIC は「1 connection に独立 stream を
多重化（stream 間 HOL 無し）」を native で持つ唯一の現実解。**VP の transport は terminal IDE ではなく、
QoS 差別化された live streaming substrate を見て選ばれた**（physical control fleet「lane を楽器にする」の
position データ = `position/*` 場）。

### 3.4.2 偽の二分法 — pub/sub vs RPC は 2 直交軸を畳んだもの

「pub/sub か RPC か」は 1 軸に見えて、**直交する 2 軸**を畳んでいるだけ:

- **軸A アドレッシング**: topic（一対多）⇔ direct（一対一）
- **軸B インタラクション**: tell（告げる・fire&forget・*送ること*が目的）⇔ ask（問う・応答待ち・*結果*が目的）

`pub/sub = (topic × tell)` / `RPC = (direct × ask)` は 4 セルのうち 2 つにすぎない。残りも有用:
- **(topic × ask)** = 場の authority に問う＝ **topic-routed RPC** ← S5 が要る cell
- **(direct × tell)** = 特定 peer への通知

→ 「単一 topic 空間」が意味すべきは **アドレッシング軸を topic に統一する**ことで、**インタラクション軸を
tell に潰すことではない**。tell/ask を正直な動詞のまま topic で routing すれば request/reply を失わず単一
空間が成立する。**Unison channel は `Event`(=tell) と `Request`/`Response`(=ask) を 1 stream で native に
運ぶ → 本質的に正しい substrate**。欠けている cell は **topic-addressed ask**（Request を場の authority へ
route し Response を correlation で返す。Unison の pending-map 機構で実装可、correlation hack 不要）。

### 3.4.3 場（place）モデル — 3 軸

**場 = 名前のついた番地（topic も wire-address も同じ namespace）。各場は ちょうど 1 authority（主・真実の
源・ask に答える）と 0+ observer（tell を受ける）を持つ。**

- **3 軸**: **場（どこ）× 動詞（tell / observe / ask）× 規律（QoS）**。場の prefix が規律を示し、stream が
  それを物理実現し、動詞は直交。
- authority 例: `process/*` = SP / `agent@X` = その session / `bastet@world` = device hub。
- **demand-driven production（S2 実装済）はこのモデルから自然に落ちる**: authority は observer 数を知るので、
  observer が居る間だけ tell を produce する。ask は point-to-point なので demand 不要。
- **command と event は同じ場の双対**（event-sourcing）: 場を ask して intent を入れ、その帰結が同じ場の
  tell として流れ出す。**agent は ask で為し observe で知る — 同じ番地で**（§5 agent-native の具体形）。

### 3.4.4 Topology 規律 — 1 connection / N streams-by-QoS / 1 protocol

| 軸 | 現状（負債） | 目標 |
|---|---|---|
| **connection** | session ごとに別 QUIC connection（`run_canvas_session`/`run_terminal_session` が毎回 connect）＝ **N conn × 各 1 stream**。QUIC の多重化を**使えていない** | **1 connection**（World へ共有） |
| **stream(channel)** | 用途ごと bespoke（§3.2 の channel 群）| **隔離単位で N 本、protocol は同一** |
| **protocol** | bespoke 多数（別 handshake・別 message 形）| **1 種**（場 subscribe + tell/observe/ask） |

- **stream = 意味の単位ではなく *隔離の単位***。独立した flow/順序/backpressure（= QoS）が要る時だけ開き、
  要らなければ相乗りさせる。「channel いくつ?」は **semantic 判断ではなく QoS/隔離から決まる perf つまみ**
  ← 概念が正しい徴（dilemma が消える）。
- 粒度: 高頻度・低遅延・大量の場（terminal 出力 per-lane / position / 音声 / asset）→ 専用 stream。
  低頻度 observe（presence / lanes list / device）→ 共有可。demand と連動し observe 中の場だけ stream を張る。
- **「1 channel に全部寄せる」は不可**: QUIC を選んだ理由（QoS 隔離）を捨て、stream 内 HOL を復活させ、mux を
  手実装で再発明する。美しく見えて弱い・冗長。

### 3.4.5 含意

- **terminal（§4, S1-S4）= "text" QoS クラスの first citizen**。topic 空間 / demand / coalesce は
  position・音声・asset にそのまま一般化する原型を作った。
- **wire-address（`agent@X` 等）と topic は同じ場 namespace**。messaging と transport の統合は将来候補。
  **agent 協働（委譲 = ask / broadcast = tell）は `(topic × ask)` / `(topic × tell)` として doc 28 に具体化**
  （dogfood #1: A→B 委譲 + 完了で block 解除 → 再開。場の番地 = 仕事 / 議題、`(direct × ask)` は infra RPC に
  残す。location-transparent federation で cross-machine / cross-World へ）。
- **S5 の再定義**: 「control を pub/sub(tell) に潰す」ではなく **「(topic × ask) を足し、command を ask 規律
  クラスとして場 namespace に載せる」**。reverse-route RPC は「`process/*` の authority(SP) への ask」になる
  （意味不変・指し方が topic に統一）。前提 = **「1 場 = 1 authority」不変条件**（ask routing が決定的になる）。
- **実装順序**: ① L1 portless（単一 endpoint）→ ② **connection 共有**（N conn → 1 conn × N streams、概念
  不変の perf 勝ち筋・QUIC multiplex の回収）→ ③ **protocol 統一**（= S5、topic-addressed ask）。

## 4. terminal = Unison stream channel（最後の holdout）

terminal は単一 topic 空間の**唯一の raw WebSocket holdout**。vp-app の Lane terminal が
`ws://127.0.0.1:<sp_port>/ws/terminal`（`main_area.rs:893,922`）で SP に直結している。

**方針**（`mem_1CcRLbsPMKXsBkt1Pk77fB` 更新分 / handoff `mem_1CcRMmtaBSzha8DC4FFW3J`）:

- terminal を **Unison stream channel** にし、WebView は **postMessage + unison-client TS** で受ける
  （北極星 柱4 / transport 統一）。→ lanes/canvas と同じ multiplexed channel として World を経由できる。
- これにより **専用 World WS bridge は不要・廃案**（Explore が critical path とした 10-14 日案を回避）。
- terminal は他 slice より**後**に回す。raw WS のまま残してもよい中間状態を許容し、transport 統一
  （M3）と一緒に Unison channel 化して最後に剥がす。
- 残実証 = **PTY I/O が Unison-channel-over-postMessage に耐えるか perf spike**（keystroke latency +
  大量出力 throughput、出力 coalescing 前提。postMessage は in-process IPC なので行けるはずだが計測で裏取り）。

> 注（PTY は 2 系統、`mem_1CcRLbsPMKXsBkt1Pk77fB`）: daemon PtySlot（`session`/`terminal` channel = Unison）と
> SP Lane PtySlot（`/ws/terminal` = WebSocket）は別物。**vp-app が見せる terminal は後者**で、daemon を
> 経由せず Unison でもない。portless 化の対象はこの SP Lane PtySlot 経路。

### 4.1 実装設計 — terminal を「topic 空間の住人」にする（bespoke 機構を増やさない）

> perf spike 完了・🟢 GREEN（`mem_1CcRhY8odVzGygQo8gmehi`）: wry 境界 615 MB/s / xterm 90 MB/s / WS 2GB/s、
> いずれも PTY 需要を桁違いに超で WS→Unison に regression なし。実装は「最短で動かす」より
> **強く美しい構造**（user directive 2026-06-26）を優先する。

**統一視点**: lanes（push snapshot）/ canvas（push retained）/ control（reverse request）/ terminal は
**同じ topic 空間の pub/sub の異なる断面**で、違うのは 3 プロパティ（payload / 方向 / production）だけ。
terminal を 4 つ目の bespoke 機構として足すと構造は**発散**する。terminal を特別扱いせず topic にすれば
**収束**する（北極星）。terminal は 3 つの「難しい角」を全部踏む唯一のケース（raw bytes / 双方向 /
on-demand）なので、正しく作る = topic 空間に汎用能力を与える = substrate が成熟する。

**topic 名前空間**（既存 `process/{capability}/{category}/{detail}` + Paisley Park の lane segment 方式を踏襲）:
```
process/terminal/{lane}/data/out      ← PTY 出力 (per-lane, ephemeral)
process/terminal/{lane}/data/in       ← keystroke
process/terminal/{lane}/state/resize  ← {cols, rows}
```
SP = topic authority（PtySlot 所有）/ World = broker（per-project `TopicRouter` = `canvas_routers` と同型）/
vp-app・WebView・将来 agent = 対称な subscriber/publisher。

**substrate に足す能力（terminal が forcing function、接地済み 2026-06-26）**:
1. **payload = `ProcessMessage` 流用**（新 raw-frame primitive を作らない）。`TopicRouter` は既に
   `ProcessMessage` 型 pub/sub で `TerminalOutput{data:base64}` topic を持つ。これを **per-lane 化**
   （`lane` field 追加 → lane segment topic）して既存 router にそのまま乗せる = canvas/lanes と同一機構。
   base64 は spike で perf 実証済。真の opaque raw-frame topic は将来の substrate 純度向上として分離。
2. **demand-driven production（本命の新 primitive）**。`TopicRouter` の subscribe/unsubscribe に demand
   hook を足し、subscriber `0→1`（start）/`1→0`（stop）を authority(SP) に通知。SP は per-lane PtySlot→topic
   pump を demand で gate。= 「**subscriber が居る間だけ producer が回る**」汎用 lazy topic。on-demand が
   ここから自然に出る（bespoke start/stop 信号を作らない）。
3. **bidirectional**（input/resize）。subscriber→authority publish を World が route。control の
   reverse-route はこの特殊例として将来吸収（今は収束先として残す）。

**段階**（各 step 単体 test 可、美しさを壊さず積む）:
- **S1**: `TerminalOutput` per-lane 化 + SP per-lane pump → World `TopicRouter` → throwaway probe で SP→World→受信実証（まず always-on）。
- **S2**: demand hook（subscriber 0→1/1→0 を SP に通知）→ pump を lazy 化。
- **S3**: input/resize を topic publish（vp-app→World→SP `write_to_lane`/`resize_lane`）。
- **S4**: WebView = unison-client TS（postMessage transport 自作注入）で 3 topic を subscribe/publish、xterm 配線（coalescing 16-64KiB）、`/ws/terminal` 撤去。
- **S5**（収束・任意）: control reverse-route を **topic-addressed ask**（§3.4.2 の (topic × ask) cell）に
  寄せ、command を場 namespace の ask 規律クラスとして載せる（bespoke 機構を 1 減らす）。「pub/sub に潰す」
  ではない＝ ask は ask のまま指し方が topic になるだけ。前提 = 「1 場 = 1 authority」不変条件（§3.4.3）。

## 5. agent = first-class World surface（制約）

human surface（vp-app / Vision Pro）と同格に、**agent（Claude）も World の独立 surface**として扱う
（`mem_1CcRMhGKFX3WCp6NJPUJ39`）。topic 空間 + event log + KDL schema は偶然 **理想的 agent harness の
3 要素**（observation = topic subscribe / action = topic publish / episodic memory = event log）。
L0 にこれを焼き込む（retrofit 不可）:

1. **MCP = World topic への thin generated bridge**。tool-call ↔ topic pub/sub を、vp-app と同じ単一
   endpoint で。手書き tool zoo（~40）を畳む → 「agent が見る VP」と「human が見る VP」が常に一致する。
2. **KDL schema = wire + agent action space の SSOT**。KDL→Rust/TS codegen に **KDL→MCP tool 定義**を
   第 3 ターゲット追加。新 channel = 新 tool が自動・型付きで生える（将来は agent が runtime で新 topic/
   schema を提案する自己拡張基盤へ）。
3. **event log = episodic memory + observe-loop closer**。build done / test fail / device plug / user
   focus / lane output を context 注入 or `vp events --since <cursor>` で配り、**agent の行動間 blind を
   解消**する（near-term で最も効く）。

→ doc 27 の制約: **topic schema / event log / KDL codegen は agent を peer subscriber/publisher
（generated tool 付き・単一 endpoint 共有）として設計する**。

## 6. 設計規律（L0 以降の全 PR の判断基準）

北極星「これは VP を単一 topic 空間に近づけるか」を L0 の具体ルールに落とす:

- **ad-hoc port 禁止**。新しい HTTP endpoint / WebSocket / per-SP port を足さない。新しい通信は
  **World の topic/channel** に足す。SP に listen を増やさない（撤去する方向にしか動かさない）。
- **state を二重化しない**。presence / lane / canvas の権威は World（daemon-canonical）。surface は
  mirror するだけ（vp-app の `SidebarState` 系は World RetainedStore に寄せる）。
- **wire 型は KDL schema から codegen**。enum 追従漏れ（PR #378）を構造的に絶滅させる。
- **addressing は論理識別子で**。`port`（reshuffle で揺れる物理）ではなく `project_path`→
  `normalize_path_key`（揺れない論理）で引く（#592 が control で実証した抽象化）。
- **surface は World 以外に繋がない**。remote から見た到達点を :32000 1 つに保つ。

## 7. 段階（bottom-up: L0 → L1 → L2 → L3）

`mem_1CcRLbsPMKXsBkt1Pk77fB` の phase を lifecycle 視点で再掲:

- **L0 transport/comm（SP-portless）** ← 本 doc。reverse-routing → lanes/canvas/control 集約（✅ #591/#592）
  → terminal Unison 化（§4）→ SP listen port 撤去。
- **L1 process/lifecycle**: daemon = LaunchAgent always-on（SMAppService）/ SP outbound-only /
  presence via World（Pull port scan 廃止）/ agent auto-reconnect / health status 修正。
- **L2 surfaces**: 各 surface を single-endpoint pure client に揃える（vp-app / agent / Swift agent）。
- **L3 M3 unification**: Swift WKWebView re-host → 統合 Mac App（vp-app 退役）+ visionOS app。

## 8. 未解決 / 次

1. **terminal perf spike**（§4）── portless 完了の律速。Unison-channel-over-postMessage の PTY 耐性を計測。
2. **SP HTTP API residual の畳み方**（§3.3）── World channel 吸収 vs process-proxy 寄せの線引き。
3. **transport / discovery / auth**（doc 25 §7）── :32000 を「認証付きで LAN/remote 公開」。Bonjour +
   QUIC+TLS（Network.framework native）+ Creo ID pairing。cross-device cloud-agent（§5 の到達性）の前提。
   → **doc 28 §5.3（location-transparent federation）が chronista-hub を制御面（registry / rendezvous /
   offline buffer）・World-to-World QUIC をデータ面とする実体設計**。
4. **agent action space codegen**（§5-2）── KDL→MCP tool 定義の codegen 設計。
5. **北極星の再シーケンス**（todo `mem_1CcRLwyxngfp2t76bBCiu1`）── 北極星 `mem_1Cb7iV6ZBczuqiBbiYQpvm` に
   SP-portless を明示 Phase 化 + cross-device(Apple) driver を追記。
6. **connection 共有**（§3.4.4）── N connection × 1 stream → **1 connection × N streams**。QUIC multiplex の
   回収（概念不変の perf 勝ち筋）。L1 portless の後・S5 protocol 統一の前に置く中間ステップ。
7. **「1 場 = 1 authority」不変条件の確立**（§3.4.3/3.4.5）── topic-addressed ask（S5）の routing 決定性の
   前提。場 prefix → authority 解決の table/claim をどう持つか。
8. **QoS クラスの場 prefix 設計**（§3.4.1/3.4.4）── `position/*` `audio/*` `asset/*` 等を live streaming
   実装時に切る際の規律（配送 discipline）と stream 割当の SSOT。terminal=text クラスが先行例。
