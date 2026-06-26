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
4. **agent action space codegen**（§5-2）── KDL→MCP tool 定義の codegen 設計。
5. **北極星の再シーケンス**（todo `mem_1CcRLwyxngfp2t76bBCiu1`）── 北極星 `mem_1Cb7iV6ZBczuqiBbiYQpvm` に
   SP-portless を明示 Phase 化 + cross-device(Apple) driver を追記。
