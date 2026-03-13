import Foundation
import Network

/// Discovered Vantage Point service via Bonjour
struct DiscoveredService: Identifiable {
    let id: String
    let name: String
    let host: String
    let port: UInt16
    let project: String?
    let version: String?
}

/// Browses for Vantage Point services on the local network using Bonjour/mDNS
@MainActor
final class BonjourBrowser: ObservableObject {
    /// The Bonjour service type for Vantage Point
    static let serviceType = "_vantage-point._tcp"

    /// Currently discovered services
    @Published private(set) var services: [DiscoveredService] = []

    /// Whether the browser is actively scanning
    @Published private(set) var isScanning: Bool = false

    private var browser: NWBrowser?
    private var pendingResolutions: [String: NWConnection] = [:]

    init() {}

    /// Start browsing for services
    func startBrowsing() {
        guard browser == nil else { return }

        let parameters = NWParameters()
        parameters.includePeerToPeer = true

        let browser = NWBrowser(
            for: .bonjour(type: Self.serviceType, domain: "local."),
            using: parameters
        )

        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in
                self?.handleStateUpdate(state)
            }
        }

        browser.browseResultsChangedHandler = { [weak self] results, changes in
            Task { @MainActor in
                self?.handleResultsChanged(results, changes: changes)
            }
        }

        browser.start(queue: .main)
        self.browser = browser
        isScanning = true

        print("[Bonjour] Started browsing for \(Self.serviceType)")
    }

    /// Stop browsing
    func stopBrowsing() {
        browser?.cancel()
        browser = nil
        isScanning = false

        // Cancel pending resolutions
        for (_, connection) in pendingResolutions {
            connection.cancel()
        }
        pendingResolutions.removeAll()

        print("[Bonjour] Stopped browsing")
    }

    /// Refresh discovered services
    func refresh() {
        stopBrowsing()
        services.removeAll()
        startBrowsing()
    }

    // MARK: - Private

    private func handleStateUpdate(_ state: NWBrowser.State) {
        switch state {
        case .ready:
            print("[Bonjour] Browser ready")
        case let .failed(error):
            print("[Bonjour] Browser failed: \(error)")
            isScanning = false
        case .cancelled:
            print("[Bonjour] Browser cancelled")
            isScanning = false
        case let .waiting(error):
            print("[Bonjour] Browser waiting: \(error)")
        case .setup:
            break
        @unknown default:
            break
        }
    }

    private func handleResultsChanged(_: Set<NWBrowser.Result>, changes: Set<NWBrowser.Result.Change>) {
        for change in changes {
            switch change {
            case let .added(result):
                resolveService(result)
            case let .removed(result):
                removeService(result)
            case .changed(old: _, new: let result, flags: _):
                removeService(result)
                resolveService(result)
            case .identical:
                break
            @unknown default:
                break
            }
        }
    }

    private func resolveService(_ result: NWBrowser.Result) {
        guard case let .service(name, type, domain, _) = result.endpoint else {
            return
        }

        print("[Bonjour] Found service: \(name) (\(type).\(domain))")

        // Create a connection to resolve the service
        let connection = NWConnection(to: result.endpoint, using: .tcp)
        let serviceId = "\(name).\(type).\(domain)"

        pendingResolutions[serviceId] = connection

        connection.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in
                switch state {
                case .ready:
                    if let path = connection.currentPath,
                       let endpoint = path.remoteEndpoint {
                        self?.processResolvedEndpoint(
                            serviceId: serviceId,
                            name: name,
                            endpoint: endpoint,
                            metadata: result.metadata
                        )
                    }
                    connection.cancel()
                    self?.pendingResolutions.removeValue(forKey: serviceId)

                case let .failed(error):
                    print("[Bonjour] Failed to resolve \(name): \(error)")
                    connection.cancel()
                    self?.pendingResolutions.removeValue(forKey: serviceId)

                case .cancelled:
                    self?.pendingResolutions.removeValue(forKey: serviceId)

                default:
                    break
                }
            }
        }

        connection.start(queue: .main)
    }

    private func processResolvedEndpoint(
        serviceId: String,
        name: String,
        endpoint: NWEndpoint,
        metadata: NWBrowser.Result.Metadata
    ) {
        var host = "localhost"
        var port: UInt16 = 0

        if case let .hostPort(resolvedHost, resolvedPort) = endpoint {
            host = resolvedHost.debugDescription
            port = resolvedPort.rawValue
        }

        // Extract TXT record data
        var project: String?
        var version: String?

        if case let .bonjour(txtRecord) = metadata {
            project = txtRecord["project"]
            version = txtRecord["version"]
        }

        let service = DiscoveredService(
            id: serviceId,
            name: name,
            host: host,
            port: port,
            project: project,
            version: version
        )

        // Remove old entry if exists, add new
        services.removeAll { $0.id == serviceId }
        services.append(service)
        services.sort { $0.port < $1.port }

        print("[Bonjour] Resolved: \(name) -> \(host):\(port) (project: \(project ?? "n/a"))")
    }

    private func removeService(_ result: NWBrowser.Result) {
        guard case let .service(name, type, domain, _) = result.endpoint else {
            return
        }

        let serviceId = "\(name).\(type).\(domain)"
        services.removeAll { $0.id == serviceId }
        pendingResolutions[serviceId]?.cancel()
        pendingResolutions.removeValue(forKey: serviceId)

        print("[Bonjour] Service removed: \(name)")
    }

    deinit {
        browser?.cancel()
        for (_, connection) in pendingResolutions {
            connection.cancel()
        }
    }
}

// MARK: - TXT Record Helper

extension NWTXTRecord {
    subscript(key: String) -> String? {
        guard let entry = getEntry(for: key) else { return nil }

        switch entry {
        case let .string(value):
            return value
        case .none, .empty, .data:
            return nil
        @unknown default:
            return nil
        }
    }
}
