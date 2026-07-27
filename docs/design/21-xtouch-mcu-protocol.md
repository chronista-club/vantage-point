> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# 21. X-Touch MCU protocol — DeviceProfile 第一号実装の byte 仕様 SSOT

> **Status**: 実装済み（`crates/vantage-point/src/device_profile.rs` `mod xtouch`、E1）
> **Date**: 2026-06-12
> **対象機材**: Behringer X-Touch（MCU mode、8 モーターフェーダー + 8 V-Pot + scribble strip ×8）
> **出典**: Ardour `libs/surfaces/mackie/`（production 実装）の精読。
> `surface.cc`（ヘッダ / LCD / 色 / handshake）、`fader.cc`、`pot.cc`/`pot.h`、`led.cc`、`device_info.cc`
> **親設計**: [doc 20 — ROTO-CONTROL SysEx protocol](./20-roto-control-sysex-protocol.md) §8（`DeviceProfile` trait）

---

## §1. SysEx ヘッダ

```
F0 00 00 66 14 <command> <payload …> F7
   └───┬───┘ └┬┘
   Mackie    device ID
```

| device ID | 機種 |
|-----------|------|
| `0x14` | MCU / MCU Pro — **X-Touch standard mode はこれを名乗る** |
| `0x15` | MCU Extender (XT) |
| `0x10` / `0x11` | Logic Control / LC XT（challenge/response handshake を要求。X-Touch には使わない） |

Ardour は受信 SysEx の byte[4] で自分の送信ヘッダを上書きする（device が名乗った ID に合わせる）。VP は **`0x14` 固定**で送る。

## §2. command カタログ（host → X-Touch）

| message | byte 列 | 意味 |
|---------|---------|------|
| wake-up / device query | `F0 00 00 66 14 00 F7` | handshake 開始（§4） |
| LCD 行書き込み | `F0 00 00 66 14 12 <offset> <56 chars> F7` | scribble strip テキスト。offset `0x00` = 上段、`0x38` = 下段。1 strip 7 文字 × 8 = 56。Ardour は 55 文字送り（strip 8 の 7 文字目が欠ける。実機で `Param 8` → `Param ` を確認）だが、**X-Touch は 56 文字を正しく受ける**（実機検証済、2026-06-13）ため VP は全送する |
| **strip 色一括**（X-Touch 固有） | `F0 00 00 66 14 72 <c0…c7> F7` | 8 strip の色を一括設定。色値は §3 |
| モーターフェーダー | `E0+ch <LSB> <MSB>` | pitch bend 14bit。`value = round(16383 × normalized)`。strip 0–7 = ch 0–7、master = ch 8 |
| V-Pot LED ring | `B0 30+n <value>` | `value = (mode << 4) \| position(1–11)`、bit6 = 中央 LED。mode: `dot=0 boost_cut=1 wrap=2 spread=3` |
| ボタン LED | `90 <note> <vel>` | vel `0x00`=off / `0x01`=点滅 / `0x7F`=on。note 番号マップは未転記（必要時に Ardour `button.cc` から） |

## §3. 色 — 固定 8 色への量子化

`XTouchColors` enum（`surface.h`）: `0=Off 1=Red 2=Green 3=Yellow 4=Blue 5=Purple 6=Cyan 7=White`。

ROTO（83 色パレット、doc 20 §4）と同型の「RGB → 最近傍量子化 → state 同梱」モデル。VP では `closest_strip_color()`（二乗距離）で量子化し、**色だけの API は持たず `project_track` の一部**として送る。

## §4. handshake — X-Touch は challenge/response 不要

```
host:   F0 00 00 66 14 00 F7   (wake-up)
device: ready 応答（bytes[5] = 0x06 が X-Touch 固有 ready、0x01 が標準 MCU ready）
        → どちらも即 active 化してよい（Ardour: turn_it_on()）
```

Logic Control（device ID `0x10/0x11`）のみ serial + challenge 4 byte の暗号応答が必要だが、X-Touch を MCU mode で使う限り不要。**wake-up 送出後、ack を待たず projection を開始してよい**（実機は無 handshake でも追従）。

## §5. VP 実装

- impl: `crates/vantage-point/src/device_profile.rs` の `mod xtouch`（`XTouchProfile`）
- LCD 行・色が**全 strip 一括 command** のため、profile が shadow state（上段/下段テキスト + 色 8 本）を保持し、1 slot 更新でも全体を再構成する。これが `DeviceProfile` trait の projection 系を `&mut self` にした理由
- `project_track` → LCD 上段 + 色一括 / `learn_parameter` → LCD 下段 + フェーダー + V-Pot ring
- テキストは ASCII 限定（非 ASCII は `_` 置換、Ardour の ISO-8859-1 fallback と同方針）
- **バッチ送出には per-message 1ms pacing が必須**（実機検証 2026-06-13: 41 メッセージ無間隔連射で
  X-Touch が末尾側を取りこぼす。接続 drop 前の flush 待ちは pacing があれば不要 — pacing 抜き /
  flush 抜きの両ビルドの比較で確定。`midi::send_batch` が実装）
- 連続駆動（30fps × 8 fader の wave、240 msg/s）は接続保持なら pacing なしで安定（`vp midi xtouch wave` で検証）
- smoke コマンド: `vp midi xtouch demo`（階段）/ `vp midi xtouch wave`（波）

## §6. 残課題

1. **ボタン LED の note 番号マップ**（Rec/Solo/Mute/Select/transport）— input flow（§7 相当）実装時に Ardour `button.cc` から転記
2. **LCD 部分更新**（offset = `strip × 7`）— MCU 一般仕様からの推定。実機検証してから使う（現実装は Ardour と同じ全行書き込みで安全側）
3. **input flow**（fader touch / V-Pot 回転 / button → VP command routing）— E1 は output flow のみ

## §7. 関連

- [doc 20 — ROTO-CONTROL SysEx protocol](./20-roto-control-sysex-protocol.md): trait 設計の親、ROTO impl の仕様
- Ardour mackie surface: <https://github.com/Ardour/ardour/tree/master/libs/surfaces/mackie>
