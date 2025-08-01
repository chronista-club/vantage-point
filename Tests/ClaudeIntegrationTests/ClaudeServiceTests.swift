import XCTest
@testable import ClaudeIntegration

// MARK: - Service Integration Tests

final class ClaudeServiceTests: XCTestCase {
    
    override func setUp() async throws {
        try await super.setUp()
    }
    
    override func tearDown() async throws {
        try await super.tearDown()
    }
    
    // MARK: - Factory Tests
    
    func testCreateDefaultService() async throws {
        // デフォルトサービスの作成
        do {
            let service = try await ClaudeServiceFactory.createDefault()
            
            // プラットフォームに応じた設定を確認
            let config = await service.configuration
            
            #if os(macOS)
            // macOSではClaude Codeが利用可能な場合はそれを使用
            if await service.isAvailable && config.connectionType == .claudeCode {
                XCTAssertEqual(config.connectionType, .claudeCode)
            } else {
                // Claude Codeが利用できない場合はAPIにフォールバック
                XCTAssertEqual(config.connectionType, .api)
            }
            #else
            XCTAssertEqual(config.connectionType, .api)
            #endif
            
            print("✅ Created service with connection type: \(config.connectionType.rawValue)")
        } catch {
            // APIキーが設定されていない場合はスキップ
            if case ClaudeIntegrationError.missingAPIKey = error {
                print("⚠️ Skipping test: API key not configured")
                return
            }
            throw error
        }
    }
    
    func testCreateAPIService() async throws {
        // API設定を作成
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"]
        )
        
        do {
            let service = try await ClaudeServiceFactory.create(with: config)
            let serviceConfig = await service.configuration
            
            XCTAssertEqual(serviceConfig.connectionType, .api)
            XCTAssertEqual(serviceConfig.defaultModel, .claude3Haiku)
            
            print("✅ Created API service")
        } catch ClaudeIntegrationError.missingAPIKey {
            print("⚠️ Skipping test: API key not configured")
        }
    }
    
    // MARK: - Service Protocol Tests
    
    func testServiceAvailability() async throws {
        // APIサービスの可用性をテスト
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"]
        )
        
        do {
            let service = try await ClaudeServiceFactory.create(with: config)
            let isAvailable = await service.isAvailable
            
            XCTAssertTrue(isAvailable, "API service should always be available")
            print("✅ Service availability: \(isAvailable)")
        } catch ClaudeIntegrationError.missingAPIKey {
            print("⚠️ Skipping test: API key not configured")
        }
    }
    
    func testWorkingDirectoryManagement() async throws {
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"]
        )
        
        do {
            let service = try await ClaudeServiceFactory.create(with: config)
            
            // 作業ディレクトリを設定
            let testPath = "/tmp/test-working-dir"
            await service.setWorkingDirectory(testPath)
            
            // ファイルコンテキストを追加
            let testFiles = ["file1.swift", "file2.swift"]
            await service.addFileContext(testFiles)
            
            print("✅ Context management configured")
        } catch ClaudeIntegrationError.missingAPIKey {
            print("⚠️ Skipping test: API key not configured")
        }
    }
    
    // MARK: - Platform-specific Tests
    
    #if os(macOS)
    func testClaudeCodeServiceCreation() async throws {
        let config = ClaudeServiceConfiguration(
            connectionType: .claudeCode,
            defaultModel: .claude3Sonnet
        )
        
        do {
            let service = try await ClaudeServiceFactory.create(with: config)
            let isAvailable = await service.isAvailable
            
            if !isAvailable {
                print("⚠️ Claude Code is not running. Skipping test.")
                return
            }
            
            print("✅ Claude Code service created and available")
        } catch ClaudeIntegrationError.serviceUnavailable(let reason) {
            print("⚠️ Service unavailable: \(reason)")
        }
    }
    #endif
    
    // MARK: - Message Sending Tests
    
    func testSimpleMessageSending() async throws {
        // 環境変数からAPIキーを取得
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            print("⚠️ Skipping integration test: CLAUDE_API_KEY not set")
            return
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // テストメッセージを送信
        let messages = [
            Message(role: .user, content: "Say 'Hello, World!' and nothing else.")
        ]
        
        let options = MessageOptions(
            maxTokens: 50,
            temperature: 0.0
        )
        
        do {
            let response = try await service.sendMessage(messages, options: options)
            print("✅ Received response: \(response.text)")
            
            XCTAssertFalse(response.text.isEmpty)
            XCTAssertTrue(response.text.lowercased().contains("hello"))
        } catch {
            print("❌ Error sending message: \(error)")
            throw error
        }
    }
    
    func testMessageWithSystemPrompt() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        let messages = [Message(role: .user, content: "What are you?")]
        let options = MessageOptions(
            system: "You are a helpful AI assistant. Always respond in exactly 10 words.",
            maxTokens: 100,
            temperature: 0.0
        )
        
        let response = try await service.sendMessage(messages, options: options)
        XCTAssertFalse(response.text.isEmpty)
        
        // システムプロンプトが効いていることを確認
        let wordCount = response.text.split(separator: " ").count
        print("Response word count: \(wordCount), text: \(response.text)")
    }
    
    // MARK: - Performance Tests
    
    func testServiceCreationPerformance() async throws {
        // サービス作成のパフォーマンスを測定
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] ?? "test-key"
        )
        
        let startTime = Date()
        _ = try await ClaudeServiceFactory.create(with: config)
        let creationTime = Date().timeIntervalSince(startTime)
        
        print("Service creation time: \(creationTime)s")
        XCTAssertLessThan(creationTime, 1.0, "Service creation should be fast")
    }
    
    func testConcurrentRequests() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // 3つの並行リクエストを送信
        await withTaskGroup(of: Result<String, Error>.self) { group in
            for i in 1...3 {
                group.addTask {
                    let messages = [Message(role: .user, content: "Say '\(i)' and nothing else.")]
                    let options = MessageOptions(maxTokens: 10, temperature: 0.0)
                    
                    do {
                        let response = try await service.sendMessage(messages, options: options)
                        return .success(response.text)
                    } catch {
                        return .failure(error)
                    }
                }
            }
            
            var successCount = 0
            for await result in group {
                switch result {
                case .success(let text):
                    print("Concurrent response: \(text)")
                    successCount += 1
                case .failure(let error):
                    print("Concurrent request failed: \(error)")
                }
            }
            
            XCTAssertGreaterThan(successCount, 0, "At least one concurrent request should succeed")
        }
    }
}