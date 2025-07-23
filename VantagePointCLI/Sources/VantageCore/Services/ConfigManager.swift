import Foundation
import Logging

/// 設定ファイルの管理を行うマネージャー
public final class ConfigManager: @unchecked Sendable {
    private static let logger = Logger(label: "com.vantage.cli.config")
    
    /// シングルトンインスタンス
    public static let shared = ConfigManager()
    
    private let queue = DispatchQueue(label: "com.vantage.cli.config", attributes: .concurrent)
    private var _config: VantageConfig
    private let fileManager = FileManager.default
    
    private init() {
        self._config = Self.loadConfig() ?? VantageConfig.default
    }
    
    /// 現在の設定を取得
    public func getConfig() -> VantageConfig {
        queue.sync {
            return _config
        }
    }
    
    /// 設定値を文字列として取得
    public func getStringValue(for key: String) -> String? {
        queue.sync {
            let mirror = Mirror(reflecting: _config)
            for child in mirror.children {
                if child.label == key {
                    return String(describing: child.value)
                }
            }
            return nil
        }
    }
    
    /// 設定値を更新
    public func setValue(_ value: String, for key: String) throws {
        try queue.sync(flags: .barrier) {
            switch key {
            case "defaultProjectPath":
                _config.defaultProjectPath = value
            case "claudeAPIKey":
                _config.claudeAPIKey = value
            case "colorOutput":
                guard let boolValue = Bool(value) else {
                    throw ConfigError.invalidValue(key, value)
                }
                _config.colorOutput = boolValue
            case "verboseLogging":
                guard let boolValue = Bool(value) else {
                    throw ConfigError.invalidValue(key, value)
                }
                _config.verboseLogging = boolValue
            case "autoSync":
                guard let boolValue = Bool(value) else {
                    throw ConfigError.invalidValue(key, value)
                }
                _config.autoSync = boolValue
            default:
                throw ConfigError.invalidKey(key)
            }
            
            try saveConfigInternal()
        }
    }
    
    /// 設定をファイルに保存
    private func saveConfigInternal() throws {
        let configDir = VantageConfig.configPath.deletingLastPathComponent()
        
        // ディレクトリが存在しない場合は作成
        if !fileManager.fileExists(atPath: configDir.path) {
            try fileManager.createDirectory(
                at: configDir,
                withIntermediateDirectories: true,
                attributes: nil
            )
        }
        
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(_config)
        
        try data.write(to: VantageConfig.configPath)
        Self.logger.info("Configuration saved to \(VantageConfig.configPath.path)")
    }
    
    /// 設定をデフォルトにリセット
    public func reset() throws {
        try queue.sync(flags: .barrier) {
            _config = VantageConfig.default
            try saveConfigInternal()
        }
    }
    
    /// 設定ファイルから読み込み
    private static func loadConfig() -> VantageConfig? {
        guard FileManager.default.fileExists(atPath: VantageConfig.configPath.path) else {
            logger.info("No configuration file found at \(VantageConfig.configPath.path)")
            return nil
        }
        
        do {
            let data = try Data(contentsOf: VantageConfig.configPath)
            let decoder = JSONDecoder()
            let config = try decoder.decode(VantageConfig.self, from: data)
            logger.info("Configuration loaded from \(VantageConfig.configPath.path)")
            return config
        } catch {
            logger.error("Failed to load configuration: \(error)")
            return nil
        }
    }
}

/// 設定関連のエラー
public enum ConfigError: LocalizedError {
    case invalidKey(String)
    case invalidValue(String, String)
    case saveFailed(Error)
    
    public var errorDescription: String? {
        switch self {
        case .invalidKey(let key):
            return "Invalid configuration key: '\(key)'"
        case .invalidValue(let key, let value):
            return "Invalid value '\(value)' for key '\(key)'"
        case .saveFailed(let error):
            return "Failed to save configuration: \(error.localizedDescription)"
        }
    }
}