# 26 — native Swift Unison Client SDK（ideal-caller-first design）

> Status: **Draft / API contract 承認済み**（2026-06-25、ideal caller sketch を user 承認）。
> 親: doc 25（Apple Platform Architecture）§3 / §3.5。
> 方法論: club-unison `design/typescript-client-api.md`（ideal caller code を先に書いて SDK surface を
> 逆設計）を Swift で再現。multi-client framework（ruby / ts / **swift**）の一貫性を保つ。

## 0. 目的

VP の Swift surface（macOS menu bar agent / visionOS app）が Mac daemon の club-unison server に
**pure Apple-native（Rust リンク無し）**で繋ぐための SDK API contract。下層は doc 25 §3.5
（`NWProtocolQUIC` 生 QUIC + `swift-protobuf` + 4B BE framing + `__channel:` mux + identity handshake）。

## 1. 想定 use case（段階①：macOS menu bar agent）

connect → World state subscribe（menu bar 表示）→ CoreMIDI hot-plug を検出して daemon に報告。
agent が **両方向**（server→client datagram subscribe / client→server stream req-resp）を行使する。

## 2. KDL contract（daemon が agent に公開する protocol）

```kdl
protocol "vp-world" version="1.0.0" {
    namespace "club.chronista.vp"

    // daemon → agent: World state push（menu bar 表示, datagram）
    channel "world" from="server" lifetime="persistent" backend="datagram" channel_id=1 {
        event "ActiveLaneChanged" {
            field "project" type="string" required=#true
            field "lane"    type="string" required=#true
        }
        event "StandStatus" {
            field "stand"  type="string" required=#true
            field "status" type="string" required=#true
        }
    }

    // agent → daemon: CoreMIDI hot-plug を報告（stream, request/response）
    channel "device" from="client" lifetime="persistent" backend="stream" channel_id=2 {
        request "ReportDevice" {
            field "port_name"  type="string" required=#true
            field "state"      type="string" required=#true   // "connected" | "disconnected"
            field "has_input"  type="bool"
            field "has_output" type="bool"
            returns "Ack" { field "ok" type="bool" }
        }
    }
}
```

## 3. Ideal caller Swift code（「これで agent を書きたい」）

```swift
import UnisonClient
import VPProtocol   // vp-world.kdl から生成: channel meta + event/request 型

// 1. Mac daemon に接続（生 QUIC + TLS1.3 + identity handshake、すべて connect 内）
let client = try await UnisonClient.connect(
    to: .localDaemon(port: 32000),     // cross-device は .bonjour("_unison._udp")
    trust: .system                      // dev localhost は .skipVerify
)

// connection lifecycle（reconnect は caller 責務 — library は auto-reconnect しない / ts と同方針）
Task {
    for await ev in client.connectionEvents {
        switch ev {
        case .connected(let remote):    log("connected: \(remote)")
        case .disconnected(let reason):  log("disconnected: \(reason)") /* 自前 reconnect */
        }
    }
}

let id = try await client.serverIdentity()
log("World: \(id.name) v\(id.version)")

// 2. World state を subscribe（datagram channel）→ menu bar を更新
let worldChan = try await client.openDatagramChannel(VPWorld.world)   // 生成 meta が channel_id/型を narrow
Task {
    for await event in worldChan.events {        // AsyncSequence<VPWorld.WorldEvent>（型安全）
        switch event {
        case .activeLaneChanged(let e): await MenuBar.shared.setLane(e.project, e.lane)
        case .standStatus(let e):       await MenuBar.shared.setStand(e.stand, e.status)
        }
    }
}

// 3. CoreMIDI hot-plug を検出 → daemon に報告（stream channel, request/response）
//    CoreMIDIWatcher は AppKit main run loop 上なので通知が「自然に」効く（doc 25: daemon の苦労が消える点）
let deviceChan = try await client.openChannel(VPWorld.device)
let midi = CoreMIDIWatcher()
for await change in midi.deviceChanges {          // AsyncSequence<MidiChange>
    let ack = try await deviceChan.request(.reportDevice(.init(
        portName:  change.portName,
        state:     change.isConnected ? "connected" : "disconnected",
        hasInput:  change.hasInput,
        hasOutput: change.hasOutput
    )))
    if !ack.ok { log("daemon rejected device report") }
}
```

## 4. 逆設計された Swift SDK surface（Phase 2 実装 contract）

```swift
public enum UnisonClient {
    public static func connect(to: Endpoint, trust: TrustPolicy) async throws -> Connection
}
public enum Endpoint    { case localDaemon(port: UInt16), host(String, port: UInt16), bonjour(String) }
public enum TrustPolicy { case system, skipVerify, pinned(Data) }

public actor Connection {                         // actor = 接続状態の thread-safe 集約
    public var connectionEvents: AsyncStream<ConnectionEvent> { get }
    public func serverIdentity() async throws -> ServerIdentity
    public func openDatagramChannel<M: DatagramChannelMeta>(_ meta: M) async throws -> DatagramChannel<M>
    public func openChannel<M: StreamChannelMeta>(_ meta: M) async throws -> StreamChannel<M>
    public func disconnect() async
}

// KDL schema から生成される channel 型（event=enum, request=型付き method）
public struct DatagramChannel<M: DatagramChannelMeta> {
    public var events: AsyncStream<M.Event> { get }          // server→client push
    public func close() async
}
public struct StreamChannel<M: StreamChannelMeta> {
    public var events: AsyncStream<M.Event> { get }
    public func request<R: M.Request>(_ req: R) async throws -> R.Response  // 型 narrowing で response 自動推論
    public func close() async
}
```

## 5. 設計ノート

- **ts client API を 1:1 で Swift idiom に翻訳**：`connect / serverIdentity / openChannel /
  openDatagramChannel / request / events / close`。`AsyncIterable`→`AsyncStream`、`Promise`→`async throws`、
  生成 `ChannelMeta`→Swift generic + 生成 enum。caller 体験を ruby / ts と揃える。
- **transport は caller に透過**（doc 25 §3.5）：`connect` 内で `NWProtocolQUIC`（ALPN 一致）+ TLS1.3 +
  handshake、`events`/`request` は内部で 4B BE framing + `swift-protobuf`(PacketHeader) を処理。
- **reconnect は caller 責務**（library は auto-reconnect しない、ts と同方針）。SDK を薄く保つ。
- **2 codegen 層**：wire(`protocol.proto`→swift-protobuf, tool) / app(`vp-world.kdl`→`VPProtocol`
  の channel meta + 型、当面手書き→将来 KDL→Swift codegen)。
- **agent の二役**：World subscribe（datagram, server→client）＋ CoreMIDI 報告（stream req/resp,
  client→server）。後者は AppKit run loop 上で CoreMIDI 通知が自然に効く＝Model D の勝ち筋。

## 6. 未解決 / 次

1. **ALPN 確認**（club-unison QuicServer config）→ `NWProtocolQUIC` に同値を設定。
2. **wire 詳細**：`__channel:` mux の Swift mapping、PacketHeader の channel routing、handshake message。
   `design/quic-runtime.md` / `wire-format.md` を実装時に追従。
3. **段階①の実装**：最小 `UnisonClient`（connect + handshake + 1 datagram subscribe + 1 stream request）を
   手書きし、Mac daemon と e2e 疎通実証。
4. **cross-device**：`.bonjour` discovery + Creo ID pairing（auth）。
5. 将来：KDL→Swift codegen（`VPProtocol` の自動生成）。
