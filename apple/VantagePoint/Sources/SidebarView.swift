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

    var body: some View {
        List(projects, selection: $selection) { project in
            SidebarProjectRow(project: project)
                .tag(project.id)
                .contextMenu {
                    // 名前変更（稼働中でも可）
                    Button("名前を変更…", systemImage: "pencil") {
                        // NSAlert でテキスト入力（SwiftUI の alert だと List 内で崩れるため）
                        promptRename(project: project)
                    }
                    Divider()
                    // 削除（稼働中は不可）
                    Button("リストから削除", systemImage: "trash", role: .destructive) {
                        onDelete?(project.path)
                    }
                    .disabled(project.isRunning)
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
        VStack(alignment: .leading, spacing: 1) {
                // プロジェクト名
                Text(project.name)
                    .fontWeight(project.isRunning ? .semibold : .regular)
                    .lineLimit(1)

                // 稼働中: ポート + 起動時刻 + Stand
                if project.isRunning {
                    HStack(spacing: 4) {
                        if let port = project.port {
                            Text(":\(port)")
                        }
                        if let startedAt = project.startedAt {
                            Text("· \(startedAt, style: .time)")
                        }
                    }
                    .font(.caption2)
                    .foregroundStyle(.secondary)

                    // Stand ステータス（disabled 以外を表示）
                    let activeStands = project.stands.filter { $0.status != "disabled" }
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
            Spacer()
        }
        .opacity(project.isRunning ? 1.0 : 0.5)
    }
}

/// サイドバー表示用の Stand 情報
struct SidebarStand: Equatable {
    let key: String     // "heavens_door", "paisley_park", etc.
    let status: String  // "active", "idle", "connected", "disabled"
    let detail: [String: Int]?

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

    init(id: String, name: String, path: String, isRunning: Bool, port: UInt16?, startedAt: Date?, stands: [SidebarStand] = []) {
        self.id = id
        self.name = name
        self.path = path
        self.isRunning = isRunning
        self.port = port
        self.startedAt = startedAt
        self.stands = stands
    }

    var statusColor: Color {
        isRunning ? .green : .gray
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
