import AppKit
import Combine
import Foundation

/// プロジェクト表示用モデル
struct ProjectItem: Identifiable {
    let id: String
    let name: String
    let path: String
    var status: ProcessStatus

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
    case connected
    case disconnected
    case starting
}

/// Popover の状態管理
@MainActor
class PopoverViewModel: ObservableObject {
    @Published var projects: [ProjectItem] = []
    @Published var theWorldState: TheWorldConnectionState = .disconnected
    @Published var isRestartingAll: Bool = false
    @Published var isRestartingTheWorld: Bool = false
    @Published var errorMessage: String?

    private let theWorldClient: TheWorldClient
    private var refreshTimer: Timer?
    /// deinit からアクセスするため nonisolated(unsafe)
    /// セットアップは init（MainActor）、解除は deinit のみ — 競合なし
    nonisolated(unsafe) private var processChangedObserver: NSObjectProtocol?

    /// DistributedNotification 通知名
    static let processChangedNotification = "tech.anycreative.vp.process.changed"

    init(theWorldClient: TheWorldClient) {
        self.theWorldClient = theWorldClient
        setupDistributedNotificationObserver()
    }

    private func setupDistributedNotificationObserver() {
        processChangedObserver = DistributedNotificationCenter.default().addObserver(
            forName: NSNotification.Name(Self.processChangedNotification),
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                await self?.refresh()
            }
        }
    }

    deinit {
        // deinit は nonisolated — MainActor 外から呼ばれる可能性がある
        let observer = processChangedObserver
        DispatchQueue.main.async {
            if let observer {
                DistributedNotificationCenter.default().removeObserver(observer)
            }
        }
    }

    // MARK: - Refresh

    func refresh() async {
        do {
            async let projectsResult = theWorldClient.listProjects()
            async let processesResult = theWorldClient.listRunningProcesses()

            let registeredProjects = try await projectsResult
            let runningProcesses = try await processesResult

            theWorldState = .connected

            var items: [ProjectItem] = []

            for project in registeredProjects {
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
                    status: status
                ))
            }

            // 登録されてないが稼働中のプロセス
            for process in runningProcesses
                where !items.contains(where: { $0.name == process.projectName }) {
                items.append(ProjectItem(
                    id: process.projectName,
                    name: process.projectName,
                    path: process.projectPath,
                    status: .running
                ))
            }

            projects = items
            errorMessage = nil

        } catch {
            theWorldState = .disconnected
            projects = []
            errorMessage = nil
        }
    }

    // MARK: - Restart Actions

    /// TheWorld + 全 Process を再起動
    func restartAll() async {
        isRestartingAll = true
        defer { isRestartingAll = false }

        await restartTheWorld()
    }

    /// TheWorld を再起動（全 Process も自動的に再起動される）
    func restartTheWorld() async {
        isRestartingTheWorld = true
        defer { isRestartingTheWorld = false }

        // PIDファイルから TheWorld を停止
        let pidPath = FileManager.default.temporaryDirectory
            .appendingPathComponent("vantage-point/daemon.pid")

        let pidContent = try? String(contentsOf: pidPath, encoding: .utf8)
        if let pidString = pidContent?.trimmingCharacters(in: .whitespacesAndNewlines),
           let pid = Int32(pidString) {
            kill(pid, SIGTERM)
            try? await Task.sleep(nanoseconds: 1_500_000_000)
        }

        theWorldState = .disconnected

        // 再起動
        await startTheWorld()
    }

    /// 個別 Process を再起動（stop → start）
    func restartProcess(projectName: String) async {
        guard let idx = projects.firstIndex(where: { $0.name == projectName }) else { return }

        let wasRunning = projects[idx].status == .running

        if wasRunning {
            projects[idx].status = .stopping
            do {
                try await theWorldClient.stopProcess(projectName: projectName)
            } catch {
                errorMessage = error.localizedDescription
                return
            }
            // 停止を待つ
            try? await Task.sleep(nanoseconds: 1_000_000_000)
        }

        projects[idx].status = .starting
        do {
            _ = try await theWorldClient.startProcess(projectName: projectName)
            projects[idx].status = .running
        } catch {
            projects[idx].status = .error
            errorMessage = error.localizedDescription
        }
    }

    /// VP ウィンドウを開く（AppDelegate が設定するコールバック経由）
    var onOpenMainWindow: ((String) -> Void)?

    /// ウィンドウを開き、該当 Lane にフォーカス
    ///
    /// SP が未起動の場合は自動起動してからウィンドウを表示する。
    func openWindow(projectName: String, projectPath: String) {
        onOpenMainWindow?(projectPath)

        Task {
            // SP が未起動なら自動起動
            let running = (try? await theWorldClient.listRunningProcesses()) ?? []
            let isRunning = running.contains { $0.projectPath == projectPath }
            if !isRunning {
                _ = try? await theWorldClient.startProcess(projectName: projectName)
                // SP 起動を待つ
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }

            // Canvas Lane 切り替え
            try? await Task.sleep(nanoseconds: 300_000_000)
            try? await theWorldClient.switchLane(projectName: projectName)
        }
    }

    /// アプリ自体を再起動
    func restartApp() {
        let executablePath = ProcessInfo.processInfo.arguments[0]

        // 新しいプロセスを起動
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executablePath)
        process.arguments = Array(ProcessInfo.processInfo.arguments.dropFirst())
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            NSLog("[VP] Failed to restart app: %@", error.localizedDescription)
            return
        }

        // 現プロセスを終了
        NSApp.terminate(nil)
    }

    // MARK: - TheWorld Lifecycle

    private func startTheWorld() async {
        theWorldState = .starting

        // vp バイナリが見つからなければ早期リターン
        if TheWorldClient.findVpBinary() == nil {
            theWorldState = .disconnected
            errorMessage = "vp command not found (~/.cargo/bin/vp)"
            return
        }

        let started = await theWorldClient.ensureRunning()
        if started {
            theWorldState = .connected
            await refresh()
        } else {
            theWorldState = .disconnected
            errorMessage = "TheWorld startup timed out"
        }
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
