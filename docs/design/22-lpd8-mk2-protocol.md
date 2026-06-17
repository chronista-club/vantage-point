# 22. LPD8 mk2 SysEx protocol — DeviceProfile 第二号実装の byte 仕様 SSOT

> **Status**: pad LED は実装済 + 実機検証済（2026-06-13、虹 8 色の表示確認）。program 構造は未実装（flag bit 未確定）
> **Date**: 2026-06-13
> **対象機材**: AKAI LPD8 mk2（2021、RGB pad ×8 + knob ×8）
> **出典**: Wireshark 実機キャプチャ由来のリバースエンジニアリング 3 repo が独立一致
> — [stephensrmmartin/lpd8mk2](https://github.com/stephensrmmartin/lpd8mk2)（program 構造の最精密資料）
> — [john-kuan/lpd8mk2sysex](https://github.com/john-kuan/lpd8mk2sysex)（仕様早見）
> — [john-kuan/lpd8mk2-traktor](https://github.com/john-kuan/lpd8mk2-traktor)（LED 制御の実コード）
> **親設計**: [doc 20](./20-roto-control-sysex-protocol.md) §8（`DeviceProfile` trait）/ [doc 21](./21-xtouch-mcu-protocol.md)（第一号 X-Touch）

---

## §1. SysEx ヘッダ — 初代 LPD8 とは別物 ⚠️

```
F0 47 7F 4C <command …> F7
   │  │  └─ model ID: LPD8 mk2 = 0x4C
   │  └─ device ID: 0x7F (broadcast)
   └─ manufacturer: 0x47 (Akai)
```

**初代 LPD8 の model ID は `0x75`、command 体系も別**（Send/Get = `61`/`63`、program 66 byte）。
`crates/vantage-point/src/midi.rs` の `mod lpd8`（`vp midi lpd8 write|switch`）は初代の ID に
独自の 11-byte pad 構造を継ぎ足したもので、**初代・mk2 どちらの実機とも互換性がない**ことが
2026-06-13 の実機検証で確定（mk2 は黙って無視する）。

## §2. pad LED 一括更新（実装済・実機検証済）

```
F0 47 7F 4C 06 00 30 <8 pad × 6 byte> F7    （計 56 byte）
```

- 1 pad = **フル RGB 6 byte**: R, G, B 各 0–255 を 7bit×2 に分割（`pack7(v) = [v >> 7, v & 0x7F]`、MSB→LSB 順）
- 例: 白 (255,255,255) → `01 7F 01 7F 01 7F`
- program 設定とは独立した即時表示 command。handshake 不要、前置きなしで効く
- **mk2 は艦隊で唯一 lane 色を量子化なしで表示できる pad**（X-Touch 8 色 / ROTO 83 色に対し 24bit RGB）

impl: `crates/vantage-point/src/device_profile.rs` `mod lpd8`（`Lpd8Profile`）。
8 pad 一括 command のため shadow state（`[Rgb; 8]`）を保持（X-Touch と同型）。
smoke: `vp midi lpd8 demo`（虹 8 色投影）。

## §3. program 構造（未実装、実装時に pin）

- Get = `0x03`（`00 01 <Prog#>`）/ Send = `0x01`（`01 29 …`）、program 全体 = **128 byte の entry 型**
- 1 entry = 8 byte: `[Index, ID, Chan, PC/Note, Min, Max, Flags, Reserved]`、`ID`: `0x0C`=knob / `0x09`=pad
- **未確定**: entry の Index 順（knob 6 表記と 8 個実機の齟齬）、Flags の bit 配置、Prog# 指定方法
  → 実装時に stephensrmmartin/lpd8mk2 の `config.py` + `docs/hex_diagram.svg` を raw 取得して確定すること
- 用途は input flow（pad note / knob CC の割当）なので E1 範囲外

## §4. 残課題

1. **`midi::lpd8`（初代 dialect のキメラ）の処遇** — `vp midi lpd8 write|switch` は mk2 実機で
   無効なことが確定。program 書き込みを §3 で再実装するときに置換 or 削除
2. program 構造の per-byte 確定（§3）
3. LED command の押下時挙動（press 中の色が program 側 on_color に切り替わるか）の実機観察
