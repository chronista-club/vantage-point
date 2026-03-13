import Foundation

/// Process instance information (legacy port-scan fallback)
struct ProcessInstance: Identifiable {
    let id: UInt16
    let port: UInt16
    let pid: Int
    let projectDir: String?

    var projectName: String? {
        projectDir?.split(separator: "/").last.map(String.init)
    }
}

/// Manages Vantage Point Process instances via direct port scanning
/// PopoverViewModel が TheWorld 経由で管理するため、通常は使用されない。
/// TheWorld 不在時のフォールバックとして残す。
actor ProcessManager {
    private let session: URLSession

    init() {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 0.5
        config.timeoutIntervalForResource = 1.0
        session = URLSession(configuration: config)
    }

    /// Scan for running Process instances on ports 33000-33010
    func scanInstances() async -> [ProcessInstance] {
        var found: [ProcessInstance] = []

        await withTaskGroup(of: ProcessInstance?.self) { group in
            for port in UInt16(33000) ... UInt16(33010) {
                group.addTask {
                    await self.checkPort(port)
                }
            }

            for await instance in group {
                if let instance {
                    found.append(instance)
                }
            }
        }

        return found.sorted { $0.port < $1.port }
    }

    /// Check if a Process is running on the given port
    private func checkPort(_ port: UInt16) async -> ProcessInstance? {
        let url = URL(string: "http://[::1]:\(port)/api/health")!

        do {
            let (data, response) = try await session.data(from: url)

            guard let httpResponse = response as? HTTPURLResponse,
                  httpResponse.statusCode == 200
            else {
                return nil
            }

            let json = try JSONSerialization.jsonObject(with: data) as? [String: Any]
            let pid = json?["pid"] as? Int ?? 0
            let projectDir = json?["project_dir"] as? String

            return ProcessInstance(
                id: port,
                port: port,
                pid: pid,
                projectDir: projectDir
            )
        } catch {
            return nil
        }
    }

    /// Stop a Process instance
    func stopInstance(port: UInt16) async {
        let url = URL(string: "http://[::1]:\(port)/api/shutdown")!
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 2.0

        _ = try? await session.data(for: request)
    }

    /// Start a new Process instance
    func startProcess(projectDir: String? = nil) async throws {
        let vpPath = findVpBinary()

        guard let vpPath else {
            throw ProcessError.vpNotFound
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: vpPath)
        process.arguments = ["start", "--headless"]

        if let projectDir {
            process.arguments?.append(contentsOf: ["-C", projectDir])
        }

        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        try process.run()
    }

    /// Find the vp binary
    private nonisolated func findVpBinary() -> String? {
        let cargoPath = NSHomeDirectory() + "/.cargo/bin/vp"
        if FileManager.default.fileExists(atPath: cargoPath) {
            return cargoPath
        }

        let usrLocalPath = "/usr/local/bin/vp"
        if FileManager.default.fileExists(atPath: usrLocalPath) {
            return usrLocalPath
        }

        let whichProcess = Process()
        whichProcess.executableURL = URL(fileURLWithPath: "/usr/bin/which")
        whichProcess.arguments = ["vp"]

        let pipe = Pipe()
        whichProcess.standardOutput = pipe

        do {
            try whichProcess.run()
            whichProcess.waitUntilExit()

            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)

            if let output, !output.isEmpty {
                return output
            }
        } catch {}

        return nil
    }
}

enum ProcessError: LocalizedError {
    case vpNotFound

    var errorDescription: String? {
        switch self {
        case .vpNotFound:
            "vp command not found. Please install it first:\n\ncargo install --path crates/vantage-point"
        }
    }
}
