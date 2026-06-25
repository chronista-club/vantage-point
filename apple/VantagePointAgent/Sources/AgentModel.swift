import Foundation
import UnisonClient

/// menu bar agent の UI 状態を集約する observable model。
///
/// UI に触れるので `@MainActor`。 daemon との実際の I/O は `DaemonClient` (actor) が持ち、
/// ここはその結果 (`Status` / 報告中 device) を `@Published` で SwiftUI に流すだけ (data/action 分離)。
@MainActor
final class AgentModel: ObservableObject {
    /// daemon 接続のライフサイクル状態。
    enum Status {
        /// 接続試行中。
        case connecting
        /// 接続成功 + identity handshake 取得済。
        case connected(ServerIdentity)
        /// 接続 or handshake 失敗。 理由文字列を保持。
        case failed(String)

        /// menu bar icon に使う SF Symbol 名。
        var symbolName: String {
            switch self {
            case .connecting: return "circle.dotted"
            case .connected: return "circle.fill"
            case .failed: return "exclamationmark.triangle"
            }
        }
    }

    @Published private(set) var status: Status = .connecting
    /// M2: agent が daemon に報告中の device displayName 一覧 (menu 表示用)。
    @Published private(set) var reportedDevices: Set<String> = []
    /// M2b: 稼働中の SP 一覧 (旧 daemon tray の instance 一覧を menu bar agent に一本化)。
    @Published private(set) var instances: [VpInstance] = []

    private let client = DaemonClient()
    /// CoreMIDI 監視 (接続成功後に起動)。 client + notify block を生かすため保持する。
    private var watcher: CoreMIDIWatcher?
    /// hot-plug 変化を daemon に流し続けるループ。
    private var devicePump: Task<Void, Never>?

    init() {
        // 起動と同時に接続を開始する。 main run loop は SwiftUI が保持するので、
        // ここで Task を投げても agent は常駐し続ける。
        Task { await connect() }
    }

    /// daemon へ接続し直す。 menu の「再接続」からも呼ばれる。
    func connect() async {
        status = .connecting
        do {
            let identity = try await client.connectAndIdentify()
            status = .connected(identity)
            // M1 疎通の機械検証用。 LSUIElement app でも terminal 直起動なら stdout に出る。
            print(
                "[VPAgent] connected: World \(identity.name) v\(identity.version) "
                    + "ns=\(identity.namespace) channels=\(identity.channels)")
            // M2: CoreMIDI hot-plug 監視 → daemon 報告を開始。
            startDeviceReporting()
        } catch {
            status = .failed(String(describing: error))
            print("[VPAgent] connect failed: \(error)")
        }
    }

    /// CoreMIDI hot-plug を監視し、 変化を daemon に報告するループを開始する。
    ///
    /// `CoreMIDIWatcher.start()` がまず現在の全 device を `connected` として流し (initial)、
    /// 以降は着脱の差分を流す。 AgentModel は `@MainActor` なのでこの Task も main で走り、
    /// `reportedDevices` の更新は安全 (CoreMIDI notify block も main run loop で配送される)。
    private func startDeviceReporting() {
        devicePump?.cancel()
        reportedDevices = []

        let watcher = CoreMIDIWatcher()
        self.watcher = watcher
        watcher.start()

        devicePump = Task { [weak self] in
            for await change in watcher.changes {
                guard let self else { break }
                await self.client.reportDevice(change)
                if change.isConnected {
                    self.reportedDevices.insert(change.portName)
                } else {
                    self.reportedDevices.remove(change.portName)
                }
            }
        }
    }

    // ─── M2b: 稼働中 SP の一覧・操作 ─────────────────────

    /// 稼働中 SP を再 scan して `instances` を更新する (menu を開いたとき / Refresh から呼ぶ)。
    func refreshInstances() async {
        instances = await InstanceControl.scan()
    }

    /// 指定 SP を graceful shutdown し、 一覧を更新する。
    func stopInstance(port: Int) async {
        await InstanceControl.stop(port: port)
        await refreshInstances()
    }
}
