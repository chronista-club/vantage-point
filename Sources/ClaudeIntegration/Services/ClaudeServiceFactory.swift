import Foundation

/// Claude連携サービスのファクトリー
/// プラットフォームに応じて適切な実装を提供する
public enum ClaudeServiceFactory {
    
    /// プラットフォームに応じたデフォルトのサービスを作成
    public static func createDefault() async throws -> any ClaudeServiceProtocol {
        let configuration = ClaudeServiceConfiguration.platformDefault
        return try await create(with: configuration)
    }
    
    /// 指定された設定でサービスを作成
    /// - Parameter configuration: サービス設定
    /// - Returns: Claude連携サービス
    public static func create(
        with configuration: ClaudeServiceConfiguration
    ) async throws -> any ClaudeServiceProtocol {
        switch configuration.connectionType {
        case .api:
            // API直接呼び出しの実装
            return try await createAPIService(configuration: configuration)
            
        case .claudeCode:
            // Claude Code連携の実装
            #if os(macOS)
            return try await createClaudeCodeService(configuration: configuration)
            #else
            // visionOSではClaude Codeは利用不可なので、APIにフォールバック
            print("⚠️ Claude Code is not available on visionOS. Falling back to API.")
            var apiConfig = configuration
            apiConfig = ClaudeServiceConfiguration(
                connectionType: .api,
                defaultModel: configuration.defaultModel,
                apiKey: configuration.apiKey,
                timeoutInterval: configuration.timeoutInterval
            )
            return try await createAPIService(configuration: apiConfig)
            #endif
        }
    }
    
    // MARK: - Private Factory Methods
    
    /// API連携サービスを作成
    private static func createAPIService(
        configuration: ClaudeServiceConfiguration
    ) async throws -> any ClaudeServiceProtocol {
        // APIキーの取得（設定またはKeychainから）
        let apiKey: String
        if let configKey = configuration.apiKey, !configKey.isEmpty {
            apiKey = configKey
        } else {
            // Keychainから取得
            let keychain = KeychainManager()
            do {
                apiKey = try await keychain.loadAPIKey()
            } catch {
                throw ClaudeIntegrationError.missingAPIKey
            }
        }
        
        // API設定を作成
        let apiConfiguration = APIConfiguration(
            apiKey: apiKey,
            timeoutInterval: configuration.timeoutInterval,
            defaultModel: configuration.defaultModel
        )
        
        // APIサービスを返す
        return ClaudeAPIService(apiConfiguration: apiConfiguration)
    }
    
    #if os(macOS)
    /// Claude Code連携サービスを作成
    private static func createClaudeCodeService(
        configuration: ClaudeServiceConfiguration
    ) async throws -> any ClaudeServiceProtocol {
        let service = ClaudeCodeService(configuration: configuration)
        
        // Claude Codeが利用可能か確認
        let isAvailable = await service.isAvailable
        if !isAvailable {
            print("⚠️ Claude Code is not available. Please ensure Claude Code is installed and running.")
            // フォールバックするかエラーを投げるか選択可能
            throw ClaudeIntegrationError.serviceUnavailable("Claude Code is not running")
        }
        
        return service
    }
    #endif
}

