import Foundation

/// CLI設定を管理する構造体
public struct VantageConfig: Codable, Sendable {
    public var defaultProjectPath: String
    public var claudeAPIKey: String?
    public var colorOutput: Bool
    public var verboseLogging: Bool
    public var autoSync: Bool
    public var devicePreferences: DevicePreferences
    
    public init(
        defaultProjectPath: String? = nil,
        claudeAPIKey: String? = nil,
        colorOutput: Bool = true,
        verboseLogging: Bool = false,
        autoSync: Bool = false,
        devicePreferences: DevicePreferences? = nil
    ) {
        self.defaultProjectPath = defaultProjectPath ?? FileManager.default
            .homeDirectoryForCurrentUser
            .appendingPathComponent("Documents/Vantage")
            .path
        self.claudeAPIKey = claudeAPIKey
        self.colorOutput = colorOutput
        self.verboseLogging = verboseLogging
        self.autoSync = autoSync
        self.devicePreferences = devicePreferences ?? DevicePreferences()
    }
    
    /// デフォルト設定
    public static let `default` = VantageConfig()
    
    /// 設定ファイルのパス
    public static var configPath: URL {
        FileManager.default
            .homeDirectoryForCurrentUser
            .appendingPathComponent(".vantage/config.json")
    }
}

/// デバイス関連の設定
public struct DevicePreferences: Codable, Sendable {
    public var preferredDevice: String?
    public var autoConnect: Bool
    public var connectionTimeout: TimeInterval
    
    public init(
        preferredDevice: String? = nil,
        autoConnect: Bool = false,
        connectionTimeout: TimeInterval = 30.0
    ) {
        self.preferredDevice = preferredDevice
        self.autoConnect = autoConnect
        self.connectionTimeout = connectionTimeout
    }
}