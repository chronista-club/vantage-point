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

    /// Project slot (nil = config で未割当、Phase L3 統合後は backend から取得)
    let slot: UInt16?
    /// Lane index (0 = Lead、1+ = Worker)
    let laneIndex: UInt16

    /// Port Management 計算済みの全 role port (Phase L3 完成、slot 未割当時は空)
    /// `LanePortLayout.ports(slot:laneIndex:)` を build 時に埋める
    let ports: [String: UInt16]
}

// MARK: - Port Layout mirror (Phase L3)

/// Swift 側 PortLayout mirror — Rust \`crates/vantage-point/src/port_layout.rs\` の
/// default 値を Mac app で port 計算するためのミラー。
///
/// 将来 (Phase L5 full) backend が `/api/port_layout` 的な endpoint を生やしたら
/// 取得層に切替、今は default 固定で OK。
enum LanePortLayout {
    static let projectSlotBase: UInt16 = 33000
    static let projectSlotSize: UInt16 = 100
    static let maxProjects: UInt16 = 20
    static let laneBaseOffset: UInt16 = 10
    static let laneSize: UInt16 = 10

    /// Role → offset within Lane
    static let roles: [String: UInt16] = [
        "agent": 0,
        "dev_server": 1,
        "db_admin": 2,
        "canvas": 3,
        "preview": 4,
    ]

    static func projectBase(slot: UInt16) -> UInt16? {
        guard slot < maxProjects else { return nil }
        return projectSlotBase + slot * projectSlotSize
    }

    static func laneBase(slot: UInt16, laneIndex: UInt16) -> UInt16? {
        guard let pb = projectBase(slot: slot) else { return nil }
        let lb = pb + laneBaseOffset + laneIndex * laneSize
        guard lb + laneSize <= pb + projectSlotSize else { return nil }
        return lb
    }

    static func port(slot: UInt16, laneIndex: UInt16, role: String) -> UInt16? {
        guard let base = laneBase(slot: slot, laneIndex: laneIndex),
              let offset = roles[role] else { return nil }
        return base + offset
    }

    /// 指定 Lane の全 role port を dictionary で返す
    static func ports(slot: UInt16, laneIndex: UInt16) -> [String: UInt16] {
        var result: [String: UInt16] = [:]
        for (role, _) in roles {
            if let p = port(slot: slot, laneIndex: laneIndex, role: role) {
                result[role] = p
            }
        }
        return result
    }

    /// URL 生成 (`http://localhost:{port}`)
    static func url(slot: UInt16, laneIndex: UInt16, role: String) -> String? {
        port(slot: slot, laneIndex: laneIndex, role: role).map { "http://localhost:\($0)" }
    }
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
    ///
    /// - projects の index を project_slot として採用 (config.projects[n].slot が未提供
    ///   なので view-time 側で n 番目 = slot n として扱う、暫定)
    /// - Lead = laneIndex 0、Worker = 1, 2, ... の appearance order
    static func build(from projects: [SidebarProject], notifications: Set<String> = []) -> LaneRegistry {
        var records: [LaneRecord] = []
        for (slotIdx, project) in projects.enumerated() {
            let slot = UInt16(slotIdx)
            records.append(leadRecord(
                for: project,
                slot: slot,
                hasNotification: notifications.contains(project.path)
            ))
            for (workerIdx, worker) in project.workers.enumerated() {
                records.append(workerRecord(
                    for: worker,
                    parent: project,
                    slot: slot,
                    laneIndex: UInt16(workerIdx + 1),
                    hasNotification: notifications.contains(worker.path)
                ))
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

    private static func leadRecord(for project: SidebarProject,
                                   slot: UInt16,
                                   hasNotification: Bool) -> LaneRecord {
        LaneRecord(
            address: "hd.lead@\(project.name)",
            path: project.path,
            projectName: project.name,
            kind: .lead,
            tmuxSession: tmuxSessionName(from: project.path),
            branch: project.branch,
            status: project.projectStatus,
            ccSessionTitle: project.ccSessionTitle,
            wireStatus: project.msgboxSession?.status,
            unreadCount: project.unreadCount,
            hasHD: project.hasHD,
            slot: slot,
            laneIndex: 0,
            ports: LanePortLayout.ports(slot: slot, laneIndex: 0)
        )
    }

    private static func workerRecord(for worker: CcwsWorkerInfo,
                                     parent: SidebarProject,
                                     slot: UInt16,
                                     laneIndex: UInt16,
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
            wireStatus: worker.msgboxSession?.status,
            unreadCount: Int(worker.msgboxSession?.pendingMessages ?? 0),
            hasHD: worker.hasHD,
            slot: slot,
            laneIndex: laneIndex,
            ports: LanePortLayout.ports(slot: slot, laneIndex: laneIndex)
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
        switch worker.msgboxSession?.status {
        case "connected": return .active
        case "idle": return .idle
        case "stale", "disconnected": return .error
        default: return .active
        }
    }
}
