import OSLog
import SwiftUI

private let logger = Logger(subsystem: "tech.anycreative.vp", category: "Sidebar")

/// サイドバー: プロジェクト一覧
///
/// HStack ベースのカスタムサイドバー。NavigationSplitView を使わず、
/// 開閉・幅・見た目を完全に自前制御する。
struct SidebarView: View {
    let projects: [SidebarProject]
    @Binding var selection: String?
    /// TheWorld 接続ステータス
    let worldStatus: WorldStatus
    /// プロジェクト追加コールバック（＋ボタン）
    var onAdd: (() -> Void)?
    /// プロジェクト追加コールバック（ドラッグ＆ドロップ、URL 指定）
    var onDropAdd: ((URL) -> Void)?
    /// プロジェクト削除コールバック
    var onDelete: ((String) -> Void)?
    /// プロジェクト名変更コールバック
    var onRename: ((String, String) -> Void)?
    /// プロジェクト並び替えコールバック
    var onReorder: ((IndexSet, Int) -> Void)?
    /// HD リスタートコールバック（プロジェクトパス）
    var onRestartHD: ((String) -> Void)?
    /// SP リスタートコールバック（プロジェクトパス）
    var onRestartSP: ((String) -> Void)?
    /// TheWorld 再起動コールバック
    var onRestartWorld: (() -> Void)?
    /// SP 有効/無効トグルコールバック（パス, 新しい enabled 値）
    var onToggleEnabled: ((String, Bool) -> Void)?
    /// CC 通知バッジ: Lane パス → 未読フラグ
    var notifications: Set<String> = []

    /// 有効なプロジェクト（稼働中 + 停止中だが enabled）
    private var enabledProjects: [SidebarProject] {
        projects.filter { $0.enabled }
    }

    /// 無効化されたプロジェクト（enabled = false）
    private var disabledProjects: [SidebarProject] {
        projects.filter { !$0.enabled }
    }

    /// 選択中のプロジェクト名
    private var selectedProjectName: String? {
        guard let sel = selection else { return nil }
        return projects.first(where: { $0.id == sel })?.name
    }

    var body: some View {
        VStack(spacing: 0) {
            // カスタムヘッダー: 選択中プロジェクト名 + 追加ボタン
            HStack {
                if let name = selectedProjectName {
                    Text(name)
                        .font(.headline)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Spacer()
                Button {
                    onAdd?()
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundColor(.secondary)
                        .frame(width: 22, height: 22)
                        .background(
                            RoundedRectangle(cornerRadius: 5)
                                .fill(Color.primary.opacity(0.06))
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("プロジェクトフォルダを追加")
            }
            .padding(.leading, 78)  // 信号機ボタン分のオフセット
            .padding(.trailing, 12)
            .padding(.top, 6)
            .padding(.bottom, 8)

            Divider()

            // プロジェクトリスト
            List(selection: $selection) {
                // 有効なプロジェクト
                ForEach(enabledProjects) { project in
                    sidebarProjectItem(project: project)
                }
                .onMove { from, to in
                    onReorder?(from, to)
                }

                // 無効化されたプロジェクト（プロジェクト名のみ、Lane 非展開）
                if !disabledProjects.isEmpty {
                    Section("Disabled") {
                        ForEach(disabledProjects) { project in
                            SidebarProjectRow(project: project)
                                .tag(project.id)
                                .contextMenu { projectContextMenu(project: project) }
                                .opacity(0.5)
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            .scrollContentBackground(.hidden)
            .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                handleDrop(providers: providers)
            }

            // フッター: TheWorld ステータス
            Divider()
            WorldStatusFooter(status: worldStatus, onRestart: onRestartWorld)
        }
    }

    /// プロジェクト行の共通レンダリング（Project = Lead Lane + Workers）
    @ViewBuilder
    private func sidebarProjectItem(project: SidebarProject) -> some View {
        // Project 行 = Lead Lane（ブランチ・HD/PP・通知を含む）
        SidebarProjectRow(
            project: project,
            ppStatus: ppBadgeStatus(for: project),
            ccwireSession: project.ccwireSession,
            hasNotification: notifications.contains(project.path)
        )
        .tag(project.id)
        .contextMenu { projectContextMenu(project: project) }

        // Workers — 各ワーカーのブランチを表示
        ForEach(project.workers) { worker in
            SidebarWorkerRow(
                worker: worker,
                isLead: false,
                parentPPStatus: ppBadgeStatus(for: project),
                ccwireSession: worker.ccwireSession,
                hasNotification: notifications.contains(worker.path)
            )
            .tag(worker.id)
            .padding(.leading, 16)
            .contextMenu {
                Button("HD をリスタート", systemImage: "arrow.clockwise") {
                    onRestartHD?(worker.path)
                }
            }
        }
    }

    /// プロジェクト行のコンテキストメニュー
    @ViewBuilder
    private func projectContextMenu(project: SidebarProject) -> some View {
        // enable/disable トグル
        if project.enabled {
            Button("SP を停止", systemImage: "stop.circle") {
                onToggleEnabled?(project.path, false)
            }
        } else {
            Button("SP を有効化", systemImage: "play.circle") {
                onToggleEnabled?(project.path, true)
            }
        }
        Divider()
        // HD は SP 無しでも独立動作可能（SP 停止中でもリスタート可）
        Button("HD をリスタート", systemImage: "arrow.clockwise") {
            onRestartHD?(project.path)
        }
        // SP リスタートはプロセス稼働中のみ有効
        Button("SP をリスタート", systemImage: "bolt.trianglebadge.exclamationmark") {
            onRestartSP?(project.path)
        }
        .disabled(!project.isRunning)
        Divider()
        Button("名前を変更…", systemImage: "pencil") {
            promptRename(project: project)
        }
        Divider()
        Button("リストから削除", systemImage: "trash", role: .destructive) {
            onDelete?(project.path)
        }
    }

    /// フォルダのドラッグ＆ドロップ処理
    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        // 同期的にファイル URL を持つ provider があるか判定
        let fileProviders = providers.filter {
            $0.hasItemConformingToTypeIdentifier("public.file-url")
        }
        guard !fileProviders.isEmpty else { return false }

        nonisolated(unsafe) let callback = onDropAdd
        for provider in fileProviders {
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url, url.hasDirectoryPath else { return }
                DispatchQueue.main.async {
                    callback?(url)
                }
            }
        }
        return true
    }

    /// NSAlert で名前変更ダイアログを表示
    private func promptRename(project: SidebarProject) {
        let alert = NSAlert()
        alert.messageText = "プロジェクト名を変更"
        alert.informativeText = project.path
        alert.alertStyle = .informational
        alert.addButton(withTitle: "変更")
        alert.addButton(withTitle: "キャンセル")

        let textField = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        textField.stringValue = project.name
        alert.accessoryView = textField

        if alert.runModal() == .alertFirstButtonReturn {
            let newName = textField.stringValue.trimmingCharacters(in: .whitespaces)
            if !newName.isEmpty && newName != project.name {
                onRename?(project.path, newName)
            }
        }
    }
}

// MARK: - プロジェクト行（カスタムビュー）

/// サイドバーの各プロジェクト行
///
/// ステータスドット + プロジェクト名 + 開始時刻をコンパクトに表示。
/// List の selection で選択状態のハイライトは自動適用される。
struct SidebarProjectRow: View {
    let project: SidebarProject
    /// PP バッジステータス
    var ppStatus: BadgeStatus = .inactive
    /// ccwire セッション情報
    var ccwireSession: CcwireSessionInfo?
    /// CC 通知バッジ
    var hasNotification: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // 1行目: プロジェクト名 + ブランチ + 通知バッジ
            HStack(spacing: 6) {
                Image(systemName: "text.book.closed")
                    .font(.system(size: 10))
                    .foregroundStyle(.green)
                Text(project.name)
                    .fontWeight(project.isRunning ? .semibold : .regular)
                    .lineLimit(1)
                if let branch = project.branch {
                    Text(branch)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
                if hasNotification {
                    Circle()
                        .fill(.orange)
                        .frame(width: 7, height: 7)
                }
            }

            // 2行目: SP + HD + PP ステータス + 起動時刻
            HStack(spacing: 6) {
                StatusBadge(label: "SP", icon: "star", isActive: project.isRunning)
                StatusBadge(label: "HD", icon: "text.book.closed", isActive: project.hasHD)
                StatusBadge(label: "PP", icon: "compass.drawing", status: ppStatus)

                // ccwire 未読メッセージ数
                if let wire = ccwireSession, wire.pendingMessages > 0 {
                    Text("📨 \(wire.pendingMessages)")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }

                if let startedAt = project.startedAt {
                    Text(startedAt, style: .time)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .opacity(project.isRunning ? 1.0 : 0.6)
    }

    /// HD バッジのツールチップに ccwire 情報を表示
    private func ccwireTooltip(for project: SidebarProject) -> String {
        guard let wire = project.ccwireSession else {
            return "HD: \(project.hasHD ? "active" : "inactive")"
        }
        var tip = "Wire: \(wire.name) (\(wire.status))"
        if wire.pendingMessages > 0 {
            tip += "\n未読: \(wire.pendingMessages)件"
        }
        return tip
    }
}

// MARK: - ワーカー行

/// Lane（Lead / Worker）の行表示
struct SidebarWorkerRow: View {
    let worker: CcwsWorkerInfo
    /// Lead か Worker か
    var isLead: Bool = false
    /// 親プロジェクトの PP 状態を継承表示
    var parentPPStatus: BadgeStatus = .inactive
    /// ccwire セッション情報
    var ccwireSession: CcwireSessionInfo?
    /// CC 通知バッジ
    var hasNotification: Bool = false

    /// 表示ラベル（Lead-HD / Worker-HD）
    private var roleLabel: String {
        isLead ? "Lead-HD" : "Worker-HD"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // 1行目: Lane 名 + ブランチ
            HStack(spacing: 6) {
                Image(systemName: isLead ? "text.book.closed" : "arrow.branch")
                    .font(.system(size: 10))
                    .foregroundStyle(isLead ? .green : .cyan)
                Text(worker.suffix)
                    .font(.callout)
                    .fontWeight(worker.hasHD ? .semibold : .regular)
                    .lineLimit(1)
                if let branch = worker.branch {
                    Text(branch)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
                if hasNotification {
                    Circle()
                        .fill(.orange)
                        .frame(width: 7, height: 7)
                }
            }

            // 2行目: HD + PP ステータス
            HStack(spacing: 6) {
                StatusBadge(label: roleLabel, icon: "text.book.closed", isActive: worker.hasHD)
                    .help(workerCcwireTooltip)
                StatusBadge(label: "PP", icon: "compass.drawing", status: parentPPStatus)

                // ccwire 未読メッセージ数
                if let wire = ccwireSession, wire.pendingMessages > 0 {
                    Text("📨 \(wire.pendingMessages)")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }
            }
        }
        .opacity(worker.hasHD ? 1.0 : 0.6)
    }

    private var workerCcwireTooltip: String {
        guard let wire = ccwireSession else {
            return "HD: \(worker.hasHD ? "active" : "inactive")"
        }
        var tip = "Wire: \(wire.name) (\(wire.status))"
        if wire.pendingMessages > 0 {
            tip += "\n未読: \(wire.pendingMessages)件"
        }
        return tip
    }
}

/// サイドバー表示用の Stand 情報
struct SidebarStand: Equatable {
    let key: String     // "heavens_door", "paisley_park", etc.
    let status: String  // "active", "idle", "connected", "disabled"
    let detail: [String: AnyCodableValue]?

    /// Stand の SF Symbol 名（単色アイコン）
    var systemImage: String {
        switch key {
        case "heavens_door": "text.book.closed"
        case "paisley_park": "compass.drawing"
        case "gold_experience": "leaf"
        case "hermit_purple": "cable.connector"
        default: "star"
        }
    }

    /// Stand の短縮名
    var shortName: String {
        switch key {
        case "heavens_door": "HD"
        case "paisley_park": "PP"
        case "gold_experience": "GE"
        case "hermit_purple": "HP"
        default: key
        }
    }

    /// ステータス色
    var statusColor: Color {
        switch status {
        case "active": .green
        case "connected": .blue
        case "idle": .gray
        case "disabled": .gray.opacity(0.4)
        default: .gray
        }
    }
}

/// ccws ワーカー情報
struct CcwsWorkerInfo: Identifiable, Equatable {
    let id: String       // ワーカーパス
    let name: String     // ディレクトリ名全体
    let suffix: String   // 親プロジェクト名を除いた部分
    let path: String
    let branch: String?
    let hasHD: Bool      // tmux セッションが存在するか
    /// ccwire セッション情報（HD に紐づく）
    let ccwireSession: CcwireSessionInfo?
}

/// サイドバー表示用のプロジェクトモデル
struct SidebarProject: Identifiable, Equatable {
    let id: String        // プロジェクトパス（一意キー）
    let name: String
    let path: String
    let isRunning: Bool
    /// プロセスのポート番号（稼働中のみ）
    let port: UInt16?
    /// プロセス開始時刻（稼働中のみ）
    let startedAt: Date?
    /// 配下の Stand 一覧（稼働中のみ）
    let stands: [SidebarStand]
    /// ccws ワーカー一覧
    let workers: [CcwsWorkerInfo]
    /// Git ブランチ名
    let branch: String?
    /// HD（tmux セッション）が存在するか
    let hasHD: Bool

    /// CC からの未読通知あり
    let hasNotification: Bool
    /// ccwire セッション情報（HD に紐づく）
    let ccwireSession: CcwireSessionInfo?
    /// SP 自動起動の有効/無効
    let enabled: Bool

    init(id: String, name: String, path: String, isRunning: Bool, port: UInt16?, startedAt: Date?, stands: [SidebarStand] = [], workers: [CcwsWorkerInfo] = [], branch: String? = nil, hasHD: Bool = false, hasNotification: Bool = false, ccwireSession: CcwireSessionInfo? = nil, enabled: Bool = true) {
        self.id = id
        self.name = name
        self.path = path
        self.isRunning = isRunning
        self.port = port
        self.startedAt = startedAt
        self.stands = stands
        self.workers = workers
        self.branch = branch
        self.hasHD = hasHD
        self.hasNotification = hasNotification
        self.ccwireSession = ccwireSession
        self.enabled = enabled
    }

    var statusColor: Color {
        isRunning ? .green : .gray
    }
}

// MARK: - ステータスバッジ

/// Stand のステータス種別
enum BadgeStatus {
    case inactive   // 灰: 停止・利用不可
    case active     // 緑: 稼働中・利用可能
    case connected  // 青: 接続中・リアルタイム

    var color: Color {
        switch self {
        case .inactive: .gray
        case .active: .green
        case .connected: .blue
        }
    }
}

/// PP の BadgeStatus を判定
/// connected(青): Canvas WebSocket 接続中、idle(緑): show 受信可能、それ以外(灰)
private func ppBadgeStatus(for project: SidebarProject) -> BadgeStatus {
    guard project.isRunning,
          let pp = project.stands.first(where: { $0.key == "paisley_park" }) else {
        return .inactive
    }
    switch pp.status {
    case "connected": return .connected
    case "idle": return .active
    default: return .inactive
    }
}

/// SP/HD/PP のステータスを統一表示するバッジ
struct StatusBadge: View {
    let label: String
    let icon: String
    var isActive: Bool = false
    var status: BadgeStatus? = nil

    var body: some View {
        HStack(spacing: 2) {
            Image(systemName: icon)
            Text(label)
        }
        .font(.caption2)
        .foregroundStyle(resolvedColor)
    }

    private var resolvedColor: Color {
        // status が明示的に指定されていればそちらを優先
        if let status { return status.color }
        // 後方互換: isActive のみ指定
        return isActive ? .green : .gray
    }
}

// MARK: - ccws ワーカー検出

/// ~/.local/share/ccws/ をスキャンして親プロジェクトに紐づくワーカーを検出
enum CcwsDiscovery {
    /// ccws ベースディレクトリ（環境変数 CCWS_DIR で上書き可能）
    static let baseDir: URL = {
        if let envPath = ProcessInfo.processInfo.environment["CCWS_DIR"] {
            return URL(fileURLWithPath: envPath)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/share/ccws")
    }()

    /// 指定プロジェクト名に紐づくワーカーを検出
    static func discoverWorkers(forProject projectName: String) -> [CcwsWorkerInfo] {
        let prefix = "\(projectName)-"
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(
            at: baseDir,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: [.skipsHiddenFiles]
        ) else {
            return []
        }

        return entries.compactMap { url in
            let dirName = url.lastPathComponent
            guard dirName.hasPrefix(prefix) else { return nil }

            // ディレクトリか確認
            var isDir: ObjCBool = false
            guard fm.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue else {
                return nil
            }

            let suffix = String(dirName.dropFirst(prefix.count))
            let branch = readGitBranch(at: url)
            // tmux セッション名: {dirName}-vp
            let tmuxSession = dirName.replacingOccurrences(of: ".", with: "-") + "-vp"
            let hasHD = tmuxSessionExists(tmuxSession)

            return CcwsWorkerInfo(
                id: url.path,
                name: dirName,
                suffix: suffix,
                path: url.path,
                branch: branch,
                hasHD: hasHD,
                ccwireSession: nil
            )
        }
        .sorted { $0.suffix < $1.suffix }
    }

    /// tmux バイナリパスをキャッシュ（PATH から一度だけ解決）
    /// GUI アプリは PATH が制限されるため、既知パスも含めてフォールバック
    static let tmuxPath: String? = {
        // 既知パスを先にチェック（GUI アプリの PATH 制限を回避）
        for knownPath in ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"] {
            if FileManager.default.isExecutableFile(atPath: knownPath) {
                logger.debug("[VP]tmux found at: \(knownPath)")
                return knownPath
            }
        }
        // zsh -lc which tmux でフォールバック
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/zsh")
        process.arguments = ["-lc", "which tmux"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            process.waitUntilExit()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let path = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)
            if let p = path, !p.isEmpty {
                logger.debug("[VP]tmux found via zsh: \(p)")
                return p
            }
            logger.debug("[VP]tmux not found")
            return nil
        } catch {
            logger.debug("[VP]tmux search error: \(error)")
            return nil
        }
    }()

    /// tmux セッションが存在するか確認（Shell Injection 回避: tmux を直接実行）
    static func tmuxSessionExists(_ name: String) -> Bool {
        guard let tmux = tmuxPath else { return false }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tmux)
        process.arguments = ["has-session", "-t", name]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            process.waitUntilExit()
            return process.terminationStatus == 0
        } catch {
            return false
        }
    }

    /// Git ブランチ名を取得
    static func readGitBranch(at path: URL) -> String? {
        let headFile = path.appendingPathComponent(".git/HEAD")
        let gitFile = path.appendingPathComponent(".git")

        let content: String
        if FileManager.default.isReadableFile(atPath: headFile.path) {
            guard let data = try? String(contentsOf: headFile, encoding: .utf8) else { return nil }
            content = data
        } else if let gitRef = try? String(contentsOf: gitFile, encoding: .utf8),
                  let gitDir = gitRef.trimmingCharacters(in: .whitespacesAndNewlines)
                      .components(separatedBy: "gitdir: ").last {
            // git worktree: .git ファイルが gitdir を指す（相対パス対応）
            let resolvedGitDir = URL(fileURLWithPath: gitDir, relativeTo: path).standardized
            let actualHead = resolvedGitDir.appendingPathComponent("HEAD")
            guard let data = try? String(contentsOf: actualHead, encoding: .utf8) else { return nil }
            content = data
        } else {
            return nil
        }

        let trimmed = content.trimmingCharacters(in: .whitespacesAndNewlines)
        if let branch = trimmed.components(separatedBy: "ref: refs/heads/").last,
           branch != trimmed {
            return branch
        }
        // detached HEAD — 短縮 SHA
        return String(trimmed.prefix(8))
    }
}

// MARK: - TheWorld ステータス

/// TheWorld の接続状態
enum WorldStatus: Equatable {
    case connected(version: String, startedAt: Date)
    case disconnected
    case checking
}

/// サイドバーフッター: TheWorld 接続ステータス
struct WorldStatusFooter: View {
    let status: WorldStatus
    /// TheWorld 再起動アクション
    var onRestart: (() -> Void)?

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(statusColor)
                .frame(width: 6, height: 6)

            switch status {
            case .connected(let version, let startedAt):
                Text("TheWorld v\(version)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(startedAt, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                // TheWorld 再起動ボタン
                Button {
                    onRestart?()
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.caption2)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("TheWorld を再起動")
            case .disconnected:
                Text("TheWorld offline")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
            case .checking:
                Text("Connecting...")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private var statusColor: Color {
        switch status {
        case .connected: .green
        case .disconnected: .red
        case .checking: .orange
        }
    }

}

// MARK: - NSVisualEffectView ラッパー

/// AppKit の NSVisualEffectView を SwiftUI で使うためのブリッジ
///
/// NavigationSplitView が内部で使っている `.sidebar` マテリアルを
/// カスタムサイドバーでも再現する。
struct VisualEffectBackground: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    let blendingMode: NSVisualEffectView.BlendingMode

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .followsWindowActiveState
        return view
    }

    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
    }
}
