# 25 — Apple Platform Architecture（World = Mac / native client per surface）

> Status: **Draft / 方向確定**（2026-06-25）。実装は段階。second opinion（council / Gemini）と
> wire-format の Swift マッピング詰めが残る。
> 関連: doc 23（Bastet/Justice）, doc 24（vp-spine）, memory `vp-spine-architecture`,
> club-unison `design/wire-format.md`。

## 0. 一行で

**VP の「World」は Mac 上の常駐 daemon（Rust + club-unison server）が唯一の権威。
各 surface（vp-app / macOS menu bar agent / visionOS app）は Unison/QUIC で繋がる
「native client」。OS 統合は各 platform の Swift agent が native に担う。Rust↔Swift の
言語境界 = Unison の wire 境界（FFI を持ち込まない）。**

## 1. 背景・問題

- CoreMIDI / IOKit / メニューバー等の macOS OS API は **Cocoa main run loop を持つプロセス**
  で動くよう設計されている。headless tokio daemon には run loop が無く、CoreMIDI の hot-plug
  通知が配送されず device cache が起動時で凍結する（doc 23 Bastet の hot-plug が macOS daemon
  内で効かない実機バグ。2026-06-25 に実機で確認）。
- daemon 内に専用 CFRunLoop thread を立てて回避する案を実機検証したが、CoreMIDI 通知の
  非同期 source 登録 × run loop 即 return 等で噛み合わず、環境（MIDIServer）を wedge させる
  リスクも露呈。**headless daemon で Apple OS API と密に握手するのは構造的に逆流**と判断。
- 長期方針：**macOS 機能を深く・継続的に使う**＋**visionOS 版 VP** を作る。
  - visionOS の native UI は SwiftUI / RealityKit のみ。**wry/tao は visionOS に存在しない**＝
    Rust で spatial app は作れない。
  - modern Apple framework（App Intents / WidgetKit / RealityKit / SwiftUI 等）は **Swift 専用**。
    objc2 で届くのは Obj-C 系（AppKit / CoreMIDI）まで。

→ Apple platform 面（presentation + OS 統合）は **Swift first** が正しい。Rust-tao 路線は
macOS desktop 限定の袋小路。

## 2. 決定（Model D：分散・World = Mac）

```
┌─ Mac（home base・常駐）──────────────────────────────────────┐
│   TheWorld daemon (Rust)  ← 唯一の World 権威                     │
│     • World logic / process 管理 / lane state / Echoes …          │
│     • club-unison SERVER（Unison/QUIC, KDL schema 定義）           │
│         │ Unison / QUIC :32000                                    │
│     ├─ vp-app（desktop window, 現 wry/tao）= client               │
│     ├─ macOS menu bar agent（Swift, 新規）= local client + OS sensor│
│     └─ …                                                          │
└──────────────────┬───────────────────────────────────────────────┘
                   │ network（LAN / Bonjour / QUIC+TLS）
                   ▼
        visionOS app（Swift, SwiftUI/RealityKit）= 空間 client
          = 同じ World への空間 window（standalone World ではない）
```

- **World = Mac daemon**。これは VP の既存 daemon-canonical（doc 24 vp-spine）を**ネットワーク
  越しに他デバイスへ一般化するだけ**。新しい埋め込みモデルを発明しない。
- **Vision Pro = Mac 上の World への空間 client**。「World ＝ あなたの Mac の開発環境」という
  VP 哲学と一致。Mac 依存は仕様（projects / process / Claude session は Mac に居る）。

## 3. Rust ↔ Swift 境界 = Unison wire（FFI を持ち込まない）

- club-unison は「**KDL schema / wire-format を SSOT に、言語ごと native client**」の枠組み
  （既に **ruby / typescript** client が存在。ts は WebTransport で native QUIC）。gRPC 的モデル。
- VP の Swift 面は **その一 client** として乗る：**native Swift Unison client**
  （Apple `Network.framework` の native QUIC で `design/wire-format.md` に対し実装）。
  - → **Swift agent/app は Rust を一切リンクしない pure Apple-native** にできる。
  - → 言語境界 = ネットワーク境界に一致（Model D の素直さを最大化）。
- 却下した代替：**Rust club-unison を FFI で Swift に埋め込む**案。Rust 実装を共有できる利点は
  あるが、既存 native-client パターンに逆行＋tokio runtime in-app＋xcframework＋visionOS で
  quinn 検証、というコストを持ち込む。drift は wire-format SSOT + conformance test（`test-strategy.md`）
  で管理できるため native 実装で十分。

## 3.5 native Swift Unison client の設計

club-unison の protocol（`design/quic-runtime.md` / `wire-format.md`）を Swift に native mapping する。

**protocol stack（club-unison）**:
```
Service  : UnisonChannel（req/resp + event push、channel = QUIC stream、__channel: 多重化）
Protocol : PacketHeader（buffa=protobuf wire）+ length-prefixed framing（4B BE）
Transport: UnisonStream / read_frame・write_frame
Network  : quinn::Connection（QUIC / TLS 1.3）+ identity handshake
```

**Swift native client の対応**:
| club-unison 層 | Swift 実装 | 備考 |
|---|---|---|
| Network（QUIC/TLS1.3）| **`Network.framework` の `NWProtocolQUIC`（生 QUIC: streams + datagrams, TLS1.3 込み）** | Apple が macOS12/iOS15 (2021) で投入。quinn と RFC9000 で interop。**ALPN を server(quinn) と一致**させるのが唯一の必須整合点。WebTransport/HTTP3 経由は不要 |
| framing（4B BE length-prefix）| Swift で自前（自明）| stream channel 用 |
| datagram channel（`send_datagram`）| **`NWProtocolQUIC` の datagram（RFC 9221）** | club-unison は datagram channel も使う（dashboard metric `backend="datagram"`）。max datagram frame size を設定 |
| wire（PacketHeader = buffa）| **`swift-protobuf`（Apple 公式）を `protocol.proto` から生成** | buffa は protobuf wire 互換（ts は `@bufbuild/protobuf`）→ wire 互換 |
| channel mux（`__channel:`）/ UnisonChannel | Swift で channel 抽象（req/resp + event、channel=stream）実装 | ts/ruby client が前例 |
| identity handshake | Swift で実装 | 接続直後 server 自己紹介 |

> interop 確認事項：① **ALPN 文字列**（quinn server の設定値を `NWProtocolQUIC` に合わせる、要 club-unison QuicServer config 確認）、② TLS cert 検証（dev=localhost / cross-device=Creo ID pairing）、③ datagram frame size 上限。それ以外は標準 QUIC 同士で疎通する。

**2 つの codegen 層**（KDL schema-first を踏襲）:
1. **wire 層**: `protocol.proto` → `swift-protobuf` で Swift 型生成（tool で完結）。
2. **app 層**: KDL schema（protocol/channel/event）→ Swift の type-safe API。**当面は手書き**（ts/ruby の early client と同じ）、**将来 KDL→Swift codegen** を足す。

**API 形**（ts client design を mirror）: schema 駆動・subscribe 型・type-safe。
VP Canvas が unison に直接 subscribe（REST polling 1s → datagram 数 ms）の use case を Swift agent/visionOS app でも再現。

**段階**: ① 最小手書き Swift client（connect + handshake + 1 channel subscribe）で Mac daemon と e2e 疎通実証 → ② channel/codec を一般化 → ③ KDL→Swift codegen。

> **caller API contract の具体（ideal-caller-first sketch + SDK surface）= `docs/design/26-swift-unison-client.md`（承認済み）**。

→ **Swift agent/app は Rust を一切リンクせず（pure Apple-native）、Network.framework + swift-protobuf だけで Unison に乗れる**ことが確定。FFI 不要（§3）の裏付け。

## 4. OS 統合・CoreMIDI の解決

- **macOS menu bar agent（Swift, LSUIElement, login item）** が「OS と握手する手」になる：
  - Cocoa main run loop を自然に保持 → **CoreMIDI hot-plug が小細工なしで効く**。
  - device 抜き差しを検出 → Unison で daemon に報告（daemon の Bastet registry を更新）。
  - MIDI **データ** I/O（ROTO への LCD 送信・knob 受信）は run loop 不要なので daemon 側に
    残してよい（現状維持）／将来 agent に寄せる選択も可。**検出だけは run loop が要るので agent**。
  - 将来：menu bar UI / global hotkey / 通知 / Shortcuts / App Intents 等も agent に集約。
- → **daemon 内 CFRunLoop の苦労は不要**になる。#584 の daemon→client device bridge は
  そのまま生きる（agent が daemon に前段で feed するだけ）。

## 5. visionOS path

- visionOS は別 daemon プロセスを持てない（sandbox）が、**Model D では visionOS app は
  Mac daemon の native client なので問題なし**（Rust 埋め込み不要）。
- visionOS app = Swift（SwiftUI/RealityKit）+ native Swift Unison client。visionOS 固有の OS 統合
  （空間入力 / hand・eye tracking / RealityKit）は local、World data は Mac から Unison で受ける。
- MIDI fleet は物理的に Mac 接続なので MIDI は Mac 側に留まる。Vision Pro は lane を**空間的に
  視る/操る**。

## 6. 却下した代替案と理由

| 案 | 却下理由 |
|---|---|
| daemon に専用 CFRunLoop thread（Rust 自前 FFI）| CoreMIDI 通知が run loop に乗らず実機で不成立。MIDIServer wedge リスク。headless で OS API と握手するのは逆流 |
| Rust-tao の menu bar agent | macOS desktop 限定。**visionOS に載らない**（tao 無し）。modern Apple framework に届かない |
| Rust core を FFI 埋め込み（uniffi/swift-bridge）した standalone Swift app（Model E）| visionOS standalone が要件でない限り過剰。Rust-in-app + FFI 複雑性。Model D なら不要 |
| OS 統合を vp-app（既存 client）に同居 | 重い webview GUI に OS 統合を混ぜると拡張しづらい。専用 agent の方が素直 |

## 7. 未解決 / 次の検討

1. **transport / discovery / auth**：localhost :32000 を「認証付きで LAN 公開」する設計。
   Bonjour/mDNS discovery + QUIC+TLS（`Network.framework` native）。cross-device auth は
   既存 `vp auth` / Creo ID を流用。
2. **wire-format の Swift マッピング**：`design/wire-format.md` / KDL schema を Swift type に
   どう落とすか（手書き or 将来 codegen）。ruby/ts client を前例にする。
3. **menu bar agent の lifecycle**：起動（login item）/ daemon 不在時の degrade / 署名・配布。
4. **MIDI データ I/O の所在**：検出は agent 確定。ROTO control 等の I/O を daemon に残すか
   agent に寄せるか（run loop 不要なのでどちらも可、責務の綺麗さで判断）。
5. **段階実装計画**：① macOS Swift menu bar agent + CoreMIDI 検出 → daemon 報告（#584 完動）
   → ② native Swift Unison client 整備 → ③ visionOS app。
6. **second opinion**：背骨に関わる大決定なので council / Gemini で叩く。

## 8. 影響

- 現状（Rust daemon + Rust vp-app）は当面維持。Swift menu bar agent を**新規 surface として
  追加**するのが第一歩（既存を壊さない）。
- club-unison は「multi-client framework」として育てる（ruby / ts / **swift**）。VP はその consumer。
