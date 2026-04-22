import Foundation
import SwiftUI

/// VP Lane の identity 統合 record (Phase L1 MVP — 2026-04-23)
///
/// Sidebar selection / Viewport attach / Mailbox target / send-keys target が
/// 1 identity で結ばれる single source of truth の UI side projection。
///
/// Rust backend に本格 LaneRegistry (stateful actor) が成立するまでは
/// Mac app 内で SidebarProject + CcwsWorkerInfo から derive する
/// **view-time computation** として運用。
///
/// 関連設計: `mem_1CaKLm2YnpHvcqpGUZx8Ux` (VP Lane Registry backbone)
struct LaneRecord: Identifiable, Equatable {
    enum Kind { case lead, worker }

    /// canonical address: `hd.lead@{project}` or `hd.{suffix}@{project}`
    let address: String
    /// Lane の作業 directory (project root or worker dir)
    let path: String
    /// 親 project の slug (address の `@` の後ろ)
    let projectName: String
    let kind: Kind
    /// tmux session 名: `{basename}-vp` (dot は hyphen 化)
    let tmuxSession: String
    let branch: String?
    let status: LaneStatus
    let ccSessionTitle: String?
    let wireStatus: String?
    let unreadCount: Int
    let hasHD: Bool

    var id: String { path }

    /// 将来 Port Management 統合用 placeholder
    /// L3 で LaneRecord が LaneRegistry から slot + lane_index を得て port 計算
    var ports: [String: UInt16] { [:] }  // TODO: Phase L3 で LanePortLayout 連携
}

/// In-memory Lane Registry (view-time derived)
///
/// `SidebarProject` + `CcwsWorkerInfo` の array から build される read-only
/// snapshot。address / path / tmuxSession のいずれでも lookup 可。
///
/// user feedback (2026-04-23): 「ccwire 外したい」「シンプルに」「開発に生かしたい」
/// → 最小 breaking で既存 path based code と並走、徐々に Registry 経由に寄せる。
struct LaneRegistry {
    let records: [LaneRecord]
    private let byAddress: [String: LaneRecord]
    private let byPath: [String: LaneRecord]
    private let byTmuxSession: [String: LaneRecord]

    init(records: [LaneRecord]) {
        self.records = records
        self.byAddress = Dictionary(records.map { ($0.address, $0) }, uniquingKeysWith: { a, _ in a })
        self.byPath = Dictionary(records.map { ($0.path, $0) }, uniquingKeysWith: { a, _ in a })
        self.byTmuxSession = Dictionary(records.map { ($0.tmuxSession, $0) }, uniquingKeysWith: { a, _ in a })
    }

    /// projects array から registry を build (view-time)
    static func build(from projects: [SidebarProject], notifications: Set<String> = []) -> LaneRegistry {
        var records: [LaneRecord] = []
        for project in projects {
            records.append(leadRecord(for: project, hasNotification: notifications.contains(project.path)))
            for worker in project.workers {
                records.append(workerRecord(for: worker, parent: project,
                                            hasNotification: notifications.contains(worker.path)))
            }
        }
        return LaneRegistry(records: records)
    }

    // MARK: - Lookup

    func findByAddress(_ address: String) -> LaneRecord? { byAddress[address] }
    func findByPath(_ path: String) -> LaneRecord? { byPath[path] }
    func findByTmuxSession(_ session: String) -> LaneRecord? { byTmuxSession[session] }

    /// 指定 project に属する全 Lane (Lead + Workers)
    func lanes(of projectName: String) -> [LaneRecord] {
        records.filter { $0.projectName == projectName }
    }

    // MARK: - Record 生成 (内部)

    private static func leadRecord(for project: SidebarProject, hasNotification: Bool) -> LaneRecord {
        LaneRecord(
            address: "hd.lead@\(project.name)",
            path: project.path,
            projectName: project.name,
            kind: .lead,
            tmuxSession: tmuxSessionName(from: project.path),
            branch: project.branch,
            status: project.projectStatus,
            ccSessionTitle: project.ccSessionTitle,
            wireStatus: project.ccwireSession?.status,
            unreadCount: project.unreadCount,
            hasHD: project.hasHD
        )
    }

    private static func workerRecord(for worker: CcwsWorkerInfo,
                                     parent: SidebarProject,
                                     hasNotification: Bool) -> LaneRecord {
        let status = deriveWorkerStatus(worker: worker, hasNotification: hasNotification)
        return LaneRecord(
            address: "hd.\(worker.suffix)@\(parent.name)",
            path: worker.path,
            projectName: parent.name,
            kind: .worker,
            tmuxSession: tmuxSessionName(from: worker.path),
            branch: worker.branch,
            status: status,
            ccSessionTitle: worker.ccSessionTitle,
            wireStatus: worker.ccwireSession?.status,
            unreadCount: Int(worker.ccwireSession?.pendingMessages ?? 0),
            hasHD: worker.hasHD
        )
    }

    /// path → tmux session 名 (既存 3 箇所の derivation を集約)
    /// - MainWindowView.tmuxSessionName
    /// - TerminalRepresentable makeNSView
    /// - CcwsDiscovery.discoverWorkers
    static func tmuxSessionName(from path: String) -> String {
        let dirName = (path as NSString).lastPathComponent
        return dirName.replacingOccurrences(of: ".", with: "-") + "-vp"
    }

    private static func deriveWorkerStatus(worker: CcwsWorkerInfo, hasNotification: Bool) -> LaneStatus {
        if !worker.hasHD { return .inactive }
        if hasNotification { return .notification }
        switch worker.ccwireSession?.status {
        case "connected": return .active
        case "idle": return .idle
        case "stale", "disconnected": return .error
        default: return .active
        }
    }
}
