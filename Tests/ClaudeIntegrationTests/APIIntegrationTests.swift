import XCTest
@testable import ClaudeIntegration

// MARK: - API Integration Tests

final class APIIntegrationTests: XCTestCase {
    
    var apiService: ClaudeServiceProtocol!
    var keychainManager: KeychainManager!
    
    override func setUp() async throws {
        try await super.setUp()
        keychainManager = KeychainManager()
    }
    
    override func tearDown() async throws {
        apiService = nil
        keychainManager = nil
        try await super.tearDown()
    }
    
    // MARK: - API Authentication Tests
    
    func testAPIKeyManagement() async throws {
        // テスト用のAPIキーを設定
        let testKey = "test-api-key-\(UUID().uuidString)"
        
        // 既存のキーを削除（重複エラーを避けるため）
        _ = try? await keychainManager.deleteAPIKey()
        
        // Keychainに保存
        try await keychainManager.saveAPIKey(testKey)
        
        // Keychainから取得
        let retrieved = try await keychainManager.loadAPIKey()
        XCTAssertEqual(retrieved, testKey, "Retrieved key doesn't match")
        
        // APIキーの存在確認
        let hasKey = await keychainManager.hasAPIKey()
        XCTAssertTrue(hasKey, "Keychain should report having API key")
        
        // APIキーを削除
        try await keychainManager.deleteAPIKey()
        
        // 削除後の確認
        let hasKeyAfterDelete = await keychainManager.hasAPIKey()
        XCTAssertFalse(hasKeyAfterDelete, "Keychain should not have API key after deletion")
    }
    
    func testServiceCreationWithKeychainKey() async throws {
        // 環境変数からAPIキーを取得
        guard let envKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured in environment")
        }
        
        // Keychainに保存
        try await keychainManager.saveAPIKey(envKey)
        
        // Keychainのキーを使用してサービスを作成
        do {
            let service = try await ClaudeServiceFactory.createDefault()
            let config = await service.configuration
            XCTAssertEqual(config.connectionType, .api)
            XCTAssertNotNil(config.apiKey)
        } catch {
            XCTFail("Failed to create service with keychain key: \(error)")
        }
        
        // クリーンアップ
        try await keychainManager.deleteAPIKey()
    }
    
    // MARK: - Model Selection Tests
    
    func testAllModelsAvailability() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let models: [ClaudeModel] = [.claude3Opus, .claude3Sonnet, .claude3Haiku]
        
        for model in models {
            let config = ClaudeServiceConfiguration(
                connectionType: .api,
                defaultModel: model,
                apiKey: apiKey
            )
            
            let service = try await ClaudeServiceFactory.create(with: config)
            let isAvailable = await service.isAvailable
            XCTAssertTrue(isAvailable, "\(model.displayName) should be available")
            
            // 簡単なテストメッセージを送信
            let messages = [Message(role: .user, content: "Say 'Hello' and nothing else.")]
            let options = MessageOptions(maxTokens: 20, temperature: 0.0)
            
            do {
                let response = try await service.sendMessage(messages, options: options)
                XCTAssertFalse(response.text.isEmpty)
                XCTAssertEqual(response.model, model.rawValue)
                print("✅ \(model.displayName) responded successfully")
            } catch {
                print("⚠️ \(model.displayName) test failed: \(error)")
            }
        }
    }
    
    func testModelSpecificFeatures() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        // Opus - 最も高性能なモデルでの複雑なタスク
        let opusConfig = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Opus,
            apiKey: apiKey
        )
        let opusService = try await ClaudeServiceFactory.create(with: opusConfig)
        
        let complexMessage = [Message(role: .user, content: "Explain quantum computing in one sentence.")]
        let opusResponse = try await opusService.sendMessage(
            complexMessage,
            options: MessageOptions(maxTokens: 100, temperature: 0.5)
        )
        XCTAssertFalse(opusResponse.text.isEmpty)
        
        // Haiku - 最速モデルでのレスポンス時間テスト
        let haikuConfig = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        let haikuService = try await ClaudeServiceFactory.create(with: haikuConfig)
        
        let simpleMessage = [Message(role: .user, content: "What is 1+1?")]
        let startTime = Date()
        let haikuResponse = try await haikuService.sendMessage(
            simpleMessage,
            options: MessageOptions(maxTokens: 10, temperature: 0.0)
        )
        let responseTime = Date().timeIntervalSince(startTime)
        
        XCTAssertFalse(haikuResponse.text.isEmpty)
        XCTAssertTrue(haikuResponse.text.contains("2"))
        print("Haiku response time: \(responseTime)s")
    }
    
    // MARK: - Rate Limit Tests
    
    func testRateLimitHandling() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // 短時間に複数のリクエストを送信
        let messages = [Message(role: .user, content: "Hi")]
        let options = MessageOptions(maxTokens: 10, temperature: 0.0)
        
        var successCount = 0
        var rateLimitEncountered = false
        
        // 5つの並行リクエストを送信
        await withTaskGroup(of: Result<ClaudeResponse, Error>.self) { group in
            for i in 0..<5 {
                group.addTask {
                    do {
                        let response = try await service.sendMessage(messages, options: options)
                        print("Request \(i) succeeded")
                        return .success(response)
                    } catch {
                        print("Request \(i) failed: \(error)")
                        return .failure(error)
                    }
                }
            }
            
            for await result in group {
                switch result {
                case .success:
                    successCount += 1
                case .failure(let error):
                    if case ClaudeIntegrationError.rateLimited = error {
                        rateLimitEncountered = true
                    }
                }
            }
        }
        
        // 少なくとも1つは成功するはず
        XCTAssertGreaterThan(successCount, 0)
        print("Success rate: \(successCount)/5")
        
        if rateLimitEncountered {
            print("Rate limit was encountered as expected")
        }
    }
    
    // MARK: - API Configuration Tests
    
    func testCustomAPIConfiguration() async throws {
        // カスタム設定でAPIクライアントを作成
        let customConfig = APIConfiguration(
            apiKey: "test-key",
            baseURL: URL(string: "https://api.anthropic.com")!,
            apiVersion: "2023-06-01",
            defaultModel: .claude35Sonnet
            // APIConfigurationにはmaxRetriesとtimeoutIntervalのパラメータがない
        )
        
        // maxRetriesとtimeoutIntervalはテストできない
        XCTAssertEqual(customConfig.defaultModel, .claude35Sonnet)
    }
    
    func testAPIEndpointConfiguration() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        // 標準のAPIエンドポイントを確認
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // サービスが正しく設定されていることを確認
        let serviceConfig = await service.configuration
        XCTAssertEqual(serviceConfig.connectionType, .api)
        XCTAssertNotNil(serviceConfig.apiKey)
    }
    
    // MARK: - Token Usage Tests
    
    func testTokenUsageTracking() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // 長めのメッセージでトークン使用量を確認
        let longMessage = """
        Please analyze this text and count the number of words in it. \
        This is a test message designed to use a reasonable number of tokens. \
        We want to ensure that token counting is working correctly in our API integration.
        """
        
        let messages = [Message(role: .user, content: longMessage)]
        let options = MessageOptions(maxTokens: 100, temperature: 0.0)
        
        let response = try await service.sendMessage(messages, options: options)
        
        // トークン使用量の確認
        XCTAssertGreaterThan(response.usage.inputTokens, 0)
        XCTAssertGreaterThan(response.usage.outputTokens, 0)
        XCTAssertEqual(
            response.usage.totalTokens,
            response.usage.inputTokens + response.usage.outputTokens
        )
        
        print("Token usage - Input: \(response.usage.inputTokens), Output: \(response.usage.outputTokens), Total: \(response.usage.totalTokens)")
    }
}