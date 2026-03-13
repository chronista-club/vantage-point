import AppKit
import Combine
import Foundation

/// プロジェクト表示用モデル
struct ProjectItem: Identifiable {
    let id: String
    let name: String
    let path: String
    var status: ProcessStatus
    var port: UInt16?
    var pid: UInt32?
    var version: String?

    /// 最終アクティビティからの経過表示
    var lastActivity: String?

    enum ProcessStatus: String {
        case stopped
        case starting
        case running
        case stopping
        case error
    }
}

/// TheWorld の接続状態
enum TheWorldConnectionState {
    /// 接続中（正常）
    case connected
    /// 未起動（vp world が動いていない）
    case disconnected
    /// 起動試行中
    case starting
}

/// Popover の状態管理
@MainActor
class PopoverViewModel: ObservableObject {
    /// 全プロジェクト（登録済み + 稼働中）
    @Published var projects: [ProjectItem] = []

    /// ローディング中
    @Published var isLoading: Bool = false

    /// エラーメッセージ
    @Published var errorMessage: String?

    /// TheWorld の接続状態
    @Published var theWorldState: TheWorldConnectionState = .disconnected

    private let theWorldClient: TheWorldClient
    private var refreshTimer: Timer?

    /// DistributedNotification 通知名
    static let processChangedNotification = "club.chronista.vp.process.changed"

    init(theWorldClient: TheWorldClient) {
        self.theWorldClient = theWorldClient
        setupDistributedNotificationObserver()
    }

    /// VP プロセスからの DistributedNotification を監視
    private func setupDistributedNotificationObserver() {
        DistributedNotificationCenter.default().addObserver(
            forName: NSNotification.Name(Self.processChangedNotification),
            object: nil,
            queue: .main
        ) { [weak self] notification in
            let event = notification.object as? String ?? "unknown"
            NSLog("[VP] DistributedNotification received: %@", event)
            Task { @MainActor [weak self] in
                await self?.refresh()
            }
        }
    }

    /// データを取得・更新
    func refresh() async {
        isLoading = projects.isEmpty // 初回のみローディング表示

        do {
            // TheWorld 経由で全プロジェクト + 稼働中プロセスを取得
            async let projectsResult = theWorldClient.listProjects()
            async let processesResult = theWorldClient.listRunningProcesses()

            let registeredProjects = try await projectsResult
            let runningProcesses = try await processesResult

            theWorldState = .connected

            // マージ: 登録プロジェクトベースで、稼働中情報を重ねる
            var items: [ProjectItem] = []

            for project in registeredProjects {
                let running = runningProcesses.first { $0.projectName == project.name }

                let status: ProjectItem.ProcessStatus = switch project.processStatus {
                case .running: .running
                case .starting: .starting
                case .stopping: .stopping
                case .error: .error
                default: .stopped
                }

                items.append(ProjectItem(
                    id: project.name,
                    name: project.name,
                    path: project.path,
                    status: status,
                    port: running?.port,
                    pid: running?.pid
                ))
            }

            // 登録されてないが稼働中のプロセス（直接 vp start したもの）
            for process in runningProcesses
                where !items.contains(where: { $0.name == process.projectName }) {
                items.append(ProjectItem(
                    id: process.projectName,
                    name: process.projectName,
                    path: process.projectPath,
                    status: .running,
                    port: process.port,
                    pid: process.pid
                ))
            }

            projects = items
            errorMessage = nil

        } catch {
            // TheWorld 未起動
            theWorldState = .disconnected
            projects = []
            errorMessage = nil
        }

        isLoading = false
    }

    // MARK: - TheWorld ライフサイクル

    /// TheWorld（vp world）を起動
    func startTheWorld() async {
        guard let vpPath = findVpBinary() else {
            errorMessage = "vp command not found"
            return
        }

        theWorldState = .starting

        let process = Process()
        process.executableURL = URL(fileURLWithPath: vpPath)
        process.arguments = ["world", "start", "--port", String(TheWorldClient.defaultPort)]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
            // 起動を待つ（最大5秒）
            for _ in 0 ..< 50 {
                try? await Task.sleep(nanoseconds: 100_000_000) // 100ms
                let healthy = try? await theWorldClient.healthCheck()
                if healthy == true {
                    theWorldState = .connected
                    await refresh()
                    return
                }
            }
            // タイムアウト
            theWorldState = .disconnected
            errorMessage = "TheWorld startup timed out"
        } catch {
            theWorldState = .disconnected
            errorMessage = "Failed to start TheWorld: \(error.localizedDescription)"
        }
    }

    /// TheWorld を再起動（停止 → 起動）
    func restartTheWorld() async {
        // PIDファイルからTheWorldのPIDを取得して停止
        let pidPath = FileManager.default.temporaryDirectory
            .appendingPathComponent("vantage-point/daemon.pid")

        let pidContent = try? String(contentsOf: pidPath, encoding: .utf8)
        if let pidString = pidContent?.trimmingCharacters(in: .whitespacesAndNewlines),
           let pid = Int32(pidString) {
            // SIGTERM 送信
            kill(pid, SIGTERM)
            // 停止を待つ
            try? await Task.sleep(nanoseconds: 1_000_000_000) // 1秒
        }

        theWorldState = .disconnected

        // 再起動
        await startTheWorld()
    }

    // MARK: - Process Actions

    /// Process を起動
    func startProcess(projectName: String) async {
        if let idx = projects.firstIndex(where: { $0.name == projectName }) {
            projects[idx].status = .starting
        }

        do {
            let result = try await theWorldClient.startProcess(projectName: projectName)
            if let idx = projects.firstIndex(where: { $0.name == projectName }) {
                projects[idx].status = .running
                projects[idx].port = result.port
                projects[idx].pid = result.pid
            }
        } catch {
            if let idx = projects.firstIndex(where: { $0.name == projectName }) {
                projects[idx].status = .error
            }
            errorMessage = error.localizedDescription
        }
    }

    /// Process を停止
    func stopProcess(projectName: String) async {
        if let idx = projects.firstIndex(where: { $0.name == projectName }) {
            projects[idx].status = .stopping
        }

        do {
            try await theWorldClient.stopProcess(projectName: projectName)
            if let idx = projects.firstIndex(where: { $0.name == projectName }) {
                projects[idx].status = .stopped
                projects[idx].port = nil
                projects[idx].pid = nil
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// PP Window を開く（TheWorld 経由の PointView API）
    func openPointView(projectName: String) async {
        do {
            try await theWorldClient.openPointView(projectName: projectName)
        } catch {
            errorMessage = "Failed to open Canvas: \(error.localizedDescription)"
        }
    }

    /// WebUI をブラウザで開く
    func openWebUI(port: UInt16) {
        let url = URL(string: "http://localhost:\(String(port))")!
        NSWorkspace.shared.open(url)
    }

    /// メインウィンドウ表示コールバック（AppDelegate が設定）
    var onOpenMainWindow: ((String) -> Void)?

    /// VP ネイティブウィンドウを開く
    func openTUI(projectPath: String) {
        if let handler = onOpenMainWindow {
            handler(projectPath)
        } else {
            // フォールバック: 外部プロセス起動（旧動作）
            guard let vpPath = findVpBinary() else {
                errorMessage = "vp command not found"
                return
            }

            let process = Process()
            process.executableURL = URL(fileURLWithPath: vpPath)
            process.arguments = ["start", "--gui", "-C", projectPath]
            process.standardOutput = FileHandle.nullDevice
            process.standardError = FileHandle.nullDevice

            do {
                try process.run()
            } catch {
                errorMessage = "Failed to open TUI: \(error.localizedDescription)"
            }
        }
    }

    // MARK: - Helpers

    private func findVpBinary() -> String? {
        let candidates = [
            FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".cargo/bin/vp").path,
            "/usr/local/bin/vp"
        ]

        return candidates.first { FileManager.default.fileExists(atPath: $0) }
    }

    // MARK: - Auto Refresh

    func startAutoRefresh(interval: TimeInterval = 5.0) {
        stopAutoRefresh()
        refreshTimer = Timer.scheduledTimer(withTimeInterval: interval, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                await self?.refresh()
            }
        }
    }

    func stopAutoRefresh() {
        refreshTimer?.invalidate()
        refreshTimer = nil
    }
}
