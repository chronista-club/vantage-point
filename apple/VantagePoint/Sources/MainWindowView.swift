import SwiftUI

/// メインウィンドウ: NavigationSplitView (Glass Sidebar + Terminal)
///
/// Liquid Glass はサイドバーとツールバーに自動適用される。
/// ターミナル領域は暗い背景のまま — Glass コントロールが浮かぶ構成。
struct MainWindowView: View {
    /// 選択中のプロジェクトパス
    @State private var selectedProjectPath: String?
    /// サイドバーのプロジェクト一覧
    @State private var projects: [SidebarProject] = []
    /// TheWorld 接続ステータス
    @State private var worldStatus: WorldStatus = .checking

    /// 外部から指定されたプロジェクトパス（起動引数・URL スキーム経由）
    var initialProjectPath: String?

    /// TheWorld API クライアント（AppDelegate と共有）
    private let theWorldClient = TheWorldClient.shared

    var body: some View {
        NavigationSplitView {
            SidebarView(projects: projects, selection: $selectedProjectPath, worldStatus: worldStatus)
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Button("Add", systemImage: "plus") {
                            // Phase 2: プロジェクト追加
                        }
                    }
                }
        } detail: {
            ZStack {
                // 全プロジェクトのターミナルを常に保持（PTY セッション維持）
                ForEach(projects) { project in
                    let isActive = selectedProjectPath == project.path
                    TerminalRepresentable(projectPath: project.path, isActive: isActive)
                        .opacity(isActive ? 1 : 0)
                        .allowsHitTesting(isActive)
                }

                // 未選択時のプレースホルダー
                if selectedProjectPath == nil {
                    ContentUnavailableView(
                        "Select a Project",
                        systemImage: "mountain.2",
                        description: Text("Choose a project from the sidebar to start")
                    )
                }
            }
        }
        .onAppear {
            loadProjects()
        }
        .onChange(of: projects) { _, newProjects in
            // @State 更新後に初期選択（onAppear 直後の競合を回避）
            if selectedProjectPath == nil {
                selectedProjectPath = initialProjectPath ?? newProjects.first?.path
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: AppDelegate.selectProjectNotification)) { notification in
            if let path = notification.userInfo?["path"] as? String {
                // プロジェクトが一覧に無ければリロード
                if !projects.contains(where: { $0.path == path }) {
                    loadProjects()
                }
                selectedProjectPath = path
            }
        }
        .task {
            // 定期ポーリング: TheWorld ステータス + プロセス状態
            await pollStatus()
        }
    }

    // MARK: - データ読み込み

    /// config.toml からプロジェクト一覧を読み込む（初期値: 非稼働）
    private func loadProjects() {
        let config = ConfigManager.shared.load()
        projects = config.projects.map { entry in
            SidebarProject(
                id: entry.path,
                name: entry.name,
                path: entry.path,
                isRunning: false,
                port: nil,
                startedAt: nil
            )
        }
    }

    // MARK: - ポーリング

    /// TheWorld ステータス + プロセス状態を定期ポーリング（5秒間隔）
    private func pollStatus() async {
        while !Task.isCancelled {
            await refreshAll()
            try? await Task.sleep(nanoseconds: 5_000_000_000)
        }
    }

    /// TheWorld ヘルス + プロセス一覧を一括更新
    private func refreshAll() async {
        // TheWorld ヘルス
        do {
            let detail = try await theWorldClient.healthDetail()
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            let date = formatter.date(from: detail.startedAt) ?? Date()
            worldStatus = .connected(version: detail.version, startedAt: date)
        } catch {
            worldStatus = .disconnected
            // TheWorld 不在 → 全プロジェクト非稼働
            resetProjectStatus()
            return
        }

        // プロセス一覧取得 → 各プロセスの started_at を health endpoint から取得
        do {
            let running = try await theWorldClient.listRunningProcesses()
            let runningByPath = Dictionary(
                running.map { ($0.projectPath, $0) },
                uniquingKeysWith: { first, _ in first }
            )

            // 各 running process の started_at を並列取得
            let startTimes = await fetchStartTimes(processes: running)

            let config = ConfigManager.shared.load()
            projects = config.projects.map { entry in
                if let process = runningByPath[entry.path] {
                    return SidebarProject(
                        id: entry.path,
                        name: entry.name,
                        path: entry.path,
                        isRunning: true,
                        port: process.port,
                        startedAt: startTimes[entry.path]
                    )
                } else {
                    return SidebarProject(
                        id: entry.path,
                        name: entry.name,
                        path: entry.path,
                        isRunning: false,
                        port: nil,
                        startedAt: nil
                    )
                }
            }
        } catch {
            // プロセス一覧取得失敗 → ステータスだけリセット
            resetProjectStatus()
        }
    }

    /// 各 Process の /api/health から started_at を並列取得
    private nonisolated func fetchStartTimes(processes: [RunningProcess]) async -> [String: Date] {
        await withTaskGroup(of: (String, Date?).self) { group in
            for process in processes {
                group.addTask {
                    do {
                        let client = TheWorldClient(port: process.port)
                        let health = try await client.healthDetail()
                        // ISO8601DateFormatter は Sendable ではないため closure 内で生成
                        let formatter = ISO8601DateFormatter()
                        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
                        let date = formatter.date(from: health.startedAt)
                        return (process.projectPath, date)
                    } catch {
                        return (process.projectPath, nil)
                    }
                }
            }

            var result: [String: Date] = [:]
            for await (path, date) in group {
                if let date { result[path] = date }
            }
            return result
        }
    }

    /// 全プロジェクトを非稼働にリセット
    private func resetProjectStatus() {
        projects = projects.map { project in
            SidebarProject(
                id: project.id,
                name: project.name,
                path: project.path,
                isRunning: false,
                port: nil,
                startedAt: nil
            )
        }
    }
}
