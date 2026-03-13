import Foundation

// MARK: - API Types

/// プロジェクト情報
struct ProjectInfo: Codable, Identifiable {
    let name: String
    let path: String
    let processStatus: ProcessStatus

    var id: String {
        name
    }

    enum CodingKeys: String, CodingKey {
        case name, path
        case processStatus = "process_status"
    }
}

/// Process状態
enum TWProcessStatus: String, Codable {
    case stopped
    case starting
    case running
    case stopping
    case error
}

/// ProcessStatus のエイリアス（TheWorldClient 内の旧名との互換性）
typealias ProcessStatus = TWProcessStatus

/// 稼働中Process情報
struct RunningProcess: Codable, Identifiable {
    let projectName: String
    let port: UInt16
    let pid: UInt32
    let projectPath: String
    let discoveredViaBonjour: Bool

    var id: String {
        projectName
    }

    enum CodingKeys: String, CodingKey {
        case port, pid
        case projectName = "project_name"
        case projectPath = "project_path"
        case discoveredViaBonjour = "discovered_via_bonjour"
    }
}

/// プロジェクト一覧レスポンス
struct ProjectsResponse: Codable {
    let projects: [ProjectInfo]
}

/// Process一覧レスポンス
struct ProcessesResponse: Codable {
    let processes: [RunningProcess]
}

/// エラーレスポンス
struct TWErrorResponse: Codable {
    let error: String
}

// MARK: - Update API Types

/// リリースアセット情報
struct AssetInfo: Codable {
    let name: String
    let browserDownloadUrl: String
    let size: UInt64
    let contentType: String

    enum CodingKeys: String, CodingKey {
        case name, size
        case browserDownloadUrl = "browser_download_url"
        case contentType = "content_type"
    }
}

/// リリース情報
struct ReleaseInfo: Codable {
    let version: String
    let tagName: String
    let name: String?
    let body: String?
    let publishedAt: String?
    let htmlUrl: String
    let assets: [AssetInfo]

    enum CodingKeys: String, CodingKey {
        case version, name, body, assets
        case tagName = "tag_name"
        case publishedAt = "published_at"
        case htmlUrl = "html_url"
    }
}

/// 更新チェック結果
struct UpdateCheckResult: Codable {
    let currentVersion: String
    let latestVersion: String
    let updateAvailable: Bool
    let release: ReleaseInfo?

    enum CodingKeys: String, CodingKey {
        case release
        case currentVersion = "current_version"
        case latestVersion = "latest_version"
        case updateAvailable = "update_available"
    }
}

/// 更新適用結果
struct UpdateApplyResult: Codable {
    let success: Bool
    let previousVersion: String
    let newVersion: String
    let binaryPath: String
    let backupPath: String?
    let message: String
    let restartRequired: Bool

    enum CodingKeys: String, CodingKey {
        case success, message
        case previousVersion = "previous_version"
        case newVersion = "new_version"
        case binaryPath = "binary_path"
        case backupPath = "backup_path"
        case restartRequired = "restart_required"
    }
}

// MARK: - Mac App Update API Types

/// Macアプリ更新チェック結果
struct MacAppUpdateCheckResult: Codable {
    let currentVersion: String
    let latestVersion: String
    let updateAvailable: Bool
    let release: ReleaseInfo?
    let appPath: String?

    enum CodingKeys: String, CodingKey {
        case release
        case currentVersion = "current_version"
        case latestVersion = "latest_version"
        case updateAvailable = "update_available"
        case appPath = "app_path"
    }
}

/// Macアプリ更新適用結果
struct MacAppUpdateApplyResult: Codable {
    let success: Bool
    let previousVersion: String
    let newVersion: String
    let appPath: String
    let backupPath: String?
    let message: String
    let restartRequired: Bool

    enum CodingKeys: String, CodingKey {
        case success, message
        case previousVersion = "previous_version"
        case newVersion = "new_version"
        case appPath = "app_path"
        case backupPath = "backup_path"
        case restartRequired = "restart_required"
    }
}

/// 再起動結果
struct RestartResult: Codable {
    let success: Bool
    let message: String
    let delay: UInt32
}

// MARK: - Errors

enum TheWorldError: LocalizedError {
    case invalidResponse
    case httpError(Int)
    case serverError(String)
    case notAvailable

    var errorDescription: String? {
        switch self {
        case .invalidResponse:
            "Invalid response from TheWorld"
        case let .httpError(code):
            "HTTP error: \(code)"
        case let .serverError(message):
            message
        case .notAvailable:
            "TheWorld is not available"
        }
    }
}
