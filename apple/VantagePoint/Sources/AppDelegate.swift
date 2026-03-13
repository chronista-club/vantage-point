import AppKit
import Combine
import SwiftUI

/// AppDelegate - メニューバーアイコン + ポップオーバーダッシュボード
@MainActor
class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private var popover: NSPopover!

    /// TheWorld API client
    private let theWorldClient = TheWorldClient()

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

    /// マルチウィンドウコントローラー（プロジェクトパス → コントローラー）
    /// キー nil = プロジェクト未指定のデフォルトウィンドウ
    private var windowControllers: [String: MainWindowController] = [:]

    /// デフォルトウィンドウ用キー
    private static let defaultKey = "__default__"

    /// イベントモニター（クリック外でポップオーバーを閉じる）
    private var eventMonitor: Any?

    /// プロジェクトのウィンドウを開く（既に開いていれば前面に出す、新規はタブ追加）
    private func openWindow(projectPath: String? = nil) {
        let key = projectPath ?? Self.defaultKey

        if let existing = windowControllers[key] {
            existing.show(projectPath: projectPath)
            return
        }

        let controller = MainWindowController()
        controller.onClose = { [weak self] ctrl in
            guard let self else { return }
            let k = ctrl.projectPath ?? Self.defaultKey
            self.windowControllers.removeValue(forKey: k)
            // 全ウィンドウが閉じたら accessory モードに戻す
            if self.windowControllers.isEmpty {
                NSApp.setActivationPolicy(.accessory)
            }
        }

        // 既存タブグループのウィンドウを探して渡す
        let existingWindow = windowControllers.values.compactMap(\.window).first
        controller.tabGroupWindow = existingWindow

        windowControllers[key] = controller
        controller.show(projectPath: projectPath)
    }

    func applicationDidFinishLaunching(_: Notification) {
        // Hide dock icon (agent app)
        NSApp.setActivationPolicy(.accessory)

        setupStatusItem()
        setupPopover()
        setupUpdateObserver()
        setupEventMonitor()

        // メインウィンドウ表示コールバック設定
        popoverViewModel.onOpenMainWindow = { [weak self] projectPath in
            self?.closePopover()
            self?.openWindow(projectPath: projectPath)
        }

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

        // コマンドライン引数: --project /path/to/dir でウィンドウを開く
        handleLaunchArguments()
    }

    /// コマンドライン引数を処理
    ///
    /// 使い方: `open VantagePoint.app --args --project /path/to/dir`
    /// または: `VantagePoint.app/Contents/MacOS/VantagePoint --project /path/to/dir`
    private func handleLaunchArguments() {
        let args = ProcessInfo.processInfo.arguments
        if let idx = args.firstIndex(of: "--project"), idx + 1 < args.count {
            let projectPath = args[idx + 1]
            // パスの存在確認
            if FileManager.default.fileExists(atPath: projectPath) {
                openWindow(projectPath: projectPath)
            } else {
                NSLog("[VP] --project path does not exist: %@", projectPath)
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
                    openWindow(projectPath: path)
                }
            }
        }
    }

    func applicationWillTerminate(_: Notification) {
        // 全ウィンドウをクリーンアップ
        for (_, controller) in windowControllers {
            controller.close()
        }
        windowControllers.removeAll()

        userPromptService.stopPolling()
        popoverViewModel.stopAutoRefresh()
        iconTimer?.invalidate()
        if let monitor = eventMonitor {
            NSEvent.removeMonitor(monitor)
        }
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
            onCheckUpdates: { [weak self] in
                self?.closePopover()
                Task { [weak self] in
                    await self?.updateService.checkForUpdates(force: true)
                }
            },
            onSettings: { [weak self] in
                self?.closePopover()
                self?.settingsWindowController.show()
            },
            onOpenWindow: { [weak self] in
                self?.closePopover()
                self?.openWindow()
            },
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
        let ports = popoverViewModel.projects
            .filter { $0.status == .running }
            .compactMap(\.port)
        userPromptService.updateActivePorts(ports: ports)
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
