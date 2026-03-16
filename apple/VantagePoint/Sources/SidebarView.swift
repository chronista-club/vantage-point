import SwiftUI

/// サイドバー: プロジェクト一覧（Liquid Glass 自動適用）
///
/// NavigationSplitView のサイドバーに配置される。
/// macOS 26 では自動的に Liquid Glass マテリアルが適用される。
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

    var body: some View {
        List(selection: $selection) {
            ForEach(projects) { project in
                // ワーカーがあれば DisclosureGroup でツリー表示
                if project.workers.isEmpty {
                    SidebarProjectRow(project: project)
                        .tag(project.id)
                        .contextMenu { projectContextMenu(project: project) }
                } else {
                    DisclosureGroup {
                        ForEach(project.workers) { worker in
                            SidebarWorkerRow(worker: worker)
                                .tag(worker.id)
                                .contextMenu {
                                    Button("HD をリスタート", systemImage: "arrow.clockwise") {
                                        onRestartHD?(worker.path)
                                    }
                                }
                        }
                    } label: {
                        SidebarProjectRow(project: project)
                            .tag(project.id)
                            .contextMenu { projectContextMenu(project: project) }
                    }
                }
            }
            // DisclosureGroup により worker が ForEach のインデックス空間から外れるため
            // projects 配列と1:1対応になり、変換不要
            .onMove { from, to in
                onReorder?(from, to)
            }
        }
        .navigationTitle("Projects")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Add", systemImage: "plus") {
                    onAdd?()
                }
                .help("プロジェクトフォルダを追加")
            }
        }
        .onDrop(of: [.fileURL], isTargeted: nil) { providers in
            handleDrop(providers: providers)
        }
        .safeAreaInset(edge: .bottom) {
            WorldStatusFooter(status: worldStatus)
        }
    }

    /// プロジェクト行のコンテキストメニュー
    @ViewBuilder
    private func projectContextMenu(project: SidebarProject) -> some View {
        Button("HD をリスタート", systemImage: "arrow.clockwise") {
            onRestartHD?(project.path)
        }
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
        .disabled(project.isRunning)
    }

    /// フォルダのドラッグ＆ドロップ処理
    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        // 同期的にファイル URL を持つ provider があるか判定
        let fileProviders = providers.filter {
            $0.hasItemConformingToTypeIdentifier("public.file-url")
        }
        guard !fileProviders.isEmpty else { return false }

        for provider in fileProviders {
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url, url.hasDirectoryPath else { return }
                DispatchQueue.main.async { [onDropAdd] in
                    onDropAdd?(url)
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

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // 1行目: プロジェクト名 + ブランチ + 通知バッジ
            HStack(spacing: 6) {
                Text(project.name)
                    .fontWeight(project.isRunning ? .semibold : .regular)
                    .lineLimit(1)
                if let branch = project.branch {
                    Text(branch)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
                if project.hasNotification {
                    Circle()
                        .fill(.orange)
                        .frame(width: 7, height: 7)
                }
            }

            // 2行目: SP / Lead-HD / PP ステータス（統一表記）
            HStack(spacing: 6) {
                StatusBadge(label: "SP", icon: "star", isActive: project.isRunning)
                StatusBadge(label: "Lead-HD", icon: "text.book.closed", isActive: project.hasHD)

                // PP: SP 稼働中で disabled でなければ利用可能（緑）
                if let pp = project.stands.first(where: { $0.key == "paisley_park" }) {
                    StatusBadge(label: "PP", icon: "compass.drawing",
                                isActive: pp.status != "disabled")
                } else {
                    StatusBadge(label: "PP", icon: "compass.drawing", isActive: false)
                }

                // 起動時刻（ツールチップ）
                if let startedAt = project.startedAt {
                    Text(startedAt, style: .time)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .opacity(project.isRunning ? 1.0 : 0.6)
    }
}

// MARK: - ワーカー行

/// ccws ワーカーの行表示
struct SidebarWorkerRow: View {
    let worker: CcwsWorkerInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // 1行目: ワーカー名 + ブランチ
            HStack(spacing: 6) {
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
            }

            // 2行目: Worker-HD ステータス
            HStack(spacing: 6) {
                StatusBadge(label: "Worker-HD", icon: "text.book.closed", isActive: worker.hasHD)
            }
        }
        .opacity(worker.hasHD ? 1.0 : 0.6)
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

    init(id: String, name: String, path: String, isRunning: Bool, port: UInt16?, startedAt: Date?, stands: [SidebarStand] = [], workers: [CcwsWorkerInfo] = [], branch: String? = nil, hasHD: Bool = false, hasNotification: Bool = false) {
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
    }

    var statusColor: Color {
        isRunning ? .green : .gray
    }
}

// MARK: - ステータスバッジ

/// SP/HD/PP のステータスを統一表示するバッジ
struct StatusBadge: View {
    let label: String
    let icon: String
    let isActive: Bool

    var body: some View {
        HStack(spacing: 2) {
            Image(systemName: icon)
            Text(label)
        }
        .font(.caption2)
        .foregroundStyle(isActive ? .green : .gray)
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
                hasHD: hasHD
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
                print("[VP] tmux found at: \(knownPath)")
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
                print("[VP] tmux found via zsh: \(p)")
                return p
            }
            print("[VP] tmux not found")
            return nil
        } catch {
            print("[VP] tmux search error: \(error)")
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
