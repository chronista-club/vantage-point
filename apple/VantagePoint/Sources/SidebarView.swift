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

    var body: some View {
        List(selection: $selection) {
            ForEach(projects) { project in
                // ワーカーがあればツリー表示
                if project.workers.isEmpty {
                    SidebarProjectRow(project: project)
                        .tag(project.id)
                        .contextMenu { projectContextMenu(project: project) }
                } else {
                    SidebarProjectRow(project: project)
                        .tag(project.id)
                        .contextMenu { projectContextMenu(project: project) }

                    // ccws ワーカーをインデント表示（D&D 並び替え対象外）
                    ForEach(project.workers) { worker in
                        SidebarWorkerRow(worker: worker)
                            .tag(worker.id)
                            .padding(.leading, 20)
                            .moveDisabled(true)
                            .contextMenu {
                                Button("HD をリスタート", systemImage: "arrow.clockwise") {
                                    onRestartHD?(worker.path)
                                }
                            }
                    }
                }
            }
            .onMove { flatFrom, flatTo in
                // フラット List インデックス → projects 配列インデックスに変換
                // worker 行が混在すると List の行番号と projects のインデックスがズレるため
                var flatToProject: [Int: Int] = [:]
                var flat = 0
                for (i, project) in projects.enumerated() {
                    flatToProject[flat] = i
                    flat += 1
                    flat += project.workers.count
                }
                let projectFrom = IndexSet(flatFrom.compactMap { flatToProject[$0] })
                // flatTo が worker 行の間を指す場合、直前のプロジェクトの「後ろ」に丸める
                let projectTo: Int
                if let direct = flatToProject[flatTo] {
                    projectTo = direct
                } else {
                    var best = projects.count
                    for fi in stride(from: flatTo - 1, through: 0, by: -1) {
                        if let pi = flatToProject[fi] {
                            best = pi + 1
                            break
                        }
                    }
                    projectTo = best
                }
                guard !projectFrom.isEmpty else { return }
                onReorder?(projectFrom, projectTo)
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
        HStack(spacing: 8) {
            // ステータスドット
            Circle()
                .fill(project.statusColor)
                .frame(width: 8, height: 8)

            VStack(alignment: .leading, spacing: 1) {
                // プロジェクト名 + 通知バッジ
                HStack(spacing: 6) {
                    Text(project.name)
                        .fontWeight(project.isRunning ? .semibold : .regular)
                        .lineLimit(1)
                    if project.hasNotification {
                        Circle()
                            .fill(.orange)
                            .frame(width: 7, height: 7)
                    }
                }

                // 稼働中: 起動時刻 + Stand
                if project.isRunning {
                    if let startedAt = project.startedAt {
                        Text(startedAt, style: .time)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }

                    // Stand ステータス（active/connected のみ表示）
                    let activeStands = project.stands.filter { $0.status == "active" || $0.status == "connected" }
                    if !activeStands.isEmpty {
                        HStack(spacing: 4) {
                            ForEach(activeStands, id: \.key) { stand in
                                HStack(spacing: 2) {
                                    Image(systemName: stand.systemImage)
                                    Text(stand.shortName)
                                }
                                .font(.caption2)
                                .foregroundStyle(stand.statusColor)
                            }
                        }
                    }
                }
            }
        }
        .opacity(project.isRunning ? 1.0 : 0.5)
    }
}

// MARK: - ワーカー行

/// ccws ワーカーの行表示
struct SidebarWorkerRow: View {
    let worker: CcwsWorkerInfo

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.branch")
                .font(.caption2)
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 1) {
                Text(worker.suffix)
                    .font(.callout)
                    .lineLimit(1)

                if let branch = worker.branch {
                    Text(branch)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
        }
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

    /// CC からの未読通知あり
    let hasNotification: Bool

    init(id: String, name: String, path: String, isRunning: Bool, port: UInt16?, startedAt: Date?, stands: [SidebarStand] = [], workers: [CcwsWorkerInfo] = [], hasNotification: Bool = false) {
        self.id = id
        self.name = name
        self.path = path
        self.isRunning = isRunning
        self.port = port
        self.startedAt = startedAt
        self.stands = stands
        self.workers = workers
        self.hasNotification = hasNotification
    }

    var statusColor: Color {
        isRunning ? .green : .gray
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

            return CcwsWorkerInfo(
                id: url.path,
                name: dirName,
                suffix: suffix,
                path: url.path,
                branch: branch
            )
        }
        .sorted { $0.suffix < $1.suffix }
    }

    /// Git ブランチ名を取得
    private static func readGitBranch(at path: URL) -> String? {
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
