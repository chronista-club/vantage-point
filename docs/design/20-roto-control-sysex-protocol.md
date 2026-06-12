# 20. ROTO-CONTROL SysEx protocol — Justice 🌫️ device profile の実装 SSOT

> **Status**: protocol reverse-engineered（grammar 確定・実装前）。E0 de-risk 完了。
> **Date**: 2026-06-12
> **対象機材**: Melbourne Instruments ROTO-CONTROL（8 モーター駆動ノブ + 9 LCD + 16 RGB ボタン）
> **Pair memories**:
> - ROTO protocol 実コード解読: `mem_1CbwpJBuUSzaGHN2ce9EyM`
> - 機材インベントリ v3: `mem_1CbwnzUnNGvw6bRDSzUMVZ`
> - epic v3 改訂 v3.1（External Control 昇格）: `mem_1Cbwr1SBiuh9KgnpncbzMe`
> **親設計**: [doc 12 — Stand architecture（LSCM）](./12-stand-architecture.md) の External Control（Bastet 🧲 / Justice 🌫️）

---

## §0. このドキュメントの位置づけ

ROTO-CONTROL は VP の物理コントロール艦隊の **main** に据える機材であり、Justice 🌫️（per-lane device I/O）が「霧で侵入し双方向 I/O」を文字通り実現する対象。本 doc は **VP の Rust daemon が ROTO の LCD・色・モーター値を host から動的制御するための protocol 仕様**を、一次実装の精読から確定したものである。

### 出典 — 実コード decompile

「ROTO-CONTROL は SysEx 非対応」という公式 FAQ の言明は **MIDI Mode のユーザー向け話法**であり、host 統合 mode は明白に SysEx を使う（deep-research の adversarial 検証で 1-2 refute）。本仕様は Melbourne 公式の Bitwig 拡張を decompile して得た:

```
JAR:    https://update.melbourneinstruments.com/roto-control/Roto-Control.bwextension
        （1.88MB、非難読化の標準 Zip/JAR、96 .java に full decompile 済）
tool:   CFR 0.152（github.com/leibnitz27/cfr）+ openjdk
key class:
  MidiCommand        フレーム
  MidiProcessor      command 定義（567 行）
  StringUtil         text/hash エンコード
  value/ColorUtil    83 色パレット
  RotoHwElements     物理要素 addressing
  states/TrackState  track 表示（色同梱）
  device/RotoParameter  knob parameter learn model
```

---

## §1. SysEx フレーム

全ての host→ROTO メッセージは下記 8 + payload バイトの枠に収まる（`MidiCommand` constructor が確定）:

```
F0 00 22 03 02 <type> <id> <payload …> F7
└──────┬─────┘ │  └─┬─┘ └┬┘            └┬┘
 固定ヘッダ      │  種別  cmd  payload     終端
               02  data[5] data[6]       0xF7
```

| byte | 値 | 意味 |
|------|-----|------|
| 0 | `F0` | SysEx start |
| 1-3 | `00 22 03` | manufacturer ID（Melbourne Instruments） |
| 4 | `02` | product / category 固定 prefix |
| 5 | `<type>` | command type（mode 別、下表） |
| 6 | `<id>` | command id（操作別） |
| 7… | payload | `payloadSize` バイト |
| 末尾 | `F7` | SysEx end |

### command type

| type | mode | 拡張側定数 |
|------|------|-----------|
| `0x0A`（10） | GENERAL（DAW 制御・track・LCD） | `CMD_ID_GENERAL` |
| `0x0B`（11） | PLUGIN（device/parameter） | `CMD_ID_PLUGIN` |
| `0x0C`（12） | MIXER | `CMD_ID_MIXER` |

> index を payload に持つ command は一律 **7bit×2 分割**（`index>>7 & 0x7F`, `index & 0x7F`）でエンコードする。

---

## §2. command カタログ（host → ROTO、完全版）

decompile で確認した全 SysEx テンプレート。`%02X` = 1 byte hex、`%s` = 可変長 byte 列（name/hash）。

### type 0A — GENERAL

| id | テンプレート | 意味 |
|----|-------------|------|
| `01` | `02 0A 01 F7` | **handshake 開始**（DAW_START、`initDaw` が送出） |
| `03 02` | `02 0A 03 02 F7` | ping |
| `07` | `02 0A 07 <hi> <lo> <name13> <colorIdx> <isGroup> F7` | **track 表示更新**（名前+色+group flag、`TrackState.toSysExUpdate`） |
| `08` | `02 0A 08 F7` | track detail 終了 |
| `0B` | `02 0A 0B <8×%02X> F7` | transport 状態（8 byte、`TransportState`） |
| — | `02 0A <code> F7` / `02 0A <code> <hi> <lo> F7` | 汎用 general value / index command |

### type 0B — PLUGIN

| id | テンプレート | 意味 |
|----|-------------|------|
| `05` | `02 0B 05 <idx> <name> <??> <name> <colorIdx> <??> F7` | device 表示（`DeviceState` / `PluginModeHandler`） |
| `06` | `02 0B 06 F7` | plugin detail 終了 |
| `08` | `02 0B 08 <pluginIdx> <pageIdx> <force> F7` | plugin select（※ web 調査が RGB と誤読したもの） |
| `0A` | `02 0B 0A <idxHi> <idxLo> <hash6> <isMacro> <detent> <steps> <posHi> <posLo> <name13> [<stepNames>] F7` | **parameter learn**（knob にパラメータを teach、§5） |
| `0E 01` | `02 0B 0E 01 <val> F7` | macro 値（`RotoMacroParameter`） |
| `0F` | `02 0B 0F 00 <idx+1> <hash6> <name13> F7` | parameter 名変更（`getSysExNameChange`） |
| — | `02 0B <code> <value> F7` | 汎用 plugin value |

### type 0C — MIXER

| id | テンプレート | 意味 |
|----|-------------|------|
| `04` | `02 0C 04 <hi> <lo> <name13> <colorIdx> <isGroup> F7` | 選択 track 表示（`TrackState.toSysExUpdateSel`） |
| `03` | `02 0C 03 <n> F7` | send 数更新 |
| `08` | `02 0C 08 <s> F7` | effect track set（`EffectTrackSet`） |
| `0B 2F` | `02 0C 0B 2F <val> F7` | meter threshold |
| `0C` | `02 0C 0C <8×bool> F7` | VU メーター有効化（`MidiCommand(12,12,8)`） |

### input echo（CC、SysEx 外）

| 操作 | message |
|------|---------|
| button 値 state | CC `0xBF` / `<ccNr>` / `<value>`（`setButtonValueState`） |

---

## §3. テキストエンコード（2 種）

ROTO は name を **hex byte 列**（"`%02X `" 連結）で受け取る。用途で 2 つのエンコーダがある（`StringUtil`）:

| 関数 | 文字数 | 用途 | 仕様 |
|------|-------|------|------|
| `toSysExName` | **13** | display（`%s` の displayName） | 13 文字 ASCII、各 char 1 byte、不足は `00` padding |
| `nameToSysEx` | **12 + null** | state（track/param の `name13`） | 12 文字に切り、13 スロット出力（末尾は必ず `00` 終端） |

共通の `toAsciiDisplay`:
- 非 ASCII（`>= 0x80`）は ラテン置換: `ä→a ö→o ü→u Ä→A Ö→O Ü→U ß→ss é,è,ê→e â,á,à→a û,ú,ù→u ô,ó,ò→o`。置換表にない非 ASCII は **drop**。

### parameter 識別 hash（`createHash`）

parameter command の `<hash6>` は parameter の fullId（path）から生成する 6 byte 識別子:
- `SHA-1(fullId)` の先頭 6 byte、各 byte を `& 0x7F`（MIDI data byte 化）。
- VP では parameter の安定 ID（lane の slot path 等）から同じ方式で生成すれば良い。

---

## §4. 色 — パレット index を state に同梱（独立 command なし）⚠️

**ROTO に「色だけ送る」command は存在しない。** 色は track / parameter の **state 更新メッセージに `colorIndex` フィールドとして埋め込まれる**（`TrackState`: `02 0A 07` / `02 0C 04` の payload）。

- 拡張は **83 色固定パレット** `ColorUtil.COLORS[]`（各要素は RGB int）を保持。
- host が RGB を投げると `findClosestColor` がユークリッド距離 `sqrt((r-cr)² + (g-cg)² + (b-cb)²)` で**最近傍の palette index に量子化**。
- `colorIndex` のデフォルトは `70`（= `0x000000` 黒）。値域は 0–82（83 色）。

VP 側の含意: `set_color` は「RGB を受けて VP 側で同じ最近傍量子化 → palette index を track/param state に同梱」する。**独立した色 API ではなく、state projection の一部として設計する**こと。パレットの 83 値は **`crates/vantage-point/src/roto_palette.rs`**（`ROTO_PALETTE` + `closest_index`）に転記済み。代表値: `0xFF0000` 赤 / `0xFFFF00` 黄 / `0xFFFFFF` 白 / `0x000000` 消灯（index 70）。

---

## §5. 物理 addressing と parameter learn model

### 物理要素（`RotoHwElements`）

| 要素 | 数 | addressing |
|------|----|-----------|
| knob | 8 | index `0–7`（`RotoKnob(i, …)`） |
| button | 8 | CC `20–27` |
| transport button | 8 | CC `28–35` |
| left / right transport | 2 | CC `36` / `37` |

→ 「16 RGB ボタン」= button 8 + transport button 8。LCD は knob と 1:1（8）+ master 1 = 9。

### parameter learn（`02 0B 0A`）— 単純な値更新ではない

knob に割り当てる parameter は「**learn**」という rich な記述単位で teach する（`RotoParameter.getLearnSysEx`）:

```
02 0B 0A <idxHi> <idxLo> <hash6> <isMacro> <centerDetent> <steps> <posHi> <posLo> <name13> [<stepNames>] F7
```

| field | 意味 |
|-------|------|
| `idxHi/idxLo` | knob index（0–7）を 7bit×2 |
| `hash6` | parameter 識別 hash（§3） |
| `isMacro` | macro parameter フラグ |
| `centerDetent` | 中央 detent（双極パラメータの 0 位置でクリック感） |
| `steps` | 段階数（0 = 連続、`-1` も 0 送出。16 以下なら末尾に `stepNames` 付与） |
| `posHi/posLo` | 現在値 = `round(value × 16383)` を 14bit で（= モーター位置） |
| `name13` | 表示名（`toSysExName`） |
| `stepNames` | steps ≤ 16 のとき各ステップのラベル列 |

**設計含意**: ROTO の output は「値を 1 つ送る」ではなく「knob にパラメータの意味（名前・型・detent・段階・値・色）を一括 projection する」モデル。VP の `DeviceProfile` はこの粒度（parameter 単位の state projection）で設計する。

---

## §6. handshake と initialized gate

```
1. init():  midiProcessor.initDaw(versionString)
            → sendSysex("02 0A 01")  (DAW_START)
2. ROTO 側が ready 応答（input: type 0A general command）
3. initialized = true になるまで sendSysEx は no-op
            （`if (!initialized) return;` ガード）
4. 以降 track/parameter state projection が流れる
```

VP 実装では DAW_START 送出 → input handshake ack 受信 → projection 開始、の state machine を持つ。

---

## §7. input flow（機材 → VP）

ROTO からの SysEx は `handleRotoUpdate(command, commandNum, data)` で type 別に分岐（`command == 10/11/12` = general/plugin/mixer）し、`commandNum` で操作を識別する（plugin は case 1/4/7/9/11/12/13/16/17/18 等）。

VP の Justice input flow はこの分岐を踏襲: knob 回し・button 押下 → type/commandNum → active Lane の command に routing。`setCcKnobMatcher` 相当の matcher を VP 側に持つ。

---

## §8. VP 実装への接続

### 既存コードとの境界（Purple Haze 調査）

- 送信口は既に集約済み: `crates/vantage-point/src/midi.rs:send_sysex(port_pattern, &bytes)`。本 protocol の byte 列はそのまま渡せる。**新規 MIDI 送信インフラは不要。**
- `crates/vantage-point/src/midi.rs` の `mod lpd8`（`Program::to_sysex`）が「named control → bytes 翻訳」の動く prototype。これを一般化した `trait DeviceProfile` の ROTO impl として本仕様を実装する。

### DeviceProfile trait（E1、改訂案）

色が state 同梱・parameter が learn model であることを踏まえ、当初の単純な `set_lcd/set_color` から **state projection 粒度**に修正:

```rust
// data: 論理 lane state、calculation: bytes 生成、action: send_sysex
trait DeviceProfile {
    // track/slot 単位の state projection（名前+色を一括、§4）
    fn project_track(&self, index: u8, name: &str, color: Rgb, is_group: bool) -> Vec<u8>;
    // knob parameter の learn（§5、名前+値+detent+steps）
    fn learn_parameter(&self, index: u8, p: &ParamSpec) -> Vec<u8>;
    // handshake（§6）
    fn handshake(&self) -> Vec<Vec<u8>>;
    // input: 機材イベント → 論理 command（§7）は別 trait/flow
}
```

impl 順は確度順: X-Touch（Ardour 由来・最確実、[doc 21](./21-xtouch-mcu-protocol.md)）→ LPD8（既存移行）→ ROTO（本 doc）。

> KEYSTAGE（完全 MIDI 2.0 ネイティブ）は 4 枚目の profile として独立に着手する。
> learn 相当は MIDI-CI Property Exchange（標準規格）、transport は `midir`（1.0 専用）では
> 解像度が落ちるため Core MIDI UMP（Rust では `coremidi` crate）の別バックエンドが必要。
> trait は送信を持たない（calculations のみ）ので transport 分岐は trait 外で吸収できる。

---

## §9. 双方向 = 2 つの片方向 flow

前駆 vision（`mem_1CavFi5D1aMSpEkas89SvQ`、2026-05-11）の「双方向 = 2 flow 合成」を本機材で具体化:

- **input flow**（機材 → VP）: §7。knob/button → Justice → active Lane の command。
- **output flow**（VP → 機材）: §2/§4/§5。lane state（progress / status / parameter）→ Justice → track/parameter projection。

この 2 flow は Bastet 🧲（World device registry）/ Justice 🌫️（per-lane I/O）の責務分割に対応する。

---

## §10. 残・実装時に確定

grammar の骨格は確定済み。以下は実装時に詰める:

1. ~~`ColorUtil.COLORS` の値の完全転記~~ → **完了**（`crates/vantage-point/src/roto_palette.rs`、83 値）。
2. **handshake ack の input フォーマット**（`initialized` を立てる input command の正確な type/commandNum）。
3. **device 表示 `02 0B 05` の可変フィールド**（PluginModeHandler の完全 payload）。
4. **transport `02 0A 0B` の 8 byte 各ビット意味**（play/stop/rec/loop…）。

---

## §11. 関連

- [doc 12 — Stand architecture（LSCM）](./12-stand-architecture.md): Bastet/Justice の Layer 配置
- epic v3 改訂 v3.1: `mem_1Cbwr1SBiuh9KgnpncbzMe`
- X-Touch（MCU）protocol: Ardour `libs/surfaces/mackie/`（production 実装、別 doc 化候補）
