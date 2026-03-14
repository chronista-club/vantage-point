import AppKit
import Combine
import SwiftUI

/// AppDelegate - メニューバーアイコン + ポップオーバーダッシュボード
@MainActor
class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private var popover: NSPopover!

    /// TheWorld API client（共有インスタンス）
    private let theWorldClient = TheWorldClient.shared

    /// Popover ViewModel
    private lazy var popoverViewModel = PopoverViewModel(theWorldClient: theWorldClient)

    /// Update service
    private lazy var updateService = UpdateService(client: theWorldClient)

    /// Update dialog window controller
    private var updateWindowController = UpdateWindowController()

    /// Update service observer
    private var updateCancellable: AnyCancellable?

    /// User prompt service for CC prompts
    private let userPromptService = UserPromptService()

    /// ステータスアイコンの更新タイマー
    private var iconTimer: Timer?

    /// Settings window controller
    private var settingsWindowController = SettingsWindowController()

    /// イベントモニター（クリック外でポップオーバーを閉じる）
    private var eventMonitor: Any?

    /// プロジェクト選択通知（Popover → MainWindowView）
    static let selectProjectNotification = Notification.Name("club.chronista.vp.selectProject")
    /// CC 完了通知（Notification hook → サイドバーバッジ）
    static let ccNotification = Notification.Name("club.chronista.vp.cc.notification")

    /// DistributedNotification リスナー
    private var ccNotificationObserver: NSObjectProtocol?

    func applicationDidFinishLaunching(_: Notification) {
        // Dock アイコン + メニューバーを有効化（Liquid Glass ウィンドウアプリ）
        NSApp.setActivationPolicy(.regular)

        setupMainMenu()
        setupStatusItem()
        setupPopover()
        setupUpdateObserver()
        setupEventMonitor()

        // ポップオーバーからプロジェクト選択 → 通知でメインウィンドウに伝達
        popoverViewModel.onOpenMainWindow = { [weak self] projectPath in
            self?.closePopover()
            NotificationCenter.default.post(
                name: AppDelegate.selectProjectNotification,
                object: nil,
                userInfo: ["path": projectPath]
            )
            NSApp.activate(ignoringOtherApps: true)
        }

        // CC 完了通知をサイドバーに転送
        setupCCNotificationObserver()

        // 自動リフレッシュ開始
        popoverViewModel.startAutoRefresh(interval: 5.0)

        // ステータスアイコンの自動更新
        startIconRefresh()

        // 初回データ取得
        Task {
            await popoverViewModel.refresh()
            updateStatusIcon()
            updatePromptServiceProcesses()
        }

        // User Prompt ポーリング開始
        userPromptService.startPolling()

        // 起動時の更新チェック
        Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            await checkForUpdatesOnLaunch()
        }

        // コマンドライン引数: --project /path/to/dir → 通知でメインウィンドウに伝達
        handleLaunchArguments()
    }

    /// コマンドライン引数を処理
    ///
    /// 使い方: `open VantagePoint.app --args --project /path/to/dir`
    private func handleLaunchArguments() {
        let args = ProcessInfo.processInfo.arguments
        if let idx = args.firstIndex(of: "--project"), idx + 1 < args.count {
            let projectPath = args[idx + 1]
            guard FileManager.default.fileExists(atPath: projectPath) else {
                NSLog("[VP] --project path does not exist: %@", projectPath)
                return
            }
            // WindowGroup のウィンドウが表示されてから通知を送る
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                NotificationCenter.default.post(
                    name: AppDelegate.selectProjectNotification,
                    object: nil,
                    userInfo: ["path": projectPath]
                )
            }
        }
    }

    /// Apple Event (URL スキーム) でプロジェクトを開く
    ///
    /// vantagepoint://open?path=/path/to/dir
    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            guard url.scheme == "vantagepoint", url.host == "open" else { continue }
            if let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
               let path = components.queryItems?.first(where: { $0.name == "path" })?.value {
                if FileManager.default.fileExists(atPath: path) {
                    NotificationCenter.default.post(
                        name: AppDelegate.selectProjectNotification,
                        object: nil,
                        userInfo: ["path": path]
                    )
                    NSApp.activate(ignoringOtherApps: true)
                }
            }
        }
    }

    /// Dock アイコンクリック時 — ウィンドウが無ければ SwiftUI が新規作成
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            // SwiftUI WindowGroup が新しいウィンドウを自動作成
            return true
        }
        // 既存ウィンドウを前面に
        NSApp.windows.first { $0.canBecomeMain && $0.isVisible }?.makeKeyAndOrderFront(nil)
        return false
    }

    /// File > New Window: メインウィンドウを表示（なければ新規作成）
    @objc func showMainWindow(_ sender: Any?) {
        let mainWindows = NSApp.windows.filter { $0.canBecomeMain }
        if let existing = mainWindows.first(where: { $0.isVisible }) {
            existing.makeKeyAndOrderFront(nil)
        } else {
            // SwiftUI WindowGroup にウィンドウ作成を依頼
            NSApp.sendAction(Selector(("newWindowForTab:")), to: nil, from: nil)
        }
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationWillTerminate(_: Notification) {
        userPromptService.stopPolling()
        popoverViewModel.stopAutoRefresh()
        iconTimer?.invalidate()
        if let monitor = eventMonitor {
            NSEvent.removeMonitor(monitor)
        }
    }

    // MARK: - メインメニュー（キーボードショートカット用）

    /// メニューバーアプリでも Cmd+T / Cmd+W が効くよう、最小限のメニューを構築
    private func setupMainMenu() {
        let mainMenu = NSMenu()

        // Application メニュー
        let appMenu = NSMenu()
        appMenu.addItem(NSMenuItem(title: "About Vantage Point", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: ""))
        appMenu.addItem(.separator())
        appMenu.addItem(NSMenuItem(title: "Settings…", action: #selector(openSettings(_:)), keyEquivalent: ","))
        appMenu.addItem(.separator())
        let hideItem = NSMenuItem(title: "Hide Vantage Point", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        appMenu.addItem(hideItem)
        let hideOthersItem = NSMenuItem(title: "Hide Others", action: #selector(NSApplication.hideOtherApplications(_:)), keyEquivalent: "h")
        hideOthersItem.keyEquivalentModifierMask = [.command, .option]
        appMenu.addItem(hideOthersItem)
        appMenu.addItem(NSMenuItem(title: "Show All", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: ""))
        appMenu.addItem(.separator())
        appMenu.addItem(NSMenuItem(title: "Quit Vantage Point", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        let appMenuItem = NSMenuItem()
        appMenuItem.submenu = appMenu
        mainMenu.addItem(appMenuItem)

        // File メニュー
        let fileMenu = NSMenu(title: "File")
        fileMenu.addItem(NSMenuItem(title: "New Window", action: #selector(showMainWindow(_:)), keyEquivalent: "n"))
        fileMenu.addItem(NSMenuItem(title: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w"))
        let fileMenuItem = NSMenuItem()
        fileMenuItem.submenu = fileMenu
        mainMenu.addItem(fileMenuItem)

        // View メニュー（サイドバートグル）
        let viewMenu = NSMenu(title: "View")
        let toggleSidebarItem = NSMenuItem(
            title: "Toggle Sidebar",
            action: #selector(NSSplitViewController.toggleSidebar(_:)),
            keyEquivalent: "s"
        )
        toggleSidebarItem.keyEquivalentModifierMask = [.command, .option]
        viewMenu.addItem(toggleSidebarItem)
        let viewMenuItem = NSMenuItem()
        viewMenuItem.submenu = viewMenu
        mainMenu.addItem(viewMenuItem)

        // Edit メニュー（Cmd+C / Cmd+V / Cmd+A）
        // copy:/paste:/selectAll: セレクタは first responder に送られる
        // → TerminalView が first responder なら TerminalView の実装が呼ばれる
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(NSMenuItem(title: "Copy", action: Selector(("copy:")), keyEquivalent: "c"))
        editMenu.addItem(NSMenuItem(title: "Paste", action: Selector(("paste:")), keyEquivalent: "v"))
        editMenu.addItem(NSMenuItem(title: "Select All", action: Selector(("selectAll:")), keyEquivalent: "a"))
        let editMenuItem = NSMenuItem()
        editMenuItem.submenu = editMenu
        mainMenu.addItem(editMenuItem)

        // Navigate メニュー（プロジェクト切り替え）
        let navigateMenu = NSMenu(title: "Navigate")
        let prevItem = NSMenuItem(title: "前のプロジェクト", action: #selector(selectPreviousProject(_:)), keyEquivalent: "\u{F700}") // ↑
        prevItem.keyEquivalentModifierMask = .command
        navigateMenu.addItem(prevItem)
        let nextItem = NSMenuItem(title: "次のプロジェクト", action: #selector(selectNextProject(_:)), keyEquivalent: "\u{F701}") // ↓
        nextItem.keyEquivalentModifierMask = .command
        navigateMenu.addItem(nextItem)
        let navigateMenuItem = NSMenuItem()
        navigateMenuItem.submenu = navigateMenu
        mainMenu.addItem(navigateMenuItem)

        // Window メニュー
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(NSMenuItem(title: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m"))
        windowMenu.addItem(NSMenuItem(title: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: ""))
        windowMenu.addItem(.separator())
        windowMenu.addItem(NSMenuItem(title: "Show All", action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: ""))
        let windowMenuItem = NSMenuItem()
        windowMenuItem.submenu = windowMenu
        mainMenu.addItem(windowMenuItem)
        NSApp.windowsMenu = windowMenu

        NSApp.mainMenu = mainMenu
    }

    /// CC 完了通知の DistributedNotification リスナーを設定
    private func setupCCNotificationObserver() {
        ccNotificationObserver = DistributedNotificationCenter.default().addObserver(
            forName: NSNotification.Name("club.chronista.vp.cc.notification"),
            object: nil,
            queue: .main
        ) { notification in
            let project = notification.userInfo?["project"] as? String ?? ""
            let message = notification.userInfo?["message"] as? String ?? "完了"
            // ローカル Notification で MainWindowView に転送
            NotificationCenter.default.post(
                name: AppDelegate.ccNotification,
                object: nil,
                userInfo: ["project": project, "message": message]
            )
        }
    }

    @objc private func openSettings(_ sender: Any?) {
        settingsWindowController.show()
    }

    @objc private func selectPreviousProject(_ sender: Any?) {
        NotificationCenter.default.post(name: .selectPreviousProject, object: nil)
    }

    @objc private func selectNextProject(_ sender: Any?) {
        NotificationCenter.default.post(name: .selectNextProject, object: nil)
    }

    // MARK: - Status Item

    private func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "mountain.2", accessibilityDescription: "Vantage Point")
            button.image?.isTemplate = true
            button.action = #selector(togglePopover)
            button.target = self
        }
    }

    // MARK: - Popover

    private func setupPopover() {
        popover = NSPopover()
        popover.behavior = .transient
        popover.animates = true

        let contentView = PopoverView(
            viewModel: popoverViewModel,
            onQuit: {
                NSApp.terminate(nil)
            }
        )

        popover.contentViewController = NSHostingController(rootView: contentView)
    }

    @objc private func togglePopover() {
        if popover.isShown {
            closePopover()
        } else {
            openPopover()
        }
    }

    private func openPopover() {
        guard let button = statusItem.button else { return }

        // リフレッシュしてからポップオーバーを表示
        Task {
            await popoverViewModel.refresh()
            updatePromptServiceProcesses()
        }

        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)

        // ポップオーバーのウィンドウを最前面に
        popover.contentViewController?.view.window?.makeKey()
    }

    private func closePopover() {
        popover.performClose(nil)
    }

    // MARK: - Event Monitor

    /// ポップオーバー外クリックで閉じる
    private func setupEventMonitor() {
        eventMonitor = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] _ in
            if let popover = self?.popover, popover.isShown {
                self?.closePopover()
            }
        }
    }

    // MARK: - Status Icon

    /// ステータスアイコンをプロジェクト状態に基づいて更新
    private func updateStatusIcon() {
        guard let button = statusItem.button else { return }

        let runningCount = popoverViewModel.projects.filter { $0.status == .running }.count

        if runningCount > 0 {
            // 稼働中プロセスあり → 塗りつぶし + カウント
            let desc = "Vantage Point (\(runningCount) running)"
            button.image = NSImage(systemSymbolName: "mountain.2.fill", accessibilityDescription: desc)
            button.title = runningCount > 1 ? " \(runningCount)" : ""
        } else {
            // すべて停止 → アウトライン
            button.image = NSImage(systemSymbolName: "mountain.2", accessibilityDescription: "Vantage Point (idle)")
            button.title = ""
        }

        button.image?.isTemplate = true
    }

    private func startIconRefresh() {
        iconTimer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.updateStatusIcon()
                self?.updatePromptServiceProcesses()
            }
        }
    }

    // MARK: - User Prompt Service

    /// Prompt Service に稼働中ポートを通知
    private func updatePromptServiceProcesses() {
        // TODO: TheWorld API から直接ポート取得に移行
        // 現在は Popover のリフレッシュ時に取得済みプロセス情報を使えないため空配列
        Task {
            let ports: [UInt16] = (try? await theWorldClient.listRunningProcesses().map(\.port)) ?? []
            userPromptService.updateActivePorts(ports: ports)
        }
    }

    // MARK: - Updates

    private func setupUpdateObserver() {
        updateCancellable = updateService.$showUpdateDialog
            .receive(on: DispatchQueue.main)
            .sink { [weak self] shouldShow in
                guard let self else { return }
                if shouldShow {
                    closePopover()
                    updateWindowController.show(updateService: updateService)
                } else {
                    updateWindowController.close()
                }
            }
    }

    private func checkForUpdatesOnLaunch() async {
        var retries = 0
        while retries < 5 {
            do {
                if try await theWorldClient.healthCheck() {
                    break
                }
            } catch {}
            retries += 1
            try? await Task.sleep(nanoseconds: 1_000_000_000)
        }

        await updateService.checkOnLaunchIfNeeded()
    }
}
