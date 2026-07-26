import CoreMIDI
import Foundation

/// CoreMIDI device 1 件の着脱変化（agent → daemon 報告の最小単位）。
struct MidiDeviceChange: Sendable {
    /// CoreMIDI port の displayName（daemon Devices registry の key と同値）
    let portName: String
    /// true = 接続 / false = 切断
    let isConnected: Bool
    let hasInput: Bool
    let hasOutput: Bool
}

/// CoreMIDI の device hot-plug を監視し、 変化を `AsyncStream<MidiDeviceChange>` で流す。
///
/// doc 25 Model D の勝ち筋: `MIDIClientCreateWithBlock` の notify block は「client を作った
/// thread の run loop」で配送される。 agent は AppKit main run loop を保持するので、 headless
/// daemon では届かなかった CoreMIDI 構成変化通知が main で自然に効く。
///
/// CoreMIDI C API を Swift から触るラッパなので `@unchecked Sendable`: 内部可変状態（`known`）は
/// init（main）と notify block（= client 作成 thread = main）からのみ触り、 単一スレッドアクセスを
/// 前提とする。 yield 先の `AsyncStream.Continuation` は元々 Sendable。
final class CoreMIDIWatcher: @unchecked Sendable {
    private var client = MIDIClientRef()
    /// 直近の enumeration（port displayName → (has_input, has_output)）
    private var known: [String: (Bool, Bool)] = [:]
    private let continuation: AsyncStream<MidiDeviceChange>.Continuation

    /// device 変化のストリーム。 `start()` 後、 まず現在の全 device が `connected` として流れ、
    /// 以降は着脱の差分が流れる。
    let changes: AsyncStream<MidiDeviceChange>

    init() {
        let (stream, cont) = AsyncStream<MidiDeviceChange>.makeStream()
        self.changes = stream
        self.continuation = cont
    }

    deinit {
        // 再接続 (AgentModel.connect → startDeviceReporting で旧 watcher を破棄) のたびに
        // CoreMIDI client ハンドルを OS レベルでリークさせないよう、対称的に dispose する。
        if client != 0 {
            MIDIClientDispose(client)
        }
        continuation.finish()
    }

    /// CoreMIDI client を作って監視を開始し、 初期 device 一覧を即 emit する。
    /// main run loop を持つ thread（= agent の main）から呼ぶこと。
    func start() {
        let name = "vp-agent-coremidi" as CFString
        MIDIClientCreateWithBlock(name, &client) { [weak self] notificationPtr in
            // setupChanged = device 着脱を含むあらゆる構成変化。 再 enumerate して diff を取る。
            if notificationPtr.pointee.messageID == .msgSetupChanged {
                self?.rescan()
            }
        }
        // 接続直後に現在の device を報告して daemon registry を埋める。
        rescan()
    }

    /// 現在の endpoint を列挙し、 `known` との diff を `MidiDeviceChange` として emit する。
    private func rescan() {
        let current = Self.enumerate()

        // 追加 or in/out 構成が変わった device → connected
        for (name, io) in current where known[name].map({ $0 != io }) ?? true {
            continuation.yield(
                MidiDeviceChange(
                    portName: name, isConnected: true, hasInput: io.0, hasOutput: io.1))
        }
        // known から消えた device → disconnected
        for name in known.keys where current[name] == nil {
            continuation.yield(
                MidiDeviceChange(
                    portName: name, isConnected: false, hasInput: false, hasOutput: false))
        }

        known = current
    }

    /// source(input) + destination(output) endpoint を displayName で merge して列挙する。
    /// 物理 device は同一 displayName で in/out 両 endpoint を持つため名前で畳む
    /// （daemon の `enumerate_ports`（devices.rs）と同じ正規化）。
    private static func enumerate() -> [String: (Bool, Bool)] {
        var result: [String: (Bool, Bool)] = [:]

        for i in 0..<MIDIGetNumberOfSources() {
            if let name = displayName(MIDIGetSource(i)) {
                var entry = result[name] ?? (false, false)
                entry.0 = true
                result[name] = entry
            }
        }
        for i in 0..<MIDIGetNumberOfDestinations() {
            if let name = displayName(MIDIGetDestination(i)) {
                var entry = result[name] ?? (false, false)
                entry.1 = true
                result[name] = entry
            }
        }

        return result
    }

    /// endpoint の `kMIDIPropertyDisplayName` を読む（midir = coremidi と同じ user-facing 名）。
    private static func displayName(_ endpoint: MIDIEndpointRef) -> String? {
        var param: Unmanaged<CFString>?
        let status = MIDIObjectGetStringProperty(endpoint, kMIDIPropertyDisplayName, &param)
        guard status == noErr, let cf = param?.takeRetainedValue() else { return nil }
        return cf as String
    }
}
