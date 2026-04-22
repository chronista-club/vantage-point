import CreoUI
import OSLog
import SwiftUI

private let logger = Logger(subsystem: "tech.anycreative.vp", category: "Sidebar")

/// サイドバー: プロジェクト一覧
///
/// HStack ベースのカスタムサイドバー。NavigationSplitView を使わず、
/// 開閉・幅・見た目を完全に自前制御する。
///
/// VP-83 Phase 1: Project Disclosure + Lane Stack 構造
/// - Project 行 = disclosure header（クリックで展開/折りたたみ）
/// - 展開時: Lead-HD 行（常時 top）+ Worker Lane 行
/// - 展開状態は @AppStorage で永続化
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

    /// サイドバー展開状態の永続化（VP-83 Phase 1）
    /// key = project.path、新規プロジェクトは default expanded
    /// 特別キー `__disabled_section__` は Disabled セクションの展開状態
    @AppStorage("vp.sidebar.expansion") private var expansionJSON: String = "{}"

    /// Disabled セクション展開状態用の特別キー
    private static let disabledSectionKey = "__disabled_section__"

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

    /// 永続化された展開状態を復元
    private var expansionState: [String: Bool] {
        guard let data = expansionJSON.data(using: .utf8),
              let dict = try? JSONDecoder().decode([String: Bool].self, from: data) else {
            return [:]
        }
        return dict
    }

    /// Project の展開状態（default: true — 新規は展開）
    private func isExpanded(for projectPath: String) -> Bool {
        expansionState[projectPath] ?? true
    }

    /// Disabled セクションの展開状態（default: false — 折りたたみ）
    private var isDisabledSectionExpanded: Bool {
        expansionState[Self.disabledSectionKey] ?? false
    }

    /// 展開状態を更新
    private func setExpanded(_ expanded: Bool, for key: String) {
        var dict = expansionState
        dict[key] = expanded
        if let data = try? JSONEncoder().encode(dict),
           let json = String(data: data, encoding: .utf8) {
            expansionJSON = json
        }
    }

    /// Project 用の Binding<Bool> を生成
    private func expansionBinding(for projectPath: String) -> Binding<Bool> {
        Binding(
            get: { isExpanded(for: projectPath) },
            set: { setExpanded($0, for: projectPath) }
        )
    }

    /// Disabled セクション用の Binding<Bool>
    private var disabledSectionBinding: Binding<Bool> {
        Binding(
            get: { isDisabledSectionExpanded },
            set: { setExpanded($0, for: Self.disabledSectionKey) }
        )
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
                        .foregroundColor(Color.colorTextSecondary)
                        .frame(width: 22, height: 22)
                        .background(
                            RoundedRectangle(cornerRadius: CreoUITokens.radiusSm)
                                .fill(Color.colorSurfaceBgEmphasis)
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
            // VP-83 Phase 1: DisclosureGroup ベースの Lane Stack
            List(selection: $selection) {
                // 有効なプロジェクト（展開可能な disclosure header）
                ForEach(enabledProjects) { project in
                    sidebarProjectDisclosure(project: project)
                }
                .onMove { from, to in
                    onReorder?(from, to)
                }

                // 無効化されたプロジェクト（Disabled セクション、default 折りたたみ）
                if !disabledProjects.isEmpty {
                    DisclosureGroup(isExpanded: disabledSectionBinding) {
                        ForEach(disabledProjects) { project in
                            SidebarProjectHeaderRow(project: project, ppStatus: .inactive)
                                .tag(project.id)
                                .contextMenu { projectContextMenu(project: project) }
                                .opacity(0.5)
                        }
                    } label: {
                        Text("Disabled")
                            .font(.caption)
                            .foregroundStyle(Color.colorTextTertiary)
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

    /// プロジェクトの disclosure 表示
    ///
    /// Header: Project 名 + status dot + chevron（SidebarProjectHeaderRow）
    /// Content:
    ///   - Lead-HD 行（常時 top、明示 row）— SidebarLeadRow
    ///   - Worker 行 — SidebarWorkerRow
    @ViewBuilder
    private func sidebarProjectDisclosure(project: SidebarProject) -> some View {
        DisclosureGroup(isExpanded: expansionBinding(for: project.path)) {
            // Lead-HD 行（常時 top）
            SidebarLeadRow(
                project: project,
                ppStatus: ppBadgeStatus(for: project),
                ccwireSession: project.ccwireSession,
                hasNotification: notifications.contains(project.path)
            )
            .tag(project.id)
            .contextMenu { projectContextMenu(project: project) }

            // Worker Lane 行
            ForEach(project.workers) { worker in
                SidebarWorkerRow(
                    worker: worker,
                    isLead: false,
                    parentProjectName: project.name,
                    parentPPStatus: ppBadgeStatus(for: project),
                    ccwireSession: worker.ccwireSession,
                    hasNotification: notifications.contains(worker.path)
                )
                .tag(worker.id)
                .contextMenu {
                    Button("HD をリスタート", systemImage: "arrow.clockwise") {
                        onRestartHD?(worker.path)
                    }
                }
            }
        } label: {
            SidebarProjectHeaderRow(
                project: project,
                ppStatus: ppBadgeStatus(for: project)
            )
            .contextMenu { projectContextMenu(project: project) }
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

/// サイドバーの Project disclosure header 行（VP-83 Phase 1）
///
/// Disclosure の label として使われる。コンパクトな project identity のみ表示:
/// - プロジェクト名（強調）
/// - SP 稼働ステータスドット
/// - 起動時刻（稼働中のみ）
///
/// 詳細な HD/PP/branch 情報は展開後の SidebarLeadRow に移譲。
struct SidebarProjectHeaderRow: View {
    let project: SidebarProject
    /// PP バッジステータス（disclosure header では SP のみ表示するが、将来の拡張用）
    var ppStatus: BadgeStatus = .inactive

    var body: some View {
        HStack(spacing: 6) {
            // SP status dot（稼働中: 緑、停止中: 灰）
            Circle()
                .fill(project.isRunning ? Color.colorSemanticSuccess : Color.colorTextTertiary)
                .frame(width: 7, height: 7)

            Text(project.name)
                .fontWeight(project.isRunning ? .semibold : .regular)
                .lineLimit(1)

            Spacer()

            // 稼働時刻（稼働中のみ）
            if let startedAt = project.startedAt {
                Text(startedAt, style: .time)
                    .font(.caption2)
                    .foregroundStyle(Color.colorTextTertiary)
            }
        }
        .opacity(project.isRunning ? 1.0 : 0.6)
    }
}

/// Lead Lane 行（VP-83 Phase 1）
///
/// Project disclosure 展開時の top row として表示される。
/// Lead-HD の branch + SP/HD/PP badges + 通知バッジを持つ。
/// 従来の SidebarProjectRow の詳細情報部分を継承。
struct SidebarLeadRow: View {
    let project: SidebarProject
    /// PP バッジステータス
    var ppStatus: BadgeStatus = .inactive
    /// ccwire セッション情報
    var ccwireSession: CcwireSessionInfo?
    /// CC 通知バッジ
    var hasNotification: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // 1行目: branch primary (Stand icon / "Lead-HD" label オミット、position で Lead 識別)
            HStack(spacing: 6) {
                if let branch = project.branch {
                    Text(branch)
                        .font(.callout)
                        .fontWeight(project.hasHD ? .semibold : .regular)
                        .foregroundStyle(Color.colorTextPrimary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                } else {
                    Text("(no branch)")
                        .font(.callout)
                        .foregroundStyle(Color.colorTextTertiary)
                }
                Spacer()
            }

            // 2行目: Lane-lead address + msgbox 状況 + 未読 (SP/PP は Phase 2 Drawer へ)
            UnifiedStatusBadge(
                laneActor: leadActor,
                unreadCount: Int(ccwireSession?.pendingMessages ?? 0),
                hasNotification: hasNotification,
                wireStatus: ccwireSession?.status
            )
        }
        .opacity(project.hasHD ? 1.0 : 0.6)
    }

    /// Lead Lane を代表する lane-lead actor: `hd.lead@{project}`
    private var leadActor: StandRef {
        StandRef(
            status: project.hasHD ? .active : .inactive,
            address: "hd.lead@\(project.name)",
            displayName: "Heaven's Door (Lead)"
        )
    }
}

// MARK: - ワーカー行

/// Lane（Lead / Worker）の行表示
struct SidebarWorkerRow: View {
    let worker: CcwsWorkerInfo
    /// Lead か Worker か (VP-83 Phase 1 以降は position で区別、historical flag として残存)
    var isLead: Bool = false
    /// 親プロジェクト名 (address 生成用: `hd.w-83@{parentProject}`)
    var parentProjectName: String = ""
    /// 親プロジェクトの PP 状態を継承表示
    var parentPPStatus: BadgeStatus = .inactive
    /// ccwire セッション情報
    var ccwireSession: CcwireSessionInfo?
    /// CC 通知バッジ
    var hasNotification: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            // 1行目: worker suffix (primary) + branch (secondary)。icon / role label オミット
            HStack(spacing: 6) {
                Text(worker.suffix)
                    .font(.callout)
                    .fontWeight(worker.hasHD ? .semibold : .regular)
                    .foregroundStyle(Color.colorTextPrimary)
                    .lineLimit(1)
                if let branch = worker.branch {
                    Text(branch)
                        .font(.caption2)
                        .foregroundStyle(Color.colorTextTertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
            }

            // 2行目: Lane-lead address + msgbox 状況 + 未読 (PP は Phase 2 Drawer へ)
            UnifiedStatusBadge(
                laneActor: workerLaneActor,
                unreadCount: Int(ccwireSession?.pendingMessages ?? 0),
                hasNotification: hasNotification,
                wireStatus: ccwireSession?.status
            )
        }
        .opacity(worker.hasHD ? 1.0 : 0.6)
    }

    /// Worker Lane を代表する lane-lead actor: `hd.{suffix}@{project}`
    ///
    /// worker.suffix = 親 project 名を除いた lane 識別子 (例: "vp-83"、"maru-42")
    private var workerLaneActor: StandRef {
        let proj = parentProjectName.isEmpty ? "?" : parentProjectName
        let lane = worker.suffix
        return StandRef(
            status: worker.hasHD ? .active : .inactive,
            address: "hd.\(lane)@\(proj)",
            displayName: "Heaven's Door (\(lane))"
        )
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
        case "active": Color.colorSemanticSuccess
        case "connected": Color.colorSemanticInfo
        case "idle": Color.colorTextTertiary
        case "disabled": Color.colorTextDisabled
        default: Color.colorTextTertiary
        }
    }
}

// MARK: - 統合 status cluster (VP-83 Phase 1 refinement)

/// Stand の address reference + 状態
///
/// Sidebar の dot 1 つ = 1 Stand actor への entry point。
/// VP-77 §7.1 ActorRef canonical form `{stand}.{lane}@{project}` に準拠。
/// tooltip で address 確認、click で clipboard copy。
struct StandRef {
    let status: BadgeStatus
    /// Actor address (canonical form、short but unique): `sp@vp`, `hd.lead@vp`, `pp.w-83@vp`
    let address: String
    /// tooltip 表示名 (human-readable): "Heaven's Door (Lead)"
    let displayName: String
}

/// Stand の address button。monospaced text で短縮形表示、
/// color で status、click で full address (`{stand}.{lane}@{project}`) clipboard copy。
/// hover で full canonical form を tooltip 表示。
struct StandDotButton: View {
    let stand: StandRef

    /// Sidebar 表示用の短縮形 (project 部分を暗黙化、例: `hd.lead@vp` → `hd.lead`)
    private var shortForm: String {
        if let atIdx = stand.address.firstIndex(of: "@") {
            return String(stand.address[..<atIdx])
        }
        return stand.address
    }

    var body: some View {
        Text(shortForm)
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(stand.status.color)
            .lineLimit(1)
            .contentShape(Rectangle())
            .help("\(stand.displayName)\n\(stand.address) — click to copy")
            .onTapGesture {
                copyAddress()
            }
            .contextMenu {
                Button {
                    copyAddress()
                } label: {
                    Label("Copy \(stand.address)", systemImage: "doc.on.doc")
                }
            }
    }

    private func copyAddress() {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(stand.address, forType: .string)
    }
}

/// Lane-lead actor + msgbox 状況 + 通知の統合 cluster (VP-83 Phase 1 refinement 4)
///
/// 構成:
/// 1. lane-lead address (monospaced text、click で copy)
/// 2. msgbox 活動状態 (wire status dot、connected=info blue / idle=gray / stale=error red)
/// 3. 未読 count or 通知 indicator (warning orange)
///
/// SP / PP の個別 status と cache 管理は Phase 2 Right Drawer に移行予定。
struct UnifiedStatusBadge: View {
    /// Lane 自身を代表する lane-lead actor (= HD actor)
    let laneActor: StandRef
    var unreadCount: Int = 0
    var hasNotification: Bool = false
    /// msgbox / ccwire 状態 ("connected" / "idle" / "disconnected" / "stale" / nil)
    var wireStatus: String? = nil

    var body: some View {
        HStack(spacing: 6) {
            StandDotButton(stand: laneActor)

            // msgbox 活動状態 (wire status)
            if let status = wireStatus {
                Circle()
                    .fill(msgboxColor(for: status))
                    .frame(width: 5, height: 5)
                    .help("msgbox: \(status)")
            }

            // 未読 count / 通知
            if unreadCount > 0 {
                Text("\(unreadCount)")
                    .font(.caption2)
                    .foregroundStyle(Color.colorSemanticWarning)
                    .padding(.leading, 2)
            } else if hasNotification {
                Circle()
                    .fill(Color.colorSemanticWarning)
                    .frame(width: 6, height: 6)
                    .padding(.leading, 2)
            }
        }
    }

    /// wire/msgbox status を色に map
    private func msgboxColor(for status: String) -> Color {
        switch status.lowercased() {
        case "connected", "active": return Color.colorSemanticInfo
        case "idle": return Color.colorTextTertiary
        case "disconnected", "stale", "error": return Color.colorSemanticError
        default: return Color.colorTextTertiary
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
        isRunning ? Color.colorSemanticSuccess : Color.colorTextTertiary
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
        case .inactive: Color.colorTextTertiary
        case .active: Color.colorSemanticSuccess
        case .connected: Color.colorSemanticInfo
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
        return isActive ? Color.colorSemanticSuccess : Color.colorTextTertiary
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
        case .connected: Color.colorSemanticSuccess
        case .disconnected: Color.colorSemanticError
        case .checking: Color.colorSemanticWarning
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
