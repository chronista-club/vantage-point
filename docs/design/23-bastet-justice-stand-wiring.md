> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# Design 23: Bastet 🧲 / Justice 🌫️ — External Control の Stand 配線 (How)

> **Status**: 設計 (実装前、Open Questions 全件解決済み §10)。epic v3.1 (`mem_1Cbwr1SBiuh9KgnpncbzMe`) の **E2 / E3 track** の How を規定する。
> **背景 hub**: External Control 背骨 `mem_1Cbfw7CjHkKmEDmAozWATW` / epic v3 hub `mem_1CbfzfLoxNAmspfoeyKrf5`
> **Why の起点**: MIDI dynamic routing vision (design-spark) `mem_1CavFi5D1aMSpEkas89SvQ`
> **protocol SSOT**: [doc 20](20-roto-control-sysex-protocol.md) / [doc 21](21-xtouch-mcu-protocol.md) / [doc 22](22-lpd8-mk2-protocol.md)

本 doc は、既に実機検証まで済んでいる **device 制御ロジック**（`device_profile.rs` 出力 / `device_input.rs` 入力）を、
**Stand アーキテクチャ**（Bastet 🧲 @ World + Justice 🌫️ @ Lane）に配線する設計を規定する。
device の byte 仕様は doc 20〜22 を、Stand framework の trait 受け皿は [doc 12](12-stand-architecture.md) を参照。本 doc は **配線（誰がどの instance を hold し、event がどう流れるか）** に集中する。

---

## 1. 何を解くのか — gap は「中身は完成、配線が未了」

E0 (protocol 解読) と E1 (DeviceProfile trait + 3 機種 impl) は完結済み。E2 の input flow (`device_input.rs`) も着手済み。
だが現状、これらは **`vp midi` CLI が `device_profile` / `device_input` を直接叩いている** だけで、Stand entity への昇格がされていない。

| 層 | 完成済み | 未配線 |
|----|---------|--------|
| **出力**（VP state → 機材 LED/LCD） | `device_profile.rs`（DeviceProfile trait + X-Touch/LPD8/ROTO） | どの Stand が profile を hold し、いつ projection するか |
| **入力**（機材 → VP） | `device_input.rs`（DeviceInput trait + RotoInput） | event を「今 active な Lane」へ届ける resolver |
| **device 集約** | `MidiCapability`（単一 port monitor） | 艦隊 N 台の同時 registry（= Bastet） |

→ 本設計のゴール: **Bastet（World の device registry）と Justice（Lane の双方向 I/O endpoint）** を新設し、上表右列を埋める。

---

## 2. Why — design-spark の 3 つの reframe を実装に落とす

`mem_1CavFi5D1aMSpEkas89SvQ` で固めた 3 つの設計判断が本 doc の前提になる。

### reframe 1: 「双方向 = 2 つの片方向 flow の合成」

真の双方向 channel は作らない。**独立した 2 flow** として設計する:

| flow | 方向 | 担当 | primitive |
|------|------|------|-----------|
| **input** | 機材 → VP → active Lane | DeviceInput → Bastet → Justice | `ControlEvent` を active Lane の command context へ |
| **output** | Lane state → 機材 LED/LCD | Justice → DeviceProfile → 機材 | lane state を subscribe し projection を送出 |

EventBus（uni-directional broadcast）を input に使っても、output 側を別 layer（state → projection）で担保すれば「双方向性がなくなる」問題は解消する。

### reframe 2: 「物理 controller で VP を演奏」

input = 操作（fader を動かす → active な値を tweak）、output = state の反映（現在値を LED brightness/color で表示）。
「MIDI を Pane に流す」より「**MIDI を command context に流す + state を LED に projection する**」方が clean。`size-stepper` skill 思想と一致。

### reframe 3: dynamic target = 「active Lane」という mutable runtime state

target は固定 address ではなく「**今 active な Lane**」。これを 1 layer（active Lane resolver）で track する。
B2 (#550 `vp midi roto control`) で「ROTO で active Lane を遠隔切替」が実機できた = この resolver の input 側が既に動いている。

---

## 3. Stand 配置 — 再帰 3-tier の World と Lane に割り付ける

External Control 背骨 (`mem_1Cbfw7CjHkKmEDmAozWATW`) の決定を doc 12 の trait 三分に対応づける。

```
World 👑   TheWorld(process) / Bastet 🧲(device registry) / Hierophant 💚(routing)
  │ controls
SP ⭐      PTY host / lane lifecycle / project routing  ← Lane の control 層
  │ controls
Lane       Echoes 💬 ∥ Heaven's Door 📖 ∥ engines ∥ Justice 🌫️(device I/O) ∥ Paisley Park 🧭
```

| Stand | scope | trait（doc 12） | 物理 instance | 役割 |
|-------|-------|-----------------|---------------|------|
| **Bastet 🧲** | World | `Service`（singleton infra） | `WorldCapabilities` に 1 | 物理 device を磁力で集約。registry / hot-plug discovery / routing policy |
| **Justice 🌫️** | Lane | `LaneStandHost`（passive marker） | `LaneStandRegistry` に per-lane | 霧で機器に侵入し双方向 I/O。lane state ↔ device の endpoint |

- **Bastet** は現 `MidiCapability`（`Service`, World scope, `hermit_purple@world` の後継座）を **multi-device registry に発展** させたもの。
- **Justice** は `lane_stand.rs` の `PaisleyParkStand` パターン（`LaneStandHost` impl, `stand_kind`）をそのまま踏襲し、`stand_kind = "justice"` で新設する。

> **なぜ Hermit Purple 🍇 を 2 つに割るのか**: 旧 Hermit Purple は「MIDI / MCP / tmux」を 1 Stand に束ねていたが、device の自然 scope は 3 層に散る — 物理 device は World 唯一（CoreMIDI port は machine 共有）、I/O endpoint は Lane 単位（active Lane ごとに projection 対象が違う）。1 Stand では表現できないので、**集約 = World/Bastet** と **I/O = Lane/Justice** に分割する。

---

## 4. 既存資産マッピング — 新規送信インフラはゼロ

本設計は **新しい MIDI 送受信インフラを一切足さない**。既存資産を Stand に配線するだけ。

| 既存資産 | 場所 | Bastet/Justice での役割 |
|----------|------|------------------------|
| `DeviceProfile` trait + 3 impl | `device_profile.rs` | Justice が hold（lane state → byte バッチ） |
| `DeviceInput` trait + `RotoInput` | `device_input.rs` | Bastet が input parse に使う（byte → `ControlEvent`） |
| `roto_palette::closest_index` | `roto_palette.rs` | ROTO profile 内で量子化（変更なし） |
| `LaneStandHost` + `LaneStandRegistry` | `process/lane_stand.rs` | Justice の host 受け皿 |
| `MidiCapability` | `capability/midi_capability.rs` | Bastet の前身（single → multi へ発展） |
| `midi::send_batch` | `midi.rs` | Justice の出力 action（pacing 確定済 #528） |
| `Service` trait | `capability/stand_service.rs` | Bastet の trait 受け皿 |

---

## 5. Bastet 🧲 設計（E2）— World の device registry

### 5.1 現 MidiCapability の限界

現 `MidiCapability` は `connected_port: Option<String>` + `monitor_task: Option<JoinHandle>` の **single-device monitor**。
1 度に 1 機材しか繋がない。design-spark の「機材が繋がっていれば常時接続」（艦隊 7 台同時）には足りない。

### 5.2 Bastet = multi-device registry

```rust,ignore
/// World scope の物理 device 集約 registry（Bastet 🧲）。
/// key = CoreMIDI port の displayName（背骨 mem 準拠）。
pub struct Bastet {
    /// 接続中 device（in/out port + 監視タスク）を port displayName で引く
    devices: HashMap<String, ConnectedDevice>,
    /// active Lane の購読 cache（SSOT は SP の lanes_state、Bastet は「lanes」channel 購読側。Q-1）
    active_lane: Arc<RwLock<Option<LaneAddress>>>,
    event_bus: Arc<EventBus>,
}

impl Service for Bastet {
    fn actor_name(&self) -> &str { "bastet" }      // wire address: bastet@world
    fn layer_scope(&self) -> LayerScope { LayerScope::World }
    fn as_any(&self) -> &dyn Any { self }
}
```

### 5.3 責務

| 責務 | 内容 |
|------|------|
| **registry** | 接続中 device を `HashMap<port_displayName, ConnectedDevice>` で hold |
| **hot-plug discovery** | `midir` enumeration を **2〜3s 周期**でポーリングし接続/切断を検出（Q-4。coremidi 直叩きは Phase 2） |
| **input parse** | 各 device の入力 byte を `DeviceInput::parse` で `ControlEvent` 化 |
| **routing policy** | `ControlEvent` を active Lane の `justice@<project>/<lane>` へ dispatch（= 下り命令、active Lane 優先 Q-2） |
| **active Lane track** | SP の「lanes」QUIC channel を購読し `active_lane` cache を更新（Q-1。SSOT は SP の `lanes_state`） |

> **hot-plug は midir polling（2〜3s）**: CoreMIDI の `MIDIClientCreate` notify callback（coremidi 直）は Phase 2。MVP は `midir` の port enumeration を周期比較（前回 list との diff）で十分。port enumeration は軽量なので 2〜3s でも負荷小（Q-4）。

---

## 6. Justice 🌫️ 設計（E3）— Lane の双方向 I/O endpoint

### 6.1 LaneStandHost impl

```rust,ignore
/// Lane に host される device I/O endpoint（Justice 🌫️）。
pub struct JusticeStand {
    state: RwLock<JusticeState>,
}

struct JusticeState {
    /// この Lane に bind された device profile 群（projection 対象）
    profiles: Vec<Box<dyn DeviceProfile>>,
}

impl LaneStandHost for JusticeStand {
    fn stand_kind(&self) -> &'static str { "justice" }  // PaisleyParkStand と同型
    fn as_any(&self) -> &dyn Any { self }
}
```

### 6.2 2 片方向 flow の担当点

```
input:   機材 → [Bastet: DeviceInput::parse] → ControlEvent → bastet@world
                  → (active Lane = この Lane なら) → justice@<project>/<lane>
                  → Lane command context（fader → token tweak 等）

output:  Lane state 変化 → [Justice subscribe] → DeviceProfile::project_track / learn_parameter
                  → Vec<Vec<u8>> → midi::send_batch（Bastet が hold する out port 経由）
```

- **output**: Justice は lane state（lane 名・色・active parameter）を subscribe し、`DeviceProfile` の `project_track` / `learn_parameter` で byte バッチを生成、送出を Bastet に委譲（out port は World が hold）。
- **input**: parse 自体は Bastet（device は World 集約）。Justice は「active Lane = 自分」のときだけ `ControlEvent` を受け取り Lane command context に着地させる。

> **なぜ parse は Bastet、projection は Justice か**: device の物理 in/out port は World に 1 つしかない（machine 共有）。だが「どの lane state を映すか」は Lane ごとに違う。port hold = World、state binding = Lane、という自然 scope に従う。

---

## 7. dispatch flow — 「Bastet uses Justice」

背骨 mem の `bastet@world → justice@<project>/<lane>` を doc 14 の wire address model で表現する。

| address | 解決 | 用途 |
|---------|------|------|
| `bastet@world` | World daemon（:32000）の Service | device registry への命令（全 device 列挙・接続・active Lane 切替） |
| `justice@<project>/<lane>` | SP host 経由で Lane の `LaneStandRegistry` | 特定 Lane への projection 指示 / input 着地 |

```
下り（命令 / routing）:  bastet@world ──(SP host)──> justice@<project>/<lane>
上り（event bubble up）:  justice ──> bastet（device 状態変化・lane state 更新通知）
```

- 下り = Bastet が active Lane を解決し、その Lane の Justice に projection を指示。
- 上り = device 切断や lane state 変化を Bastet に通知。
- いずれも World→SP→Lane の再帰階層を流れる（背骨 mem「External Control は階層を貫く path」）。

---

## 8. data / calculations / actions 分離（CLAUDE.md 規約）

E1 で確立した分離を Stand 配線でも保つ。

| 分類 | 該当 |
|------|------|
| **data** | `Rgb` / `ParamSpec` / `ControlEvent` / `ConnectedDevice` / `LaneAddress` |
| **calculations**（純粋） | `DeviceProfile::*`（state → byte）/ `DeviceInput::parse`（byte → event）/ active Lane resolve |
| **actions**（I/O） | `midi::send_batch`（送出）/ midir polling（discovery）/ wire dispatch |

→ Bastet / Justice は data と action の **配線（orchestration）** を担い、変換ロジック（calculations）は既存の純粋関数に委譲する。これにより Stand 層を I/O なしで unit test できる。

---

## 9. 実装 Phase（sub-PR 分解）

dev path 順は **E2 → E3**（Bastet uses Justice なので registry が土台）。

| sub-PR | scope | 依存 |
|--------|-------|------|
| **E2-0 cleanup** | `stands.rs`: `HERMIT_PURPLE` → `BASTET` rename + `JUSTICE` 追加 / `HermitPurpleState` skeleton（`project_stands_state.rs:111`）削除 | なし |
| **E2-1 Bastet 受け皿** | `Bastet` struct（`Service` impl）+ multi-device `HashMap` registry 型のみ。既存挙動への影響ゼロ | E2-0 |
| **E2-2 hot-plug** | midir enumeration polling で接続/切断検出、`devices` 更新 | E2-1 |
| **E2-3 input routing** | device 入力 → `DeviceInput::parse` → active Lane へ dispatch | E2-2 |
| **E3-1 Justice 受け皿** | `JusticeStand`（`LaneStandHost` impl, `stand_kind="justice"`）型 + `LaneStandRegistry` insert | E2-1 |
| **E3-2 output projection** | lane state subscribe → `DeviceProfile` → `send_batch` 委譲 | E3-1 |
| **Converge** | X-Touch fleet mixer（9 fader = lane×8）/ ROTO focus lane（page = lane 物理リコール）/ LPD8 action pad | 全部 |

> **PR-δ-1 / VP-159 PR-1 と同じ「i 路線」**: 各受け皿 PR は **型のみ**落として既存挙動ゼロ影響、lifecycle は後続 PR で必要が出た段階で足す。`LaneStandHost` / `Service` の先例（passive marker first）に倣う。

---

## 10. Open Questions — 全件解決済み（2026-06-18）

| # | 課題 | 決定 | 根拠 |
|---|------|------|------|
| **Q-1** | active Lane resolver の primitive | **既存「lanes」QUIC channel を Bastet が購読**。active Lane SSOT は SP の `lanes_state`(`session.json`) のまま、Bastet は購読側 | B2(#550) で CLI が同経路を実装済（`commands/midi.rs:786`）。新機構ゼロ・既存資産再利用。TopicRouter 新設や Bastet 内 field 保持（World→SP 逆流で scope 濁る）を回避 |
| **Q-2** | 1 device に複数 Lane が同時 projection 要求 | **active Lane 優先**。1 device は active Lane のみ projection、Lane 切替で対象も切替 | reframe 3「active Lane に流れる」と一致、調停ロジック最小。bind 方式は fleet mixer 不向きで Converge の特殊用途行き |
| **Q-3** | `midi` feature gate 無効時の Bastet placeholder | **現 `WorldCapabilities.midi: Option<...>` と同型の `Option` placeholder**。`#[cfg(feature="midi")]` gate 踏襲 | 既存 `WorldCapabilities::new`(midi:None) / `with_midi`(Some) パターンをそのまま流用 |
| **Q-4** | hot-plug polling 間隔 | **2〜3s**（device 抜き差しの体感重視） | midir port enumeration は CoreMIDI port list 取得のみで軽量。既存 reconcile(15s/30s)は機材反応に遅い。CoreMIDI notify callback(即時)は coremidi 直叩き = Phase 2 送り |
| **Q-5** | Justice projection の throttle | **MVP は throttle なし**、`send_batch` の既存 pacing(#528) に委譲。dogfood で高頻度変化が問題化したら再訪 | 早期最適化回避。pacing 自体は実機検証済 |
| **Q-6** | ROTO `learn` の per-lane parameter binding 表の定義場所 | **code default + `config.kdl` override**（`config.rs` schema 拡張）。Converge 段階で着手 | VP の config 流儀（code に妥当 default、kdl で上書き）。専用ファイル新設は現時点 over-engineering |

---

## 11. 関連

- **背骨**: External Control + Stand 配置 `mem_1Cbfw7CjHkKmEDmAozWATW`
- **epic**: v3.1 改訂 `mem_1Cbwr1SBiuh9KgnpncbzMe` / hub `mem_1CbfzfLoxNAmspfoeyKrf5`
- **Why 起点**: MIDI dynamic routing vision (design-spark) `mem_1CavFi5D1aMSpEkas89SvQ`
- **protocol SSOT**: [doc 20](20-roto-control-sysex-protocol.md) / [doc 21](21-xtouch-mcu-protocol.md) / [doc 22](22-lpd8-mk2-protocol.md)
- **Stand framework**: [doc 12](12-stand-architecture.md)（`Stand` / `Service` / `LaneStandHost` trait 三分） / [doc 13](13-paisley-park-revival.md)（`LaneStandHost` 先例 PR-δ）
- **wire address**: [doc 14](14-wire-address-v3.md)（`bastet@world` / `justice@<project>/<lane>`）
- **実装済み**: `device_profile.rs`（E1, #529/#530） / `device_input.rs`（E2 input, #531） / B2 active Lane 切替（#550）
- **機材インベントリ**: `mem_1CbwnzUnNGvw6bRDSzUMVZ`
