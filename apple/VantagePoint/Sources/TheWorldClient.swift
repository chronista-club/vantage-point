import Foundation

// MARK: - TheWorld API Client

/// TheWorld API クライアント
/// Rust 側の ProcessManagerCapability と通信するための Swift クライアント
actor TheWorldClient {
    /// TheWorld のベースURL
    private var baseURL: URL

    /// URLSession
    private let session: URLSession

    /// TheWorld のデフォルトポート
    static let defaultPort: UInt16 = 32000

    /// 共有インスタンス（TheWorld 接続用、AppDelegate + MainWindowView で共有）
    static let shared = TheWorldClient()

    init(host: String = "[::1]", port: UInt16 = TheWorldClient.defaultPort) {
        baseURL = URL(string: "http://\(host):\(port)")!

        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 10
        config.timeoutIntervalForResource = 30
        session = URLSession(configuration: config)
    }

    // MARK: - API Methods

    /// プロジェクト一覧を取得
    func listProjects() async throws -> [ProjectInfo] {
        let url = baseURL.appendingPathComponent("/api/world/projects")
        let resp: ProjectsResponse = try await getAndDecode(url: url)
        return resp.projects
    }

    /// 稼働中Process一覧を取得
    func listRunningProcesses() async throws -> [RunningProcess] {
        let url = baseURL.appendingPathComponent("/api/world/processes")
        let resp: ProcessesResponse = try await getAndDecode(url: url)
        return resp.processes
    }

    /// ccwire セッション一覧を取得
    func listCcwireSessions() async throws -> [CcwireSessionInfo] {
        let url = baseURL.appendingPathComponent("/api/world/ccwire/sessions")
        let resp: CcwireSessionsResponse = try await getAndDecode(url: url)
        return resp.sessions
    }

    /// プロジェクトのProcessを起動
    func startProcess(projectName: String) async throws -> RunningProcess {
        let url = baseURL.appendingPathComponent(
            "/api/world/processes/\(projectName)/start"
        )
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        return try await postAndDecode(request: request)
    }

    /// プロジェクトのProcessを停止
    func stopProcess(projectName: String) async throws {
        try await postWithErrorHandling(
            path: "/api/world/processes/\(projectName)/stop"
        )
    }

    /// プロジェクトのPointViewを開く
    func openPointView(projectName: String) async throws {
        try await postWithErrorHandling(
            path: "/api/world/processes/\(projectName)/pointview"
        )
    }

    /// Canvas Lane を切り替え
    func switchLane(projectName: String) async throws {
        let url = baseURL.appendingPathComponent("/api/canvas/switch_lane")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let body = ["lane": projectName]
        request.httpBody = try JSONEncoder().encode(body)

        try await executeWithErrorHandling(request: request)
    }

    /// プロジェクトを追加
    func addProject(name: String, path: String) async throws {
        let url = baseURL.appendingPathComponent("/api/world/projects")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["name": name, "path": path])
        try await executeWithErrorHandling(request: request)
    }

    /// プロジェクトを削除
    func removeProject(path: String) async throws {
        let url = baseURL.appendingPathComponent("/api/world/projects/remove")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["path": path])
        try await executeWithErrorHandling(request: request)
    }

    /// プロジェクト名を変更
    func updateProject(path: String, name: String) async throws {
        let url = baseURL.appendingPathComponent("/api/world/projects/update")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["path": path, "name": name])
        try await executeWithErrorHandling(request: request)
    }

    /// プロジェクトの enabled/disabled を切り替え
    func setProjectEnabled(path: String, enabled: Bool) async throws {
        let url = baseURL.appendingPathComponent("/api/world/projects/update")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let body: [String: Any] = ["path": path, "enabled": enabled]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        try await executeWithErrorHandling(request: request)
    }

    /// プロジェクトの並び順を変更
    func reorderProjects(paths: [String]) async throws {
        let url = baseURL.appendingPathComponent("/api/world/projects/reorder")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(["paths": paths])
        try await executeWithErrorHandling(request: request)
    }

    /// Process状態をリフレッシュ
    func refreshStatus() async throws {
        try await postWithErrorHandling(path: "/api/world/refresh")
    }

    /// ヘルスチェック
    func healthCheck() async throws -> Bool {
        let url = baseURL.appendingPathComponent("/api/health")

        do {
            let (_, response) = try await session.data(from: url)
            guard let httpResponse = response as? HTTPURLResponse else {
                return false
            }
            return httpResponse.statusCode == 200
        } catch {
            return false
        }
    }

    /// ヘルス詳細取得（バージョン・起動時刻含む）
    func healthDetail() async throws -> WorldHealthDetail {
        let url = baseURL.appendingPathComponent("/api/health")
        return try await getAndDecode(url: url)
    }

    // MARK: - Update API Methods

    /// 更新をチェック
    func checkUpdate() async throws -> UpdateCheckResult {
        let url = baseURL.appendingPathComponent("/api/update/check")
        return try await getAndDecode(url: url)
    }

    /// 更新を適用
    func applyUpdate() async throws -> UpdateApplyResult {
        let url = baseURL.appendingPathComponent("/api/update/apply")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 300 // 5分（ダウンロードに時間がかかる場合）
        return try await postAndDecode(request: request)
    }

    /// ロールバックを実行
    func rollback(backupPath: String) async throws {
        let url = baseURL.appendingPathComponent("/api/update/rollback")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let body = ["backup_path": backupPath]
        request.httpBody = try JSONEncoder().encode(body)

        try await executeWithErrorHandling(request: request)
    }

    /// アプリケーションを再起動
    func restart(
        appPath: String? = nil, delay: UInt32 = 1
    ) async throws -> RestartResult {
        let url = baseURL.appendingPathComponent("/api/update/restart")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        var body: [String: Any] = ["delay": delay]
        if let path = appPath {
            body["app_path"] = path
        }
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        return try await postAndDecode(request: request)
    }

    // MARK: - Mac App Update API Methods

    /// VantagePoint.appの更新をチェック
    func checkMacUpdate(
        currentVersion: String? = nil
    ) async throws -> MacAppUpdateCheckResult {
        let checkURL = baseURL.appendingPathComponent("/api/update/mac/check")
        var urlComponents = URLComponents(
            url: checkURL, resolvingAgainstBaseURL: false
        )!

        let version = currentVersion
            ?? Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String
            ?? "0.0.0"
        urlComponents.queryItems = [
            URLQueryItem(name: "current_version", value: version)
        ]

        guard let url = urlComponents.url else {
            throw TheWorldError.invalidResponse
        }

        return try await getAndDecode(url: url)
    }

    /// VantagePoint.appの更新を適用
    func applyMacUpdate(
        appPath: String? = nil
    ) async throws -> MacAppUpdateApplyResult {
        let url = baseURL.appendingPathComponent("/api/update/mac/apply")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.timeoutInterval = 300

        let path = appPath ?? Bundle.main.bundlePath
        let body = ["app_path": path]
        request.httpBody = try JSONEncoder().encode(body)

        return try await postAndDecode(request: request)
    }

    /// VantagePoint.appをロールバック
    func rollbackMacApp(backupPath: String) async throws {
        let url = baseURL.appendingPathComponent("/api/update/mac/rollback")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let body = ["backup_path": backupPath]
        request.httpBody = try JSONEncoder().encode(body)

        try await executeWithErrorHandling(request: request)
    }

    // MARK: - Lifecycle

    /// 起動中フラグ（二重起動防止）
    private var isStarting: Bool = false

    /// TheWorld が未起動なら自動起動する
    ///
    /// App 起動時に AppDelegate から呼ばれる。
    /// 既に起動中ならすぐに return。未起動なら `vp world start` を spawn して health check で待つ。
    /// actor 内部の `isStarting` フラグで二重起動を防止。
    func ensureRunning() async -> Bool {
        // 既に起動中？
        if (try? await healthCheck()) == true {
            return true
        }

        // 別の呼び出しが既に起動中なら、完了を待つ
        guard !isStarting else {
            for _ in 0 ..< 50 {
                try? await Task.sleep(nanoseconds: 100_000_000)
                if (try? await healthCheck()) == true { return true }
            }
            return false
        }

        isStarting = true
        defer { isStarting = false }

        // vp バイナリを探す
        guard let vpPath = Self.findVpBinary() else {
            return false
        }

        // vp world start を spawn
        let process = Process()
        process.executableURL = URL(fileURLWithPath: vpPath)
        process.arguments = ["world", "start", "--port", String(Self.defaultPort)]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            return false
        }

        // health check ポーリング（最大5秒）
        for _ in 0 ..< 50 {
            try? await Task.sleep(nanoseconds: 100_000_000)
            if (try? await healthCheck()) == true {
                return true
            }
        }

        return false
    }

    /// vp バイナリのパスを探す
    static func findVpBinary() -> String? {
        let candidates = [
            FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".cargo/bin/vp").path,
            "/usr/local/bin/vp",
        ]
        return candidates.first { FileManager.default.fileExists(atPath: $0) }
    }

    // MARK: - Private Helpers

    /// GET して Decodable にデコード
    private func getAndDecode<T: Decodable>(url: URL) async throws -> T {
        let (data, response) = try await session.data(from: url)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw TheWorldError.invalidResponse
        }

        guard httpResponse.statusCode == 200 else {
            if let errResp = try? JSONDecoder().decode(
                TWErrorResponse.self, from: data
            ) {
                throw TheWorldError.serverError(errResp.error)
            }
            throw TheWorldError.httpError(httpResponse.statusCode)
        }

        return try JSONDecoder().decode(T.self, from: data)
    }

    /// POST して Decodable にデコード
    private func postAndDecode<T: Decodable>(
        request: URLRequest
    ) async throws -> T {
        let (data, response) = try await session.data(for: request)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw TheWorldError.invalidResponse
        }

        guard httpResponse.statusCode == 200 else {
            if let errResp = try? JSONDecoder().decode(
                TWErrorResponse.self, from: data
            ) {
                throw TheWorldError.serverError(errResp.error)
            }
            throw TheWorldError.httpError(httpResponse.statusCode)
        }

        return try JSONDecoder().decode(T.self, from: data)
    }

    /// POST してエラーハンドリングのみ
    private func postWithErrorHandling(path: String) async throws {
        let url = baseURL.appendingPathComponent(path)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        try await executeWithErrorHandling(request: request)
    }

    /// リクエストを実行してエラーハンドリングのみ
    private func executeWithErrorHandling(
        request: URLRequest
    ) async throws {
        let (data, response) = try await session.data(for: request)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw TheWorldError.invalidResponse
        }

        guard httpResponse.statusCode == 200 else {
            if let errResp = try? JSONDecoder().decode(
                TWErrorResponse.self, from: data
            ) {
                throw TheWorldError.serverError(errResp.error)
            }
            throw TheWorldError.httpError(httpResponse.statusCode)
        }
    }
}
