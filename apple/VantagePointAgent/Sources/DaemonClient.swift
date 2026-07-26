import Foundation
import UnisonClient

/// Mac daemon (daemon :32000) への Unison 接続を司る。
///
/// 接続状態 (`Connection` / device channel) を所有するので `actor` で thread-safe に閉じ込める
/// (doc 26 §4 が `Connection` を actor にした意図と同じ)。 UI からは `AgentModel`
/// 経由でのみ叩かれ、 main thread には `Connection` を晒さない。
///
/// M1: connect + identity handshake。 M2: device channel (CoreMIDI hot-plug 報告) を追加。
actor DaemonClient {
    /// 現在の接続。 再接続時は古い接続を畳んでから張り直す。
    private var connection: Connection?
    /// agent → daemon の hot-plug 報告に使う device stream channel (doc 26 §2 channel "device")。
    private var deviceChannel: StreamChannel<VPDaemon.Device>?

    /// daemon に接続し、 server identity を取得し、 device channel を開く。
    ///
    /// transport は doc 25 §3.5: `NWProtocolQUIC` (生 QUIC) + ALPN `"unison"` + TLS1.3 +
    /// identity handshake。 これらは `UnisonClient.connect` の内部で完結する。
    /// dev の daemon は `dev_localhost` 自己署名 cert なので loopback では `.skipVerify`。
    func connectAndIdentify() async throws -> ServerIdentity {
        // 既存接続/channel を畳んでから張り直す (再接続で leak させない)。
        await teardown()

        let connection = try await UnisonClient.connect(
            to: .localDaemon(port: 32000),
            trust: .skipVerify
        )
        self.connection = connection
        let identity = try await connection.serverIdentity()

        // M2: device channel を開いておく (agent → daemon の ReportDevice 経路)。
        // 失敗しても identity は返す (device 報告は best-effort、 menu bar の daemon 表示は生かす)。
        do {
            self.deviceChannel = try await connection.openChannel(VPDaemon.device)
        } catch {
            print("[VPAgent] device channel open 失敗 (報告を無効化): \(error)")
        }

        return identity
    }

    /// CoreMIDI hot-plug 変化を daemon に報告する (device channel 経由、 best-effort)。
    ///
    /// daemon の `handle_device_report` が Devices registry を更新し、 `devices.*` を emit
    /// (→ daemon-device bridge → vp-app) する。 channel 未確立時は no-op。
    func reportDevice(_ change: MidiDeviceChange) async {
        guard let channel = deviceChannel else { return }
        let state = change.isConnected ? "connected" : "disconnected"
        do {
            let ack = try await channel.request(
                ReportDevice(
                    portName: change.portName,
                    state: state,
                    hasInput: change.hasInput,
                    hasOutput: change.hasOutput
                ))
            print(
                "[VPAgent] device \(state): \(change.portName) "
                    + "(in=\(change.hasInput) out=\(change.hasOutput)) ack=\(ack.ok)")
        } catch {
            print("[VPAgent] device report 失敗 (\(change.portName)): \(error)")
        }
    }

    /// 接続を明示的に閉じる。
    func disconnect() async {
        await teardown()
    }

    /// 現在の channel / connection を畳む (再接続前・切断時の共通後始末)。
    private func teardown() async {
        await deviceChannel?.close()
        deviceChannel = nil
        await connection?.disconnect()
        connection = nil
    }
}
