import OSLog
import SwiftUI

private let logger = Logger(subsystem: "tech.anycreative.vp", category: "MainWindow")

/// メインウィンドウ: NavigationSplitView (Glass Sidebar + Terminal)
///
/// Liquid Glass はサイドバーとツールバーに自動適用される。
/// ターミナル領域は暗い背景のまま — Glass コントロールが浮かぶ構成。
/// Pane Split Navigator のステップ状態
enum SplitNavigatorStep: Equatable {
    case hidden
    case direction(selected: Int)   // 0=右, 1=下, 2=上, 3=左
    case content(horizontal: Bool, selected: Int)  // 0=The Hand, 1=PP, 2=HD
}

/// 分割方向の定義
struct SplitDirection: Identifiable {
    let id: Int
    let label: String
    let symbol: String
    let horizontal: Bool  // tmux の horizontal パラメータ
}

/// コンテンツ種別の定義
struct SplitContent: Identifiable {
    let id: Int
    let label: String
    let emoji: String
    let contentType: String
}

/// 分割方向の一覧（右, 下, 上, 左）
let splitDirections: [SplitDirection] = [
    SplitDirection(id: 0, label: "右", symbol: "→", horizontal: true),
    SplitDirection(id: 1, label: "下", symbol: "↓", horizontal: false),
    SplitDirection(id: 2, label: "上", symbol: "↑", horizontal: false),
    SplitDirection(id: 3, label: "左", symbol: "←", horizontal: true),
]

/// コンテンツ種別の一覧（The Hand, Paisley Park, Heaven's Door）
let splitContents: [SplitContent] = [
    SplitContent(id: 0, label: "The Hand", emoji: "✋", contentType: "shell"),
    SplitContent(id: 1, label: "Paisley Park", emoji: "🧭", contentType: "pp"),
    SplitContent(id: 2, label: "Heaven's Door", emoji: "📖", contentType: "agent"),
]

struct MainWindowView: View {
    /// 選択中のプロジェクトパス
    @State private var selectedProjectPath: String?
    /// サイドバーのプロジェクト一覧
    @State private var projects: [SidebarProject] = []
    /// TheWorld 接続ステータス
    @State private var worldStatus: WorldStatus = .checking
    /// CC 通知バッジ: プロジェクト名 → 未読フラグ
    @State private var notifications: Set<String> = []
    /// ターミナルターゲットのパス一覧（プロジェクト + worker）
    ///
    /// computed property だとポーリングのたびに再計算 → ForEach が
    /// TerminalRepresentable を再生成 → PTY 再起動してしまう。
    /// @State で保持し、差分がある場合のみ更新する。
    @State private var terminalPaths: [String] = []
    /// TerminalRepresentable の強制再生成用カウンタ（HD リスタート時にインクリメント）
    @State private var terminalGeneration: [String: Int] = [:]
    /// HD 自動起動を試みたパス（ポーリングで繰り返し起動しないため）
    @State private var hdAutoStartAttempted: Set<String> = []
    /// SP 自動起動を試みたパス（ポーリングで繰り返し起動しないため）
    @State private var spAutoStartAttempted: Set<String> = []
    /// Pane Split Navigator の状態
    @State private var splitNavigator: SplitNavigatorStep = .hidden
    /// VP Pane レイアウト: プロジェクトパス → ペインツリー
    @State private var paneLayouts: [String: VPPaneLayout] = [:]
    /// 退避中のペイン: プロジェクトパス → 退避ペインリスト (VP-49)
    @State private var minimizedPanes: [String: [MinimizedPane]] = [:]
    /// サイドバー表示状態
    @State private var sidebarVisible: Bool = true
    /// ProjectTabBar の手動表示設定（true = 常時表示）
    @State private var projectTabBarForced: Bool = false

    /// ProjectTabBar を表示するか（サイドバー非表示時は自動表示、手動トグルで常時表示）
    private var showProjectTabBar: Bool {
        projectTabBarForced || !sidebarVisible
    }

    /// サイドバー幅（固定）
    private let sidebarWidth: CGFloat = 240

    /// 外部から指定されたプロジェクトパス（起動引数・URL スキーム経由）
    var initialProjectPath: String?

    /// TheWorld API クライアント（AppDelegate と共有）
    private let theWorldClient = TheWorldClient.shared

    /// 選択中プロジェクトの SP ポート（Canvas 接続用）
    ///
    /// Worker 選択時は親プロジェクトのポートを返す（Worker は独自の SP を持たない）。
    private var selectedPort: UInt16? {
        selectedProject?.port
    }

    /// enabled プロジェクト一覧
    private var enabledProjects: [SidebarProject] {
        projects.filter { $0.enabled }
    }

    /// フォーカス中プロジェクトの Lane 一覧（lead + workers）
    private var currentLanes: [Lane] {
        guard let project = selectedProject else { return [] }
        var lanes: [Lane] = [
            Lane(path: project.path, label: "Lead-HD", isLead: true)
        ]
        for worker in project.workers {
            lanes.append(Lane(path: worker.path, label: worker.suffix, isLead: false))
        }
        return lanes
    }

    var body: some View {
        HStack(spacing: 0) {
            // カスタムサイドバー（半透明 Material 背景）
            if sidebarVisible {
                SidebarView(
                    projects: projects,
                    selection: $selectedProjectPath,
                    worldStatus: worldStatus,
                    onAdd: addProject,
                    onDropAdd: dropAddProject,
                    onDelete: deleteProject,
                    onRename: renameProject,
                    onReorder: reorderProjects,
                    onRestartHD: restartHD,
                    onRestartSP: restartSP,
                    onRestartWorld: restartWorld,
                    onToggleEnabled: toggleProjectEnabled,
                    notifications: notifications
                )
                .frame(width: sidebarWidth)
                .background(VisualEffectBackground(material: .sidebar, blendingMode: .behindWindow))
                .transition(.move(edge: .leading))

                Divider()
            }

            // メインエリア（ターミナル + タブ）
            VStack(spacing: 0) {
                // Project Tab バー — サイドバー非表示時 or 手動トグルで表示
                if showProjectTabBar {
                    ProjectTabBar(
                        projects: enabledProjects,
                        selectedPath: selectedProject?.path,
                        onSelect: { path in
                            if selectedProjectPath != path {
                                selectedProjectPath = path
                            }
                        }
                    )
                }

                // Lane Tab バー — フォーカス中プロジェクトの Lane 切替 (VP-51)
                if currentLanes.count > 1 {
                    LaneTabBar(
                        lanes: currentLanes,
                        selectedPath: selectedProjectPath,
                        onSelect: { path in selectedProjectPath = path }
                    )
                }

                // ビューポート: VP Pane コンテナ（NSView レイヤの分割管理）
                // プロジェクト + worker それぞれ独立した VP Pane ツリーを持つ
                ZStack {
                        ForEach(terminalPaths, id: \.self) { path in
                            let isActive = selectedProjectPath == path
                            let gen = terminalGeneration[path] ?? 0
                            let layout = paneLayouts[path] ?? VPPaneLayout.initial()
                            VPPaneContainer(
                                projectPath: path,
                                node: layout.root.withFocus(on: layout.focusedPaneId),
                                isActive: isActive,
                                splitNavigatorActive: splitNavigator != .hidden,
                                terminalGeneration: gen,
                                port: selectedPort,
                                onMinimizePane: { paneId in
                                    minimizePane(path: path, paneId: paneId)
                                },
                                onClosePane: { paneId in
                                    closePane(path: path, paneId: paneId)
                                }
                            )
                                .id("\(path):\(gen)")
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

                        // Pane Split Navigator フッター
                        if splitNavigator != .hidden {
                            VStack {
                                Spacer()
                                splitNavigatorFooter
                            }
                            .transition(.move(edge: .bottom).combined(with: .opacity))
                            .animation(.easeInOut(duration: 0.15), value: splitNavigator)
                        }
                    }

                    // Pane Dock — 退避ペインのアイコンバー (VP-49)
                    if let path = selectedProjectPath,
                       let docked = minimizedPanes[path],
                       !docked.isEmpty {
                        PaneDock(minimizedPanes: docked) { pane in
                            restorePane(path: path, pane: pane)
                        }
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                    }

            }
        }
        .animation(.easeInOut(duration: 0.2), value: sidebarVisible)
        .onAppear {
            loadProjects()
        }
        .onChange(of: projects) { _, newProjects in
            // @State 更新後に初期選択（onAppear 直後の競合を回避）
            if selectedProjectPath == nil {
                selectedProjectPath = initialProjectPath ?? newProjects.first?.path
            }
            // ターミナルパス一覧を差分更新（不要な TerminalRepresentable 再生成を防ぐ）
            let newPaths = Self.buildTerminalPaths(from: newProjects)
            if newPaths != terminalPaths {
                terminalPaths = newPaths
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
            // Cmd+D: ナビゲーター展開（トグル）
            withAnimation(.easeInOut(duration: 0.15)) {
                if splitNavigator == .hidden {
                    splitNavigator = .direction(selected: 0)
                } else {
                    splitNavigator = .hidden
                }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .closeTerminalPane)) { _ in
            closePane()
        }
        .onReceive(NotificationCenter.default.publisher(for: .splitNavigatorKey)) { notification in
            handleSplitNavigatorKey(notification)
        }
        .onReceive(NotificationCenter.default.publisher(for: .selectProjectByNumber)) { notification in
            if let number = notification.userInfo?["number"] as? Int {
                selectProjectByNumber(number)
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .selectLaneByNumber)) { notification in
            if let number = notification.userInfo?["number"] as? Int {
                selectLaneByNumber(number)
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: AppDelegate.ccNotification)) { notification in
            if let project = notification.userInfo?["project"] as? String, !project.isEmpty {
                // path が指定されていればそのまま使用（Lane 単位通知）
                let notifPath = notification.userInfo?["path"] as? String ?? ""
                let lanePath: String?
                if !notifPath.isEmpty {
                    lanePath = notifPath
                } else {
                    // path 未指定: プロジェクト名からプロジェクトパスを解決
                    lanePath = projects.first(where: {
                        $0.name == project || $0.path.hasSuffix("/\(project)")
                    })?.path
                }
                // 現在選択中の Lane でなければバッジを付ける
                if let path = lanePath, path != selectedProjectPath {
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
        .onReceive(NotificationCenter.default.publisher(for: .vpPaneFocused)) { notification in
            // VP Pane クリック → フォーカス切り替え
            guard let paneId = notification.userInfo?["paneId"] as? UUID,
                  let path = selectedProjectPath else { return }
            if paneLayouts[path]?.focusedPaneId != paneId {
                paneLayouts[path]?.focusedPaneId = paneId
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .toggleSidebar)) { _ in
            withAnimation(.easeInOut(duration: 0.2)) {
                sidebarVisible.toggle()
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .toggleProjectTabBar)) { _ in
            withAnimation(.easeInOut(duration: 0.15)) {
                projectTabBarForced.toggle()
            }
        }
        .onChange(of: terminalPaths) { _, newPaths in
            // 新しいプロジェクトの VP Pane レイアウトを初期化
            for path in newPaths where paneLayouts[path] == nil {
                paneLayouts[path] = VPPaneLayout.initial()
            }
        }
    }

    // MARK: - ターミナルヘッダー/フッター

    /// 選択中プロジェクトの情報（worker 選択時は親プロジェクト）
    private var selectedProject: SidebarProject? {
        guard let path = selectedProjectPath else { return nil }
        if let project = projects.first(where: { $0.path == path }) {
            return project
        }
        // worker パスから親プロジェクトを解決
        return projects.first(where: { project in
            project.workers.contains(where: { $0.id == path })
        })
    }

    /// projects からターミナルパス一覧を計算（差分チェック用）
    private static func buildTerminalPaths(from projects: [SidebarProject]) -> [String] {
        var paths: [String] = []
        for project in projects {
            paths.append(project.path)
            for worker in project.workers {
                paths.append(worker.path)
            }
        }
        return paths
    }

    // MARK: - Pane Split Navigator

    /// フッターナビ UI
    @ViewBuilder
    private var splitNavigatorFooter: some View {
        HStack(spacing: 0) {
            switch splitNavigator {
            case .hidden:
                EmptyView()

            case .direction(let selected):
                HStack(spacing: 4) {
                    Text("Split")
                        .fontWeight(.medium)
                        .foregroundStyle(.secondary)
                    ForEach(splitDirections) { dir in
                        splitNavItem(
                            index: dir.id,
                            label: "\(dir.symbol) \(dir.label)",
                            isSelected: dir.id == selected,
                            total: splitDirections.count
                        )
                    }
                    Spacer()
                    Text("←→/Enter  Esc:cancel")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }

            case .content(_, let selected):
                HStack(spacing: 4) {
                    Text("Open")
                        .fontWeight(.medium)
                        .foregroundStyle(.secondary)
                    ForEach(splitContents) { content in
                        splitNavItem(
                            index: content.id,
                            label: "\(content.emoji) \(content.label)",
                            isSelected: content.id == selected,
                            total: splitContents.count
                        )
                    }
                    Spacer()
                    Text("←→/Enter  Esc:cancel")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .font(.caption)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(Color(white: 0.12).opacity(0.95))
    }

    /// ナビの個別アイテム
    private func splitNavItem(index: Int, label: String, isSelected: Bool, total: Int) -> some View {
        Text("\(index + 1): \(label)")
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(isSelected ? Color.accentColor.opacity(0.3) : Color.clear)
            .cornerRadius(4)
            .foregroundStyle(isSelected ? .white : .secondary)
    }

    /// キー入力ハンドラー
    private func handleSplitNavigatorKey(_ notification: Notification) {
        guard let key = notification.userInfo?["key"] as? String else { return }

        withAnimation(.easeInOut(duration: 0.15)) {
            switch splitNavigator {
            case .hidden:
                break

            case .direction(let selected):
                switch key {
                case "left":
                    splitNavigator = .direction(selected: (selected - 1 + splitDirections.count) % splitDirections.count)
                case "right":
                    splitNavigator = .direction(selected: (selected + 1) % splitDirections.count)
                case "enter":
                    let dir = splitDirections[selected]
                    splitNavigator = .content(horizontal: dir.horizontal, selected: 0)
                case "1", "2", "3", "4":
                    let idx = Int(key)! - 1
                    if idx < splitDirections.count {
                        let dir = splitDirections[idx]
                        splitNavigator = .content(horizontal: dir.horizontal, selected: 0)
                    }
                case "escape":
                    splitNavigator = .hidden
                default:
                    break
                }

            case .content(let horizontal, let selected):
                switch key {
                case "left":
                    splitNavigator = .content(horizontal: horizontal, selected: (selected - 1 + splitContents.count) % splitContents.count)
                case "right":
                    splitNavigator = .content(horizontal: horizontal, selected: (selected + 1) % splitContents.count)
                case "enter":
                    let content = splitContents[selected]
                    executeSplit(horizontal: horizontal, contentType: content.contentType)
                    splitNavigator = .hidden
                case "1", "2", "3":
                    let idx = Int(key)! - 1
                    if idx < splitContents.count {
                        let content = splitContents[idx]
                        executeSplit(horizontal: horizontal, contentType: content.contentType)
                        splitNavigator = .hidden
                    }
                case "escape":
                    splitNavigator = .hidden
                default:
                    break
                }
            }
        }
    }

    /// VP Pane 追加（NSView レイヤの分割）
    ///
    /// tmux split API ではなく、SwiftUI レイヤでペインを分割する。
    /// 新しいペインは tmux の新 window + グループセッション経由で独立表示。
    private func executeSplit(horizontal: Bool, contentType: String) {
        guard let path = selectedProjectPath else { return }

        // レイアウトが無ければ初期化
        if paneLayouts[path] == nil {
            paneLayouts[path] = VPPaneLayout.initial()
        }

        let paneId = UUID()
        let shortId = paneId.uuidString.prefix(8).lowercased()
        let projectName = (path as NSString).lastPathComponent
            .replacingOccurrences(of: ".", with: "-")
        let paneSession = "\(projectName)-vpp-\(shortId)"

        let newLeaf = VPPaneLeaf(
            id: paneId,
            paneSessionName: paneSession,
            tmuxWindowName: nil,
            contentType: contentType
        )

        var layout = paneLayouts[path]!
        layout.root = layout.root.inserting(
            newLeaf: newLeaf,
            adjacentTo: layout.focusedPaneId,
            horizontal: horizontal
        )
        layout.focusedPaneId = paneId
        paneLayouts[path] = layout

        logger.info("VP Pane added: \(paneSession) (horizontal=\(horizontal), content=\(contentType), leafCount=\(layout.root.leafCount))")
    }

    /// VP Pane を閉じる（⌘⇧D）
    ///
    /// フォーカス中の VP Pane を削除し、対応する tmux リソースをクリーンアップ。
    /// 最後の 1 つは閉じない（プロジェクトには最低 1 ペイン必要）。
    private func closePane() {
        guard let path = selectedProjectPath else { return }
        let layout = paneLayouts[path] ?? VPPaneLayout.initial()
        closePane(path: path, paneId: layout.focusedPaneId)
    }

    /// 指定ペインを閉じる（PaneHeader の × ボタンから呼ばれる）
    private func closePane(path: String, paneId: UUID) {
        guard var layout = paneLayouts[path],
              layout.root.leafCount > 1 else {
            logger.info("VP Pane close: 最後の1つは閉じない")
            return
        }

        // 最後の Agent ペインは閉じない（VP-46: Agent 消失防止）
        if let leaf = layout.root.findLeaf(id: paneId),
           leaf.contentType == "agent" {
            let agentCount = layout.root.leaves.filter { $0.contentType == "agent" }.count
            if agentCount <= 1 {
                logger.info("VP Pane close: 最後の Agent ペインは閉じない")
                return
            }
        }

        // 削除対象のリーフの tmux リソースをクリーンアップ
        if let leaf = layout.root.findLeaf(id: paneId) {
            cleanupVPPaneTmux(leaf: leaf)
        }

        // ツリーから削除
        if let newRoot = layout.root.removing(targetId: paneId) {
            layout.root = newRoot
            // フォーカスを最初のリーフに移動
            if layout.focusedPaneId == paneId {
                layout.focusedPaneId = newRoot.leafIds.first ?? layout.focusedPaneId
            }
            paneLayouts[path] = layout
        }

        logger.info("VP Pane closed: \(layout.root.leafCount) panes remaining")
    }

    /// ペインを退避して Dock に格納 (VP-49)
    private func minimizePane(path: String, paneId: UUID) {
        guard var layout = paneLayouts[path],
              layout.root.leafCount > 1,
              let leaf = layout.root.findLeaf(id: paneId) else {
            logger.info("VP Pane minimize: 最後の1つは退避できない")
            return
        }

        // 隣接リーフ ID を取得（復帰時の挿入位置）
        let leafIds = layout.root.leafIds
        let adjacentId: UUID? = {
            guard let idx = leafIds.firstIndex(of: paneId) else { return nil }
            if idx > 0 { return leafIds[idx - 1] }
            if idx < leafIds.count - 1 { return leafIds[idx + 1] }
            return nil
        }()

        // MinimizedPane を作成
        let minimized = MinimizedPane(
            id: paneId,
            leaf: leaf,
            adjacentToId: adjacentId,
            horizontal: true,
            standInfo: PaneStandInfo.from(leaf: leaf)
        )

        // ツリーから削除（tmux はクリーンアップしない — 復帰時に再利用）
        // removing が成功した場合のみ Dock に追加する（アトミックに更新）
        guard let newRoot = layout.root.removing(targetId: paneId) else { return }

        withAnimation(.spring(duration: 0.3)) {
            layout.root = newRoot
            if layout.focusedPaneId == paneId {
                layout.focusedPaneId = newRoot.leafIds.first ?? layout.focusedPaneId
            }
            paneLayouts[path] = layout

            // Dock に追加
            var docked = minimizedPanes[path] ?? []
            docked.append(minimized)
            minimizedPanes[path] = docked
        }

        logger.info("VP Pane minimized: \(leaf.contentType) → Dock (\(minimizedPanes[path]?.count ?? 0) items)")
    }

    /// Dock から復帰（元の分割位置に挿入）(VP-49)
    private func restorePane(path: String, pane: MinimizedPane) {
        guard var layout = paneLayouts[path] else { return }

        // Dock から削除
        withAnimation(.spring(duration: 0.3)) {
            minimizedPanes[path]?.removeAll { $0.id == pane.id }

            // ツリーに再挿入（adjacentToId の隣に分割）
            let targetId = pane.adjacentToId ?? layout.focusedPaneId
            layout.root = layout.root.inserting(
                newLeaf: pane.leaf,
                adjacentTo: targetId,
                horizontal: pane.horizontal
            )
            layout.focusedPaneId = pane.leaf.id
            paneLayouts[path] = layout
        }

        logger.info("VP Pane restored: \(pane.standInfo.label) from Dock")
    }

    // MARK: - VP Pane ヘルパー

    /// プロジェクトパスから tmux セッション名を生成
    private func tmuxSessionName(for path: String) -> String {
        let projectName = (path as NSString).lastPathComponent
        return projectName.replacingOccurrences(of: ".", with: "-") + "-vp"
    }

    // MARK: - SP 自動起動

    /// SP 未起動のプロジェクトを TheWorld API 経由で自動起動
    ///
    /// ポーリングで繰り返し起動しないよう、試行済みパスを記録。
    private func autoStartSP(project: SidebarProject) async {
        guard !spAutoStartAttempted.contains(project.path) else { return }
        spAutoStartAttempted.insert(project.path)
        logger.info("[VP]Auto-starting SP for: \(project.name)")

        do {
            _ = try await theWorldClient.startProcess(projectName: project.name)
            logger.info("[VP]SP auto-started: \(project.name)")
        } catch {
            logger.error("[VP]SP auto-start failed: \(project.name) - \(error)")
        }
    }

    // MARK: - HD 自動起動

    /// SP 稼働中 + HD 未起動のプロジェクトに HD を自動起動
    ///
    /// ポーリングで繰り返し起動しないよう、試行済みパスを記録。
    /// HD が起動したら hasHD = true になり、次のポーリングでは対象外。
    ///
    /// Note: Process() は App Sandbox では使えないが、現在は Notarize のみ配布。
    /// App Store 配布時は SP の HTTP API 経由（POST /api/hd/start）に移行する。
    private func autoStartHD(path: String) {
        guard !hdAutoStartAttempted.contains(path) else { return }
        hdAutoStartAttempted.insert(path)
        logger.info("[VP]Auto-starting HD for: \(path)")

        Task.detached(priority: .utility) {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/bin/zsh")
            process.arguments = ["-lc", "vp hd start"]
            process.currentDirectoryURL = URL(fileURLWithPath: path)
            process.standardOutput = FileHandle.nullDevice
            process.standardError = FileHandle.nullDevice
            do {
                try process.run()
                process.waitUntilExit()
                logger.info("[VP]Auto HD start exit=\(process.terminationStatus) for \(path)")
            } catch {
                logger.error("[VP]Auto HD start error: \(error)")
            }
        }
    }

    // MARK: - HD リスタート

    /// HD（tmux セッション）を再生成する
    ///
    /// `vp hd stop && vp hd start` をプロジェクトディレクトリで実行。
    /// tmux セッション死亡 → PTY 終了検知 → 自動復旧でターミナルが再接続する。
    ///
    /// Note: Process() は App Sandbox では使えないが、現在は Notarize のみ配布。
    /// TerminalView の PTY spawn も同様に非 Sandbox 前提。
    /// App Store 配布時は SP の HTTP API 経由（POST /api/hd/restart）に移行する。
    private func restartHD(path: String) {
        logger.info("[VP]restartHD called for path: \(path)")

        // waitUntilExit() はブロッキング API のため detached で実行
        Task.detached(priority: .utility) {
            // vp hd stop → vp hd start（tmux セッション再生成）
            // zsh -lc 経由なので PATH から vp を解決（ハードコード不要）
            for (label, cmd) in [("hd stop", "vp hd stop"), ("hd start", "vp hd start")] {
                let process = Process()
                process.executableURL = URL(fileURLWithPath: "/bin/zsh")
                process.arguments = ["-lc", cmd]
                process.currentDirectoryURL = URL(fileURLWithPath: path)
                process.standardOutput = FileHandle.nullDevice
                process.standardError = FileHandle.nullDevice
                do {
                    try process.run()
                    process.waitUntilExit()
                    logger.info("[VP]\(label) exit=\(process.terminationStatus)")
                } catch {
                    logger.error("[VP]\(label) error: \(error)")
                }
            }

            // @State 更新は MainActor で実行
            await MainActor.run {
                terminalGeneration[path, default: 0] += 1
                logger.info("HD restart done, terminal generation=\(terminalGeneration[path] ?? 0)")
            }
        }
    }

    /// SP（Star Platinum）をリスタート — TheWorld API 経由で stop → start
    private func restartSP(path: String) {
        logger.info("[VP]restartSP called for path: \(path)")
        guard let project = projects.first(where: { $0.path == path }) else { return }

        Task {
            // stop と start を独立 do-catch に分離（stop 失敗でも start を試行）
            do {
                try await theWorldClient.stopProcess(projectName: project.name)
                hdAutoStartAttempted.remove(path)
                spAutoStartAttempted.remove(path)
                logger.info("[VP]SP stopped: \(project.name)")
            } catch {
                logger.error("[VP]SP stop skipped (may already be stopped): \(error)")
            }

            // ポート解放待ち
            try? await Task.sleep(nanoseconds: 500_000_000)

            do {
                let newProcess = try await theWorldClient.startProcess(projectName: project.name)
                logger.info("[VP]SP restarted: \(project.name) on port \(newProcess.port)")
            } catch {
                logger.error("[VP]SP start error: \(error)")
            }

            await refreshAll()
        }
    }

    /// TheWorld を再起動（vp stop → vp world）
    private func restartWorld() {
        Task {
            worldStatus = .checking

            // 停止
            do {
                let stop = Process()
                stop.executableURL = URL(fileURLWithPath: "/bin/zsh")
                stop.arguments = ["-lc", "vp stop --port 32000"]
                stop.standardOutput = FileHandle.nullDevice
                stop.standardError = FileHandle.nullDevice
                try stop.run()
                stop.waitUntilExit()
                logger.info("[VP]TheWorld stopped")
            } catch {
                logger.error("[VP]TheWorld stop skipped: \(error)")
            }

            try? await Task.sleep(nanoseconds: 1_000_000_000)

            // 起動（バックグラウンドで vp world）
            do {
                let start = Process()
                start.executableURL = URL(fileURLWithPath: "/bin/zsh")
                start.arguments = ["-lc", "vp world"]
                start.standardOutput = FileHandle.nullDevice
                start.standardError = FileHandle.nullDevice
                try start.run()
                logger.info("[VP]TheWorld starting")
            } catch {
                logger.error("[VP]TheWorld start error: \(error)")
            }

            // 起動待ち
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            await refreshAll()
        }
    }

    // MARK: - プロジェクト選択ナビゲーション

    /// 前のプロジェクトを選択（⌘↑）— enabled プロジェクトのみ
    private func selectPreviousProject() {
        let enabled = enabledProjects
        guard !enabled.isEmpty else { return }
        guard let current = selectedProjectPath else {
            selectedProjectPath = enabled.last?.path
            return
        }
        guard let index = enabled.firstIndex(where: { $0.path == current }) else {
            return // disabled 選択中は何もしない
        }
        guard index > 0 else {
            selectedProjectPath = enabled.last?.path // 先頭でラップアラウンド
            return
        }
        selectedProjectPath = enabled[index - 1].path
    }

    /// 次のプロジェクトを選択（⌘↓）— enabled プロジェクトのみ
    private func selectNextProject() {
        let enabled = enabledProjects
        guard !enabled.isEmpty else { return }
        guard let current = selectedProjectPath else {
            selectedProjectPath = enabled.first?.path
            return
        }
        guard let index = enabled.firstIndex(where: { $0.path == current }) else {
            return // disabled 選択中は何もしない
        }
        guard index < enabled.count - 1 else {
            selectedProjectPath = enabled.first?.path // 末尾でラップアラウンド
            return
        }
        selectedProjectPath = enabled[index + 1].path
    }

    /// ⌘1〜9 で enabled プロジェクトを番号で切り替え
    private func selectProjectByNumber(_ number: Int) {
        let enabled = enabledProjects
        let index = number - 1
        guard index >= 0 && index < enabled.count else { return }
        selectedProjectPath = enabled[index].path
    }

    /// ⌃1〜9 でフォーカス中プロジェクト内の Lane を番号で切り替え
    private func selectLaneByNumber(_ number: Int) {
        let lanes = currentLanes
        let index = number - 1
        guard index >= 0 && index < lanes.count else { return }
        selectedProjectPath = lanes[index].path
    }

    // MARK: - プロジェクト CRUD（TheWorld API 経由）

    /// フォルダ選択ダイアログでプロジェクトを追加
    private func addProject() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "プロジェクトフォルダを選択"
        panel.prompt = "追加"

        guard panel.runModal() == .OK, let url = panel.url else { return }

        let path = url.path
        let name = url.lastPathComponent
        Task {
            try? await theWorldClient.addProject(name: name, path: path)
            await refreshAll()
        }
    }

    /// ドラッグ＆ドロップでプロジェクトを追加（URL 指定）
    private func dropAddProject(url: URL) {
        let path = url.path
        let name = url.lastPathComponent
        Task {
            try? await theWorldClient.addProject(name: name, path: path)
            await refreshAll()
        }
    }

    /// プロジェクトをリストから削除（SP 稼働中なら先に停止）
    private func deleteProject(path: String) {
        Task {
            // SP 稼働中なら先に停止
            if let project = projects.first(where: { $0.path == path }), project.isRunning {
                try? await theWorldClient.stopProcess(projectName: project.name)
            }
            try? await theWorldClient.removeProject(path: path)
            await refreshAll()
        }
    }

    /// プロジェクトの並び順を変更（ドラッグ＆ドロップ）
    private func reorderProjects(from: IndexSet, to: Int) {
        var paths = projects.map(\.path)
        paths.move(fromOffsets: from, toOffset: to)
        Task {
            try? await theWorldClient.reorderProjects(paths: paths)
            await refreshAll()
        }
    }

    /// プロジェクト名を変更
    private func renameProject(path: String, newName: String) {
        Task {
            try? await theWorldClient.updateProject(path: path, name: newName)
            await refreshAll()
        }
    }

    /// SP の有効/無効をトグル
    private func toggleProjectEnabled(path: String, enabled: Bool) {
        Task {
            do {
                try await theWorldClient.setProjectEnabled(path: path, enabled: enabled)
                // 無効化する場合は SP を停止
                if !enabled, let project = projects.first(where: { $0.path == path }), project.isRunning {
                    try? await theWorldClient.stopProcess(projectName: project.name)
                }
                await refreshAll()
            } catch {
                logger.error("[VP]toggleProjectEnabled error: \(error)")
            }
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

            // プロジェクト一覧を TheWorld API から取得（config.toml ではなく TheWorld が真実源）
            let registeredProjects = (try? await theWorldClient.listProjects()) ?? []

            // ccwire セッション一覧を取得（エラー時は空配列）
            let ccwireSessions = (try? await theWorldClient.listCcwireSessions()) ?? []

            // ccws ワーカー + Git ブランチをバックグラウンドでスキャン
            let projectEntries = registeredProjects
            let projectInfoByPath = await Task.detached(priority: .utility) {
                Dictionary(uniqueKeysWithValues: projectEntries.map { entry in
                    let workers = CcwsDiscovery.discoverWorkers(forProject: entry.name)
                    let branch = CcwsDiscovery.readGitBranch(
                        at: URL(fileURLWithPath: entry.path)
                    )
                    let tmuxName = entry.name.replacingOccurrences(of: ".", with: "-") + "-vp"
                    let hasHD = CcwsDiscovery.tmuxSessionExists(tmuxName)
                    return (entry.path, (workers: workers, branch: branch, hasHD: hasHD))
                })
            }.value

            projects = registeredProjects.map { entry in
                let info = projectInfoByPath[entry.path]
                let branch = info?.branch
                let hasHD = info?.hasHD ?? false

                // Worker に ccwire セッションを注入
                let workers: [CcwsWorkerInfo] = (info?.workers ?? []).map { worker in
                    let workerTmux = worker.name.replacingOccurrences(of: ".", with: "-") + "-vp"
                    let wireSession = ccwireSessions.first { $0.name == workerTmux }
                    return CcwsWorkerInfo(
                        id: worker.id, name: worker.name, suffix: worker.suffix,
                        path: worker.path, branch: worker.branch, hasHD: worker.hasHD,
                        ccwireSession: wireSession
                    )
                }

                // ccwire セッション名マッチング: "{project-name}-vp" パターン
                let tmuxName = entry.name.replacingOccurrences(of: ".", with: "-") + "-vp"
                let wireSession = ccwireSessions.first { $0.name == tmuxName }

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
                        workers: workers,
                        branch: branch,
                        hasHD: hasHD,
                        hasNotification: notifications.contains(entry.path),
                        ccwireSession: wireSession,
                        enabled: entry.enabled
                    )
                } else {
                    return SidebarProject(
                        id: entry.path,
                        name: entry.name,
                        path: entry.path,
                        isRunning: false,
                        port: nil,
                        startedAt: nil,
                        workers: workers,
                        branch: branch,
                        hasHD: hasHD,
                        hasNotification: notifications.contains(entry.path),
                        ccwireSession: wireSession,
                        enabled: entry.enabled
                    )
                }
            }

            // SP 未起動 + enabled のプロジェクトを自動起動（TheWorld API 経由）
            for project in projects where !project.isRunning && project.enabled {
                await autoStartSP(project: project)
            }

            // SP 稼働中 + HD 未起動のプロジェクトに HD を自動起動
            for project in projects where project.isRunning && !project.hasHD {
                autoStartHD(path: project.path)
            }
            // ccws ワーカーの HD も自動起動（ワーカー環境が存在 + HD 未起動）
            for project in projects {
                for worker in project.workers where !worker.hasHD {
                    autoStartHD(path: worker.path)
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

    /// 全プロジェクトを非稼働にリセット（workers はローカル情報なので保持）
    private func resetProjectStatus() {
        projects = projects.map { project in
            // Worker の ccwireSession もクリア（TheWorld オフライン時は ccwire 情報も無効）
            let cleanWorkers = project.workers.map { worker in
                CcwsWorkerInfo(
                    id: worker.id, name: worker.name, suffix: worker.suffix,
                    path: worker.path, branch: worker.branch, hasHD: worker.hasHD,
                    ccwireSession: nil
                )
            }
            return SidebarProject(
                id: project.id,
                name: project.name,
                path: project.path,
                isRunning: false,
                port: nil,
                startedAt: nil,
                workers: cleanWorkers,
                hasNotification: notifications.contains(project.path)
            )
        }
    }
}

// MARK: - Project Tab バー

/// Project Tab バー — enabled プロジェクト切替
struct ProjectTabBar: View {
    let projects: [SidebarProject]
    let selectedPath: String?
    let onSelect: (String) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(projects.enumerated()), id: \.element.id) { index, project in
                let isSelected = project.path == selectedPath
                Button {
                    onSelect(project.path)
                } label: {
                    HStack(spacing: 4) {
                        // ⌘1〜9 のみヒント表示（キーバインドは9まで）
                        if index < 9 {
                            Text("⌘\(index + 1)")
                                .font(.system(size: 9, weight: .medium, design: .monospaced))
                                .foregroundStyle(.tertiary)
                        }

                        // 稼働状態のドット
                        Circle()
                            .fill(project.isRunning ? .green : .gray)
                            .frame(width: 6, height: 6)

                        Text(project.name)
                            .font(.system(size: 11, weight: isSelected ? .semibold : .regular))
                            .lineLimit(1)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(isSelected ? Color.white.opacity(0.1) : Color.clear)
                    .cornerRadius(4)
                }
                .buttonStyle(.plain)
                .foregroundStyle(isSelected ? .primary : .secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 8)
        .padding(.top, 6)
        .padding(.bottom, 2)
        .background(Color(white: 0.1))
    }
}

// MARK: - Lane モデル

/// プロジェクト内の Lane（lead または worker）
struct Lane: Identifiable {
    let path: String
    let label: String
    let isLead: Bool

    var id: String { path }
}

/// Lane Tab バー — フォーカス中プロジェクトの Lane 切替 (VP-51)
struct LaneTabBar: View {
    let lanes: [Lane]
    let selectedPath: String?
    let onSelect: (String) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(lanes.enumerated()), id: \.element.id) { index, lane in
                let isSelected = lane.path == selectedPath
                Button {
                    onSelect(lane.path)
                } label: {
                    HStack(spacing: 4) {
                        // ⌃番号のショートカットヒント
                        Text("⌃\(index + 1)")
                            .font(.system(size: 9, weight: .medium, design: .monospaced))
                            .foregroundStyle(.tertiary)

                        Image(systemName: lane.isLead ? "text.book.closed" : "arrow.branch")
                            .font(.system(size: 10))
                            .foregroundStyle(lane.isLead ? .green : .cyan)

                        Text(lane.label)
                            .font(.system(size: 11, weight: isSelected ? .semibold : .regular))
                            .lineLimit(1)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(isSelected ? Color.white.opacity(0.08) : Color.clear)
                    .cornerRadius(4)
                }
                .buttonStyle(.plain)
                .foregroundStyle(isSelected ? .primary : .secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 2)
        .background(Color(white: 0.12))
    }
}

// MARK: - Notification Names

extension Notification.Name {
    static let selectPreviousProject = Notification.Name("VP.selectPreviousProject")
    static let selectNextProject = Notification.Name("VP.selectNextProject")
    static let splitTerminalPane = Notification.Name("VP.splitTerminalPane")
    static let closeTerminalPane = Notification.Name("VP.closeTerminalPane")
    static let selectProjectByNumber = Notification.Name("VP.selectProjectByNumber")
    static let selectLaneByNumber = Notification.Name("VP.selectLaneByNumber")
    static let splitNavigatorKey = Notification.Name("VP.splitNavigatorKey")
    static let vpPaneFocused = Notification.Name("VP.vpPaneFocused")
    static let toggleSidebar = Notification.Name("VP.toggleSidebar")
    static let toggleProjectTabBar = Notification.Name("VP.toggleProjectTabBar")
}
