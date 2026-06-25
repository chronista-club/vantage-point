import Foundation
import UnisonClient

/// menu bar agent の UI 状態を集約する observable model。
///
/// UI に触れるので `@MainActor`。 daemon との実際の I/O は `DaemonClient` (actor) が持ち、
/// ここはその結果 (`Status`) を `@Published` で SwiftUI に流すだけ (data/action 分離)。
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
            case .connected:  return "circle.fill"
            case .failed:     return "exclamationmark.triangle"
            }
        }
    }

    @Published private(set) var status: Status = .connecting

    private let client = DaemonClient()

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
            print("[VPAgent] connected: World \(identity.name) v\(identity.version) "
                + "ns=\(identity.namespace) channels=\(identity.channels)")
        } catch {
            status = .failed(String(describing: error))
            print("[VPAgent] connect failed: \(error)")
        }
    }
}
