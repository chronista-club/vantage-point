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
    /// Canvas（Paisley Park）表示フラグ
    @State private var showCanvas: Bool = false
    /// Canvas の幅（ドラッグで変更可能）
    @State private var canvasWidth: CGFloat = 500
    /// CC 通知バッジ: プロジェクト名 → 未読フラグ
    @State private var notifications: Set<String> = []

    /// 外部から指定されたプロジェクトパス（起動引数・URL スキーム経由）
    var initialProjectPath: String?

    /// TheWorld API クライアント（AppDelegate と共有）
    private let theWorldClient = TheWorldClient.shared

    /// 選択中プロジェクトの SP ポート（Canvas 接続用）
    private var selectedPort: UInt16? {
        guard let path = selectedProjectPath else { return nil }
        return projects.first(where: { $0.path == path })?.port
    }

    var body: some View {
        NavigationSplitView {
            SidebarView(
                projects: projects,
                selection: $selectedProjectPath,
                worldStatus: worldStatus,
                onAdd: addProject,
                onDropAdd: dropAddProject,
                onDelete: deleteProject,
                onRename: renameProject
            )
        } detail: {
            // ターミナル + Canvas（Canvas は Cmd+O でトグル）
            HStack(spacing: 0) {
                // ターミナル（左 — ヘッダー + ビューポート + フッター）
                VStack(spacing: 0) {
                    // ヘッダー: プロジェクト情報 + Stand ステータス
                    terminalHeader

                    // ビューポート: PTY → tmux セッション
                    ZStack {
                        ForEach(projects) { project in
                            let isActive = selectedProjectPath == project.path
                            TerminalRepresentable(projectPath: project.path, isActive: isActive)
                                .opacity(isActive ? 1 : 0)
                                .allowsHitTesting(isActive)
                        }

                        if selectedProjectPath == nil {
                            ContentUnavailableView(
                                "Select a Project",
                                systemImage: "mountain.2",
                                description: Text("Choose a project from the sidebar to start")
                            )
                        }
                    }

                    // フッター: ショートカットヒント
                    terminalFooter
                }

                // Canvas（右）— トグルで表示/非表示、ドラッグで幅変更
                if showCanvas {
                    // ドラッグハンドル（分割線）
                    Rectangle()
                        .fill(Color.gray.opacity(0.01)) // ほぼ透明（ホバー時だけ見える）
                        .frame(width: 6)
                        .contentShape(Rectangle())
                        .onHover { hovering in
                            if hovering {
                                NSCursor.resizeLeftRight.push()
                            } else {
                                NSCursor.pop()
                            }
                        }
                        .gesture(
                            DragGesture()
                                .onChanged { value in
                                    // ドラッグで Canvas 幅を調整（左にドラッグ = 幅拡大）
                                    let newWidth = canvasWidth - value.translation.width
                                    canvasWidth = max(200, min(newWidth, 1200))
                                }
                        )

                    CanvasRepresentable(port: selectedPort)
                        .frame(width: canvasWidth)
                }
            }
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        showCanvas.toggle()
                    } label: {
                        Label(
                            showCanvas ? "Hide Canvas" : "Show Canvas",
                            systemImage: showCanvas ? "sidebar.right" : "sidebar.squares.right"
                        )
                    }
                    .help("Canvas (Paisley Park) の表示/非表示  ⌘O")
                    .keyboardShortcut("o", modifiers: .command)
                }
            }
            .toolbarBackground(.visible, for: .windowToolbar)
            .navigationTitle(selectedProject?.name ?? "Vantage Point")
            .navigationSubtitle(selectedProject != nil ? (selectedProject?.path as NSString?)?.lastPathComponent ?? "" : "")
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
        .onReceive(NotificationCenter.default.publisher(for: .selectPreviousProject)) { _ in
            selectPreviousProject()
        }
        .onReceive(NotificationCenter.default.publisher(for: .selectNextProject)) { _ in
            selectNextProject()
        }
        .onReceive(NotificationCenter.default.publisher(for: .splitTerminalPane)) { _ in
            splitPane()
        }
        .onReceive(NotificationCenter.default.publisher(for: AppDelegate.ccNotification)) { notification in
            if let project = notification.userInfo?["project"] as? String, !project.isEmpty {
                // 現在選択中のプロジェクトでなければバッジを付ける
                let matchingPath = projects.first(where: {
                    $0.name == project || $0.path.hasSuffix("/\(project)")
                })?.path
                if let path = matchingPath, path != selectedProjectPath {
                    notifications.insert(path)
                }
            }
        }
        .onChange(of: selectedProjectPath) { _, newPath in
            // プロジェクト選択時にバッジクリア
            if let path = newPath {
                notifications.remove(path)
            }
        }
    }

    // MARK: - ターミナルヘッダー/フッター

    /// 選択中プロジェクトの情報
    private var selectedProject: SidebarProject? {
        guard let path = selectedProjectPath else { return nil }
        return projects.first(where: { $0.path == path })
    }

    /// ターミナル上部のヘッダー（プロジェクト名 + Stand ステータス）
    @ViewBuilder
    private var terminalHeader: some View {
        if let project = selectedProject {
            HStack(spacing: 8) {
                Text(project.name)
                    .fontWeight(.medium)

                if project.isRunning {
                    // Stand アイコン
                    let activeStands = project.stands.filter { $0.status != "disabled" }
                    ForEach(activeStands, id: \.key) { stand in
                        HStack(spacing: 2) {
                            Image(systemName: stand.systemImage)
                            Text(stand.shortName)
                        }
                        .foregroundStyle(stand.statusColor)
                    }
                }

                Spacer()

                if let startedAt = project.startedAt {
                    Text(startedAt, style: .time)
                        .foregroundStyle(.tertiary)
                }
            }
            .font(.caption)
            .foregroundStyle(.white)
            .padding(.horizontal, 12)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(white: 0.15))
        }
    }

    /// ターミナル下部のフッター（ショートカットヒント）
    @ViewBuilder
    private var terminalFooter: some View {
        if selectedProject != nil {
            HStack(spacing: 16) {
                shortcutHint("⌘O", "Canvas")
                shortcutHint("⌘↑↓", "Project")
                shortcutHint("⌘D", "Split")
            }
            .font(.caption2)
            .foregroundStyle(.gray)
            .padding(.horizontal, 12)
            .padding(.vertical, 4)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(white: 0.15))
        }
    }

    /// ショートカットヒントの表示ヘルパー
    private func shortcutHint(_ key: String, _ label: String) -> some View {
        HStack(spacing: 3) {
            Text(key)
                .fontWeight(.medium)
                .foregroundStyle(.secondary)
            Text(label)
        }
    }

    // MARK: - tmux ペイン操作

    /// tmux ペインを分割（⌘D）
    private func splitPane() {
        guard let port = selectedPort else { return }
        Task {
            let url = URL(string: "http://[::1]:\(port)/api/tmux/split")!
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try? JSONSerialization.data(withJSONObject: ["horizontal": true])
            request.timeoutInterval = 5
            _ = try? await URLSession.shared.data(for: request)
        }
    }

    // MARK: - プロジェクト選択ナビゲーション

    /// 前のプロジェクトを選択（⌘↑）
    private func selectPreviousProject() {
        guard !projects.isEmpty else { return }
        guard let current = selectedProjectPath,
              let index = projects.firstIndex(where: { $0.path == current }),
              index > 0 else {
            selectedProjectPath = projects.last?.path
            return
        }
        selectedProjectPath = projects[index - 1].path
    }

    /// 次のプロジェクトを選択（⌘↓）
    private func selectNextProject() {
        guard !projects.isEmpty else { return }
        guard let current = selectedProjectPath,
              let index = projects.firstIndex(where: { $0.path == current }),
              index < projects.count - 1 else {
            selectedProjectPath = projects.first?.path
            return
        }
        selectedProjectPath = projects[index + 1].path
    }

    // MARK: - プロジェクト CRUD

    /// config.toml を保存（失敗時はログ出力）
    private func saveConfig(_ config: ConfigManager.VpConfig) {
        do {
            try ConfigManager.shared.save(config)
        } catch {
            NSLog("[VP] config.toml 保存失敗: %@", error.localizedDescription)
        }
    }

    /// フォルダ選択ダイアログでプロジェクトを追加
    private func addProject() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "プロジェクトフォルダを選択"
        panel.prompt = "追加"

        guard panel.runModal() == .OK, let url = panel.url else { return }

        var config = ConfigManager.shared.load()
        let path = url.path
        // 重複チェック
        guard !config.projects.contains(where: { $0.path == path }) else { return }
        config.projects.append(
            ConfigManager.ProjectEntry(name: url.lastPathComponent, path: path)
        )
        saveConfig(config)
        loadProjects()
    }

    /// ドラッグ＆ドロップでプロジェクトを追加（URL 指定）
    private func dropAddProject(url: URL) {
        var config = ConfigManager.shared.load()
        let path = url.path
        guard !config.projects.contains(where: { $0.path == path }) else { return }
        config.projects.append(
            ConfigManager.ProjectEntry(name: url.lastPathComponent, path: path)
        )
        saveConfig(config)
        loadProjects()
    }

    /// プロジェクトをリストから削除（config.toml から除去）
    private func deleteProject(path: String) {
        var config = ConfigManager.shared.load()
        config.projects.removeAll { $0.path == path }
        saveConfig(config)
        loadProjects()
    }

    /// プロジェクト名を変更
    private func renameProject(path: String, newName: String) {
        var config = ConfigManager.shared.load()
        if let index = config.projects.firstIndex(where: { $0.path == path }) {
            config.projects[index] = ConfigManager.ProjectEntry(
                name: newName,
                path: config.projects[index].path,
                port: config.projects[index].port
            )
        }
        saveConfig(config)
        loadProjects()
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
                startedAt: nil,
                hasNotification: notifications.contains(entry.path)
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

            // 各 running process の started_at + stands を並列取得
            let details = await fetchProcessDetails(processes: running)

            let config = ConfigManager.shared.load()
            projects = config.projects.map { entry in
                if let process = runningByPath[entry.path] {
                    let detail = details[entry.path]
                    return SidebarProject(
                        id: entry.path,
                        name: entry.name,
                        path: entry.path,
                        isRunning: true,
                        port: process.port,
                        startedAt: detail?.startedAt,
                        stands: detail?.stands ?? [],
                        hasNotification: notifications.contains(entry.path)
                    )
                } else {
                    return SidebarProject(
                        id: entry.path,
                        name: entry.name,
                        path: entry.path,
                        isRunning: false,
                        port: nil,
                        startedAt: nil,
                        hasNotification: notifications.contains(entry.path)
                    )
                }
            }
        } catch {
            // プロセス一覧取得失敗 → ステータスだけリセット
            resetProjectStatus()
        }
    }

    /// SP の health から取得した詳細情報
    struct ProcessDetail {
        let startedAt: Date?
        let stands: [SidebarStand]
    }

    /// 各 Process の /api/health から started_at + stands を並列取得
    private nonisolated func fetchProcessDetails(processes: [RunningProcess]) async -> [String: ProcessDetail] {
        await withTaskGroup(of: (String, ProcessDetail).self) { group in
            for process in processes {
                group.addTask {
                    do {
                        let client = TheWorldClient(port: process.port)
                        let health = try await client.healthDetail()
                        // ISO8601DateFormatter は Sendable ではないため closure 内で生成
                        let formatter = ISO8601DateFormatter()
                        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
                        let date = formatter.date(from: health.startedAt)

                        // Stand ステータスを変換
                        let stands: [SidebarStand] = (health.stands ?? [:]).map { key, value in
                            SidebarStand(key: key, status: value.status, detail: value.detail)
                        }.sorted { $0.key < $1.key }

                        return (process.projectPath, ProcessDetail(startedAt: date, stands: stands))
                    } catch {
                        return (process.projectPath, ProcessDetail(startedAt: nil, stands: []))
                    }
                }
            }

            var result: [String: ProcessDetail] = [:]
            for await (path, detail) in group {
                result[path] = detail
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
                startedAt: nil,
                hasNotification: notifications.contains(project.path)
            )
        }
    }
}

// MARK: - Notification Names

extension Notification.Name {
    static let selectPreviousProject = Notification.Name("VP.selectPreviousProject")
    static let selectNextProject = Notification.Name("VP.selectNextProject")
    static let splitTerminalPane = Notification.Name("VP.splitTerminalPane")
}
