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

    /// Design token override store (Design Inspector で live edit 可能)
    @State private var tokens = DesignTokenStore.shared
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

    /// 選択中プロジェクト (Lead 選択時 / Worker 選択時の親 project)
    private var selectedProject: SidebarProject? {
        guard let sel = selection else { return nil }
        if let p = projects.first(where: { $0.id == sel }) {
            return p
        }
        return projects.first(where: { proj in
            proj.workers.contains(where: { $0.id == sel })
        })
    }

    /// 選択中プロジェクトの display title
    /// - displayName (user 設定) があれば優先
    /// - なければ slug (`vantage-point`) を `Vantage Point` に format
    private var selectedProjectTitle: String {
        guard let p = selectedProject else { return "" }
        return p.displayTitle
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
            // カスタムヘッダー: + ボタンのみ (選択中 Project 名は冗長のため削除、
            // 選択中 Lane は Sidebar 内行で highlight される)
            HStack(spacing: 8) {
                // 選択中 Project の display title (VP-83 refinement 30)
                Text(selectedProjectTitle)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.colorTextPrimary)
                    .lineLimit(1)
                    .truncationMode(.tail)

                Spacer(minLength: 8)

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
            .padding(.leading, 78)  // 信号機ボタン分のオフセット (左にドラッグ area 確保)
            .padding(.trailing, 12)
            .padding(.top, 6)
            .padding(.bottom, 8)

            Divider()

            // プロジェクトリスト
            // VP-83 Phase 1 refinement 16: sharp stack — .plain + listRowInsets 0
            // .sidebar style は auto card 化 (rounded outline + row gap) するため廃止
            List(selection: $selection) {
                // 有効なプロジェクト（展開可能な disclosure header）
                // refinement 55.2: .onMove は Custom DisclosureGroupStyle と併用すると
                // 発火しないため、.draggable + .dropDestination で手動 D&D に切替。
                // project.path を String として搬送、drop target で index 解決して reorder。
                ForEach(enabledProjects) { project in
                    sidebarProjectDisclosure(project: project)
                        .listRowInsets(EdgeInsets(
                            top: 0,
                            leading: tokens.sidebarListRowLeadingInset,
                            bottom: 0,
                            trailing: tokens.sidebarListRowTrailingInset
                        ))
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                }
                // refinement 55: .onMove で行間挿入 indicator UI、index ずれ fix は
                // MainWindowView.reorderProjects 側で enabled/disabled 分離して対応
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
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            // macOS 14+ API: scroll content の系統的 horizontal margin を 0 に。
            // これで Project card bg が window/sidebar edge にピタッと到達
            .contentMargins(.horizontal, 0, for: .scrollContent)
            // macOS List の system selection overlay (blue) を透明化
            .tint(.clear)
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
            // refinement 50: .tag が DisclosureGroupStyle 層で block される問題、
            // manual onTapGesture で selection 直接 set に変更
            SidebarLeadRow(
                project: project,
                ppStatus: ppBadgeStatus(for: project),
                msgboxSession: project.msgboxSession,
                hasNotification: notifications.contains(project.path),
                isFocused: selection == project.id
            )
            .tag(project.id)
            .listRowInsets(EdgeInsets())
            .listRowBackground(laneRowBackground(isFocused: selection == project.id))
            .contentShape(Rectangle())
            // refinement 55: D&D 並び替え (onMove) と tap 選択を両立
            .simultaneousGesture(TapGesture().onEnded { selection = project.id })
            .contextMenu { projectContextMenu(project: project) }

            // Worker Lane 行
            ForEach(project.workers) { worker in
                SidebarWorkerRow(
                    worker: worker,
                    isLead: false,
                    parentProjectName: project.name,
                    parentPPStatus: ppBadgeStatus(for: project),
                    msgboxSession: worker.msgboxSession,
                    hasNotification: notifications.contains(worker.path),
                    isFocused: selection == worker.id
                )
                .tag(worker.id)
                .listRowInsets(EdgeInsets())
                .listRowBackground(laneRowBackground(isFocused: selection == worker.id))
                .contentShape(Rectangle())
                .simultaneousGesture(TapGesture().onEnded { selection = worker.id })
                .contextMenu {
                    Button("エージェントを再起動", systemImage: "arrow.clockwise") {
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
        .disclosureGroupStyle(RightChevronDisclosureStyle(
            tokens: tokens,
            isProjectFocused: isProjectFocused(project)
        ))
    }

    /// Project の配下に focused Lane があるか判定 (selection が Lead or Worker の tag と一致)
    private func isProjectFocused(_ project: SidebarProject) -> Bool {
        if selection == project.id { return true }
        return project.workers.contains { selection == $0.id }
    }

    /// Lane row の背景 (sharp 矩形、角丸なし)
    ///
    /// VP-83 refinement 15: List の default selection (rounded rect) を上書きし、
    /// sharp stack 思想に合わせて pad/margin 0 の矩形 highlight を描画
    @ViewBuilder
    private func laneRowBackground(isFocused: Bool) -> some View {
        Rectangle()
            .fill(isFocused ? Color.colorSemanticSuccess.opacity(tokens.sidebarLaneFocusOpacity) : Color.clear)
    }

    /// プロジェクト行のコンテキストメニュー
    @ViewBuilder
    private func projectContextMenu(project: SidebarProject) -> some View {
        // enable/disable トグル
        if project.enabled {
            Button("プロジェクトを停止", systemImage: "stop.circle") {
                onToggleEnabled?(project.path, false)
            }
        } else {
            Button("プロジェクトを有効化", systemImage: "play.circle") {
                onToggleEnabled?(project.path, true)
            }
        }
        Divider()
        // エージェントはプロジェクト停止中でも独立起動可
        Button("エージェントを再起動", systemImage: "arrow.clockwise") {
            onRestartHD?(project.path)
        }
        // プロジェクト再起動は稼働中のみ有効
        Button("プロジェクトを再起動", systemImage: "bolt.trianglebadge.exclamationmark") {
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

/// サイドバーの Project disclosure header 行（VP-83 Phase 1、refinement 7）
///
/// Disclosure の label として使われる。コンパクトな project identity:
/// - SP を address 形式 (`sp@{project}`) で表示 (Lane row と同じ address 語彙)
/// - プロジェクト名（強調）
/// - 起動時刻（Phase 2 Right Drawer 移行予定）
///
/// 詳細な HD/PP/branch/status 情報は展開後の SidebarLeadRow + Phase 2 Drawer に移譲。
struct SidebarProjectHeaderRow: View {
    let project: SidebarProject
    /// PP バッジステータス（将来の拡張用、現状未使用）
    var ppStatus: BadgeStatus = .inactive

    var body: some View {
        // VP-83 refinement 13: accent bar 廃止
        // 理由: sp@{project} address の色 (active=緑/inactive=gray) と
        // 完全に redundant、さらに Lane focus light と同色 (緑) で視覚的に紛らわしい。
        // 廃止により「光る緑縦線 = Lane focus」の identity が明確化。
        // 稼働状態は Title の bold/semibold + address の色で十分伝わる。
        VStack(alignment: .leading, spacing: 2) {
            Text(project.name)
                .font(.headline)
                .fontWeight(project.isRunning ? .bold : .semibold)
                .lineLimit(1)

            // SP actor address (lane-lead address と同じ vocabulary)
            StandDotButton(stand: spStand)
        }
        .opacity(project.isRunning ? 1.0 : 0.55)
    }

    /// Star Platinum actor (project server、lane 概念外)
    ///
    /// basic form: `sp@{project}` (Lane の actor address と同系)
    /// Editor mode で `sp` に短縮
    private var spStand: StandRef {
        StandRef(
            status: project.isRunning ? .active : .inactive,
            address: "sp@\(project.name)",
            displayName: "Star Platinum (Project Server)"
        )
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
    var msgboxSession: MsgboxSessionInfo?
    /// CC 通知バッジ
    var hasNotification: Bool = false
    /// 現在 focus されているか (VP-83 refinement 12: focus light)
    var isFocused: Bool = false

    var body: some View {
        // VP-83 refinement 46: Lane row を 2 ブロック化
        //   上段: [icon] + [L1 Lane 名 / L2 branch+address] + [focus pill]
        //   下段: L3 通知層 (幅いっぱい、icon の下から)
        VStack(alignment: .leading, spacing: 3) {
            // 上段
            HStack(spacing: 0) {
                LaneRootPaneIcon(systemImage: "book.pages", status: laneStatus, isFocused: isFocused)
                    .padding(.trailing, 6)

                VStack(alignment: .leading, spacing: 2) {
                    // L1 (可変): Lane 名
                    Text(laneDisplayName)
                        .font(.callout)
                        .fontWeight(project.hasHD ? .semibold : .regular)
                        .foregroundStyle(Color.colorTextPrimary)
                        .lineLimit(1)

                    // L2 (固定): branch + address
                    HStack(spacing: 8) {
                        if let branch = project.branch {
                            Text(branch.smartHead(tailLimit: 12))
                                .font(.caption2)
                                .foregroundStyle(Color.colorTextTertiary)
                                .lineLimit(1)
                                .help(branch)
                        }
                        StandDotButton(stand: leadActor, forceShort: true)
                    }
                }

                Spacer(minLength: 0)

                LaneStatusBar(isFocused: isFocused)
                    .padding(.leading, 6)
                    .padding(.trailing, 6)
            }

            // 下段: L3 通知層 (幅いっぱい、上段と切り離し)
            LaneNotificationRow(
                unreadCount: Int(msgboxSession?.pendingMessages ?? 0),
                hasNotification: hasNotification,
                wireStatus: msgboxSession?.status,
                recentMessages: project.recentMessages
            )
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .opacity(project.hasHD ? 1.0 : 0.6)
    }

    /// Lane の現在状態 (HD / ccwire / notification から導出)
    private var laneStatus: LaneStatus {
        if !project.hasHD { return .inactive }
        if hasNotification { return .notification }
        switch msgboxSession?.status {
        case "connected": return .active
        case "idle": return .idle
        case "stale", "disconnected": return .error
        default: return .active
        }
    }

    /// Lane 名 (L1 primary) — "Lead" identity は常に visible、session title は suffix
    /// refinement 48: Lead が session title に埋もれ worker と混同される問題 fix
    private var laneDisplayName: String {
        if let title = project.ccSessionTitle, !title.isEmpty {
            return "Lead · \(title)"
        }
        return "Lead"
    }

    /// Lead Lane を代表する lane-lead actor: `hd.lead@{project}`
    /// displayName は tooltip 表示 — layer 2 で JoJo 名 (Heaven's Door) を併記
    private var leadActor: StandRef {
        StandRef(
            status: project.hasHD ? .active : .inactive,
            address: "hd.lead@\(project.name)",
            displayName: "Lead Agent — Heaven's Door 📖"
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
    var msgboxSession: MsgboxSessionInfo?
    /// CC 通知バッジ
    var hasNotification: Bool = false
    /// 現在 focus されているか (VP-83 refinement 12: focus light)
    var isFocused: Bool = false

    var body: some View {
        // VP-83 refinement 46: Lane row を 2 ブロック化
        VStack(alignment: .leading, spacing: 3) {
            // 上段
            HStack(spacing: 0) {
                LaneRootPaneIcon(systemImage: "book.pages", status: laneStatus, isFocused: isFocused)
                    .padding(.trailing, 6)

                VStack(alignment: .leading, spacing: 2) {
                    Text(workerLaneDisplayName)
                        .font(.callout)
                        .fontWeight(worker.hasHD ? .semibold : .regular)
                        .foregroundStyle(Color.colorTextPrimary)
                        .lineLimit(1)

                    HStack(spacing: 8) {
                        if let branch = worker.branch {
                            Text(branch.smartHead(tailLimit: 12))
                                .font(.caption2)
                                .foregroundStyle(Color.colorTextTertiary)
                                .lineLimit(1)
                                .help(branch)
                        }
                        StandDotButton(stand: workerLaneActor, forceShort: true)
                    }
                }

                Spacer(minLength: 0)

                LaneStatusBar(isFocused: isFocused)
                    .padding(.leading, 6)
                    .padding(.trailing, 6)
            }

            // 下段: L3 通知層 (幅いっぱい)
            LaneNotificationRow(
                unreadCount: Int(msgboxSession?.pendingMessages ?? 0),
                hasNotification: hasNotification,
                wireStatus: msgboxSession?.status
            )
        }
        .padding(.vertical, 4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .opacity(worker.hasHD ? 1.0 : 0.6)
    }

    /// Lane の現在状態 (HD / ccwire / notification から導出)
    private var laneStatus: LaneStatus {
        if !worker.hasHD { return .inactive }
        if hasNotification { return .notification }
        switch msgboxSession?.status {
        case "connected": return .active
        case "idle": return .idle
        case "stale", "disconnected": return .error
        default: return .active
        }
    }

    /// Worker Lane の L1 primary — CC session title 優先、なければ worker.suffix
    private var workerLaneDisplayName: String {
        if let title = worker.ccSessionTitle, !title.isEmpty {
            return title
        }
        return worker.suffix
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
            displayName: "Agent \(lane) — Heaven's Door 📖"
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

// MARK: - Disclosure style (web accordion 風、2-state design)

/// VP Project Accordion — closed/open の 2 state をデザインとして成立させた
/// DisclosureGroup style (VP-83 Phase 1 refinement 12)
///
/// ## Closed State
/// ```
/// [ label                                  ▷ ]
/// ```
/// - chevron: right、tertiary text
/// - background: transparent
///
/// ## Open State
/// ```
/// [ label                                  ▽ ]   ← subtle bg
/// [ │  content ............................ ]   ← left rail (tree branch)
/// ```
/// - chevron: 90° rotate (down)
/// - header: 薄い bg tint で "active" を示す
/// - content: 左 rail (1pt vertical line) で tree を表現
///
/// ## Motion
/// - chevron rotation: easeInOut 220ms
/// - content: List が自動で row insert/remove を animate
/// - header bg: transition 180ms
struct RightChevronDisclosureStyle: DisclosureGroupStyle {
    /// Expand/shrink アニメーション
    private static let expandAnimation: Animation = .smooth(duration: 0.28, extraBounce: 0)

    /// Design token store (Inspector で live edit)
    let tokens: DesignTokenStore

    /// この Project の配下に focused Lane があるか (selection の親かどうか)
    /// VP-83 refinement 29: 選択中のみ濃く、他の open project は dim (40%) 表示
    var isProjectFocused: Bool = false

    /// 非選択 Project の tint dim 係数
    private static let dimFactor: Double = 0.4

    func makeBody(configuration: Configuration) -> some View {
        // Open 時 Project card 全体を subtle tint で塗り、header + content が
        // 同じ area にまとまっているように見せる (VP-83 refinement 18)
        //
        // 階層感 (bg opacity で 4 段階):
        //  - Closed project:        0.00  (transparent)
        //  - Open project card:     0.14  (area 全体、ベース tint)
        //  - Open project header:   0.14 + 0.22 = 0.36 (base + overlay)
        //  - Focused Lane row:      0.14 + 0.50 ≒ 0.64 (base + focus highlight)
        VStack(alignment: .leading, spacing: 0) {
            // Header row — chevron オミット、header 全体が tap area
            HStack(spacing: 6) {
                // bg は edge 到達、text だけ tokens.sidebarHeaderTextLeading inset
                configuration.label
                    .padding(.leading, tokens.sidebarHeaderTextLeading)
                Spacer(minLength: 0)
            }
            .padding(.vertical, CreoUITokens.spacingXs + 2)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                // Open 時のみ header extra tint (緑系、tokens driven)
                // 選択中のみ full、非選択 open は dimFactor で弱める
                Color.colorSemanticSuccess
                    .opacity(headerOverlayOpacity(isExpanded: configuration.isExpanded))
                    .animation(Self.expandAnimation, value: configuration.isExpanded)
            )
            .contentShape(Rectangle())
            // refinement 55: .onTapGesture だと drag gesture を intercept、
            // List の .onMove が発火しなくなるため .simultaneousGesture に変更
            .simultaneousGesture(
                TapGesture().onEnded {
                    withAnimation(Self.expandAnimation) {
                        configuration.isExpanded.toggle()
                    }
                }
            )

            // Content area — Open 時のみ表示
            // refinement 49: VStack wrap を撤廃 — List(selection:) が children の
            // .tag を row 個別に認識できなくなる問題の fix。
            // List 内で使う前提、各 row の .listRowInsets が効くよう raw content 直出し。
            if configuration.isExpanded {
                configuration.content
            }

            // Project 間の sharp boundary (hairline divider)
            Rectangle()
                .fill(Color.colorSurfaceBorderSubtle)
                .frame(height: 1)
                .frame(maxWidth: .infinity)
        }
        // Project card 全体の base tint (open 時、tokens driven、selection-aware)
        .background(
            Color.colorSemanticSuccess
                .opacity(cardBaseOpacity(isExpanded: configuration.isExpanded))
                .animation(Self.expandAnimation, value: configuration.isExpanded)
        )
    }

    /// Card base tint opacity (選択中 full / 非選択 open dim)
    private func cardBaseOpacity(isExpanded: Bool) -> Double {
        guard isExpanded else { return 0 }
        let base = tokens.sidebarCardBaseOpacity
        return isProjectFocused ? base : base * Self.dimFactor
    }

    /// Header overlay tint opacity (同上)
    private func headerOverlayOpacity(isExpanded: Bool) -> Double {
        guard isExpanded else { return 0 }
        let base = tokens.sidebarHeaderOverlayOpacity
        return isProjectFocused ? base : base * Self.dimFactor
    }
}

// MARK: - Focus light (lane 選択インジケータ)

/// Lane row の leading に描画する focus light
///
/// VP-83 refinement 12: "縦のラインで左側に光っている、縦のラインよりちょっと短く、
/// ちょっと内側にパディングされたような位置" を実現する
///
/// - width: 2.5pt (rail の 1pt より太く、存在感あり)
/// - height: row の 65% 程度 (rail より短い、vertical padding で削る)
/// - offset: rail より内側 (content 側)、row の leading edge 付近
/// - glow: subtle shadow で "光っている" 感
// MARK: - Lane Status (VP-83 refinement 32)

/// Lane の状態 — 色で表現し、右端 `LaneStatusBar` で可視化
enum LaneStatus: Equatable {
    case active        // HD 稼働 + ccwire connected (通常状態)
    case idle          // HD 稼働 + idle (reply 待ち等)
    case notification  // 未読 / attention 要請あり
    case inactive      // HD 未稼働
    case error         // stale / disconnected / failure

    var color: Color {
        switch self {
        case .active: return .colorSemanticSuccess
        case .idle: return .colorTextTertiary
        case .notification: return .colorSemanticWarning
        case .inactive: return .colorSurfaceBorderSubtle
        case .error: return .colorSemanticError
        }
    }

    var baseOpacity: Double {
        switch self {
        case .inactive: return 0.35
        default: return 1.0
        }
    }
}

/// Lane の focus marker — Lane row 右端、**選択中かどうか** のみを示す
///
/// VP-83 refinement 38: Agent status 表現は LaneRootPaneIcon に移譲。
/// このバーは「selected Lane の右端に発光する緑 pill」として役割固定。
///
/// - isFocused=true: 緑 pill + glow
/// - isFocused=false: 非表示 (width 0 + opacity 0)
struct LaneStatusBar: View {
    let isFocused: Bool

    var body: some View {
        RoundedRectangle(cornerRadius: 2)
            .fill(Color.colorSemanticSuccess)
            .frame(width: isFocused ? 4 : 0)
            .padding(.vertical, 3)
            .opacity(isFocused ? 1 : 0)
            .shadow(
                color: isFocused ? Color.colorSemanticSuccess.opacity(0.7) : .clear,
                radius: isFocused ? 4 : 0,
                x: 0, y: 0
            )
            .animation(.easeInOut(duration: 0.22), value: isFocused)
    }
}

// MARK: - Lane root pane icon (VP-83 refinement 31)

/// Lane の root pane (代表 pane) を示す icon badge
///
/// 現状 Lane = HD single pane が default だが、VP-77 Lane-as-Process では
/// Lane が複数 pane を持つ pane stack。その **root = 最上位** pane を
/// icon で Sidebar に表示、Lane の中身が一目で分かる。
///
/// - 下地: 角丸 15pt の rounded rect (colorSurfaceBgEmphasis subtle)
/// - padding: 内側 8pt で icon の周囲に呼吸空間
/// - icon: SF Symbol (HD = `text.book.closed`、将来 pane kind 別)
struct LaneRootPaneIcon: View {
    let systemImage: String
    var status: LaneStatus = .inactive
    var isFocused: Bool = false

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: 13, weight: .medium))
            .foregroundStyle(iconColor)
            .frame(width: 14, height: 14)
            .padding(8)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(backgroundColor)
            )
            .shadow(
                color: glowColor,
                radius: isFocused ? 3 : 0, x: 0, y: 0
            )
            .animation(.easeInOut(duration: 0.22), value: status)
            .animation(.easeInOut(duration: 0.22), value: isFocused)
    }

    /// icon foreground — status.color を solid (inactive のみ tertiary)
    private var iconColor: Color {
        switch status {
        case .inactive: Color.colorTextTertiary
        default:        status.color
        }
    }

    /// 下地 tint — status color を低 opacity で (status の tone を area で示す)
    private var backgroundColor: Color {
        switch status {
        case .inactive:      Color.colorSurfaceBgEmphasis.opacity(0.5)
        case .active:        status.color.opacity(0.18)
        case .idle:          Color.colorSurfaceBgEmphasis.opacity(0.7)
        case .notification:  status.color.opacity(0.28)
        case .error:         status.color.opacity(0.25)
        }
    }

    /// focused 時のみ glow (active/notification/error は強め、idle は subtle)
    private var glowColor: Color {
        guard isFocused else { return .clear }
        switch status {
        case .inactive, .idle: return .clear
        default:               return status.color.opacity(0.55)
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

/// Stand の address button。
///
/// UI 表示原則:
/// - **basic UI (default)**: full canonical form `{stand}.{lane}@{project}` (正確性優先)
/// - **Editor mode (opt-in)**: alias 展開で短縮 (`@project` 省略 or alias map)
///
/// Editor mode は `@AppStorage("vp.ui.editorMode")` で切替、将来 CreoUI Editor Mode protocol と連動。
///
/// 動作:
/// - text color で status、monospaced font で address 感
/// - click で full address (basic form) を clipboard copy
/// - hover で displayName + full address tooltip
struct StandDotButton: View {
    let stand: StandRef

    /// 強制 short form (Lane row 用、VP-83 refinement 14)
    /// Lane は Project accordion の中にある = 所属 project が tree で自明なので、
    /// `@project` は redundant。default false、Lane 側で明示的に true を渡す。
    var forceShort: Bool = false

    /// Editor mode 切替 (default = off = basic UI)
    /// opt-in: Drawer / preferences で on にすると alias 短縮表示
    @AppStorage("vp.ui.editorMode") private var editorMode: Bool = false

    /// 表示用 address
    /// - forceShort=true (Lane row): 常に `@project` 省略
    /// - editor mode off (basic UI): full canonical `hd.lead@vantage-point`
    /// - editor mode on (alias): `@project` 省略版 `hd.lead`
    ///
    /// ※ copy value (`stand.address`) は常に full canonical — msg_send で
    /// 使える実 actor address 形を保つ
    private var displayText: String {
        if (forceShort || editorMode), let atIdx = stand.address.firstIndex(of: "@") {
            return String(stand.address[..<atIdx])
        }
        return stand.address
    }

    var body: some View {
        Text(displayText)
            .font(.system(size: 10, design: .monospaced))
            .foregroundStyle(stand.status.color)
            .lineLimit(1)
            .truncationMode(.middle)
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
// MARK: - Lane Notification Row (L3、通知 only、refinement 41)

/// Lane row L3 — 通知 layer のみ (msgbox / unread / notification)
///
/// VP-83 refinement 41: Lane row 3 層構造の確立:
/// - L1: 可変 Lane 名 (将来 CC session title)
/// - L2: 固定情報 (branch + address)
/// - **L3: 通知 (本 component)**
///
/// 無通知時は空 view (spacing 0 で縮む)
struct LaneNotificationRow: View {
    let unreadCount: Int
    let hasNotification: Bool
    let wireStatus: String?
    /// 直近 msgbox 履歴 (VP-83: 受取・開封状況 UI、/api/diagnose 由来、最大 3 件)
    let recentMessages: [MsgboxHistoryEntry]

    init(unreadCount: Int, hasNotification: Bool, wireStatus: String?, recentMessages: [MsgboxHistoryEntry] = []) {
        self.unreadCount = unreadCount
        self.hasNotification = hasNotification
        self.wireStatus = wireStatus
        self.recentMessages = recentMessages
    }

    private var hasAnySignal: Bool {
        unreadCount > 0 || hasNotification || wireStatus != nil || !recentMessages.isEmpty
    }

    var body: some View {
        if hasAnySignal {
            HStack(spacing: 10) {
                // msgbox / ccwire 状態 — icon + label で意味明示 (refinement 45)
                if let status = wireStatus {
                    HStack(spacing: 3) {
                        Image(systemName: msgboxIcon(for: status))
                            .font(.system(size: 10))
                            .foregroundStyle(msgboxColor(for: status))
                        Text(msgboxLabel(for: status))
                            .font(.caption2)
                            .foregroundStyle(Color.colorTextTertiary)
                    }
                    .help("msgbox: \(status)")
                }

                // 未読 (envelope + count)
                if unreadCount > 0 {
                    HStack(spacing: 3) {
                        Image(systemName: "envelope.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(Color.colorSemanticWarning)
                        Text("\(unreadCount) 未読")
                            .font(.caption2)
                            .foregroundStyle(Color.colorSemanticWarning)
                    }
                } else if hasNotification {
                    // 通知 (bell)
                    HStack(spacing: 3) {
                        Image(systemName: "bell.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(Color.colorSemanticWarning)
                        Text("通知")
                            .font(.caption2)
                            .foregroundStyle(Color.colorSemanticWarning)
                    }
                }

                // 直近 msgbox 3 件 (VP-83 開封状況)
                // state 別 color: queued=orange / received=blue / acked=green
                if !recentMessages.isEmpty {
                    HStack(spacing: 2) {
                        ForEach(recentMessages.prefix(3)) { entry in
                            Circle()
                                .fill(recentStateColor(entry.state))
                                .frame(width: 6, height: 6)
                        }
                    }
                    .help(recentTooltip(recentMessages))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func recentStateColor(_ state: MsgboxEnvelopeState) -> Color {
        switch state {
        case .queued:   Color.colorSemanticWarning  // 未開封 (orange)
        case .received: Color.colorSemanticInfo     // 開封済 (blue)
        case .acked:    Color.colorSemanticSuccess  // ack 済 (green)
        }
    }

    private func recentTooltip(_ entries: [MsgboxHistoryEntry]) -> String {
        entries.prefix(3).map { e in
            let label: String = switch e.state {
            case .queued:   "⏳ queued"
            case .received: "📥 received"
            case .acked:    "✓ acked"
            }
            let payload = e.payloadPreview ?? ""
            let trimmed = payload.count > 32 ? String(payload.prefix(32)) + "…" : payload
            return "\(label) \(e.from)→\(e.to) \(trimmed)"
        }.joined(separator: "\n")
    }

    /// wireStatus 文字列を SF Symbol に
    private func msgboxIcon(for status: String) -> String {
        switch status {
        case "connected", "active": "antenna.radiowaves.left.and.right"
        case "idle":                "moon.zzz"
        case "stale", "disconnected", "error": "exclamationmark.triangle.fill"
        default:                    "circle"
        }
    }

    /// wireStatus の日本語ラベル
    private func msgboxLabel(for status: String) -> String {
        switch status {
        case "connected", "active": "接続"
        case "idle":                "待機"
        case "stale":               "失効"
        case "disconnected":        "切断"
        case "error":               "エラー"
        default:                    status
        }
    }

    /// wireStatus 文字列を色に
    private func msgboxColor(for status: String) -> Color {
        switch status {
        case "connected", "active": Color.colorSemanticInfo
        case "idle":                Color.colorTextTertiary
        case "stale", "disconnected", "error": Color.colorSemanticError
        default:                    Color.colorTextTertiary
        }
    }
}

struct UnifiedStatusBadge: View {
    /// Lane 自身を代表する lane-lead actor (= HD actor)
    let laneActor: StandRef
    var unreadCount: Int = 0
    var hasNotification: Bool = false
    /// msgbox / ccwire 状態 ("connected" / "idle" / "disconnected" / "stale" / nil)
    var wireStatus: String? = nil
    /// 強制 short form (Lane row 用、`@project` 省略)
    var forceShort: Bool = false

    var body: some View {
        HStack(spacing: 6) {
            StandDotButton(stand: laneActor, forceShort: forceShort)

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
    let msgboxSession: MsgboxSessionInfo?
    /// Claude CLI session title (最新 session の最初 user message、refinement 44)
    var ccSessionTitle: String? = nil
}

/// サイドバー表示用のプロジェクトモデル
extension SidebarProject {
    /// 表示用 title — displayName があれば優先、なければ slug を titleCased
    /// (CreoUI の `String.titleCased` util 経由、Rails titleize / Lodash startCase 相当)
    var displayTitle: String {
        if let d = displayName, !d.isEmpty { return d }
        return name.titleCased
    }

    /// Project level の agent status (Lead HD 基準、Tab Bar T1 で使用)
    /// Sidebar Lane row と同じ derivation ロジック
    var projectStatus: LaneStatus {
        if !hasHD { return .inactive }
        if hasNotification { return .notification }
        switch msgboxSession?.status {
        case "connected": return .active
        case "idle": return .idle
        case "stale", "disconnected": return .error
        default: return .active
        }
    }

    /// 未読 msgbox count (ccwire pendingMessages)
    var unreadCount: Int {
        Int(msgboxSession?.pendingMessages ?? 0)
    }
}

struct SidebarProject: Identifiable, Equatable {
    let id: String        // プロジェクトパス（一意キー）
    let name: String      // slug (kebab-case identifier, e.g. "vantage-point")
    /// 表示用の人間可読名 (user 設定 or config 由来)
    /// 未設定なら `name` (slug) を CamelCase + space 区切りに自動変換して表示
    /// VP-83 refinement 30: slug と displayName を first-class 分離
    let displayName: String?
    /// Claude CLI session title (最新 session の最初 user message、refinement 44)
    var ccSessionTitle: String? = nil
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
    let msgboxSession: MsgboxSessionInfo?
    /// SP 自動起動の有効/無効
    let enabled: Bool
    /// 直近の msgbox 履歴 (VP-83: 受取・開封状況 UI 用)
    /// /api/diagnose.msgbox.recent から取得 (最大 3 件)
    let recentMessages: [MsgboxHistoryEntry]

    init(id: String, name: String, displayName: String? = nil, path: String, isRunning: Bool, port: UInt16?, startedAt: Date?, stands: [SidebarStand] = [], workers: [CcwsWorkerInfo] = [], branch: String? = nil, hasHD: Bool = false, hasNotification: Bool = false, msgboxSession: MsgboxSessionInfo? = nil, enabled: Bool = true, recentMessages: [MsgboxHistoryEntry] = []) {
        self.id = id
        self.name = name
        self.displayName = displayName
        self.path = path
        self.isRunning = isRunning
        self.port = port
        self.startedAt = startedAt
        self.stands = stands
        self.workers = workers
        self.branch = branch
        self.hasHD = hasHD
        self.hasNotification = hasNotification
        self.msgboxSession = msgboxSession
        self.enabled = enabled
        self.recentMessages = recentMessages
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
                msgboxSession: nil
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
    /// システム全体 restart 中 (VP-83 refinement 34)
    case restarting(RestartPhase)
}

/// システム restart の phase — footer に逐次表示される
enum RestartPhase: Equatable {
    case stoppingProjects    // 稼働中の各 SP を明示的 kill
    case stoppingWorld       // World (daemon) stop
    case killingTmux         // tmux sessions kill (HD agent 含む)
    case waitingShutdown     // shutdown 待ち
    case startingWorld       // World (daemon) start
    case waitingHealth       // /api/health が通るまで poll
    case reconnectingSPs     // SP auto_start + project list reconnect
    case verifying           // 最終確認
    case complete            // 正常完了
    case failed(String)      // 失敗

    var displayText: String {
        switch self {
        case .stoppingProjects:   "プロジェクト停止中…"
        case .stoppingWorld:      "World 停止中…"
        case .killingTmux:        "ターミナル終了中…"
        case .waitingShutdown:    "停止待機中…"
        case .startingWorld:      "World 起動中…"
        case .waitingHealth:      "ヘルスチェック待機中…"
        case .reconnectingSPs:    "プロジェクト再接続中…"
        case .verifying:          "検証中…"
        case .complete:           "準備完了"
        case .failed(let msg):    "失敗: \(msg)"
        }
    }

    var isActive: Bool {
        switch self {
        case .complete, .failed: false
        default: true
        }
    }
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
                // Backend (daemon) のバージョン表示 (VP-83 refinement 35)
                // Mac app は常在、これは backend daemon のみの version
                Text("World v\(version)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
                Text(startedAt, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                // Backend 再接続ボタン (Mac app は生存、daemon + tmux のみ restart)
                Button {
                    onRestart?()
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.caption2)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .disabled(isRestarting)
                .help("World を再接続（ターミナル・デーモン・プロジェクト）")
            case .disconnected:
                Text("オフライン")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
            case .checking:
                Text("接続中…")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()

            case .restarting(let phase):
                Text(phase.displayText)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Spacer()
                if phase.isActive {
                    ProgressView()
                        .controlSize(.mini)
                        .scaleEffect(0.6)
                        .frame(width: 14, height: 14)
                }
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
        case .restarting(let phase):
            switch phase {
            case .complete: Color.colorSemanticSuccess
            case .failed: Color.colorSemanticError
            default: Color.colorSemanticWarning
            }
        }
    }

    /// Restart 中は button を disable (重複 trigger 防止)
    private var isRestarting: Bool {
        if case .restarting(let phase) = status, phase.isActive { return true }
        return false
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
