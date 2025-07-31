import XCTest
@testable import ClaudeIntegration

final class ClaudeServiceTests: XCTestCase {
    
    override func setUp() {
        super.setUp()
    }
    
    override func tearDown() {
        super.tearDown()
    }
    
    // MARK: - Factory Tests
    
    func testCreateDefaultService() async throws {
        // デフォルトサービスの作成
        do {
            let service = try await ClaudeServiceFactory.createDefault()
            
            // プラットフォームに応じた設定を確認
            let config = await service.configuration
            
            #if os(macOS)
            XCTAssertEqual(config.connectionType, .claudeCode)
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
            defaultModel: .claude3_haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"]
        )
        
        do {
            let service = try await ClaudeServiceFactory.create(with: config)
            let serviceConfig = await service.configuration
            
            XCTAssertEqual(serviceConfig.connectionType, .api)
            XCTAssertEqual(serviceConfig.defaultModel, .claude3_haiku)
            
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
            defaultModel: .claude3_haiku,
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
            defaultModel: .claude3_haiku,
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
            defaultModel: .claude3_sonnet
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
    
    // MARK: - Integration Tests
    
    func testSimpleMessageSending() async throws {
        // 環境変数からAPIキーを取得
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            print("⚠️ Skipping integration test: CLAUDE_API_KEY not set")
            return
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3_haiku,
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
}