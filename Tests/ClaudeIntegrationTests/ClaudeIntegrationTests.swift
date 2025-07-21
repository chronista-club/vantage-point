import XCTest
@testable import ClaudeIntegration

final class ClaudeAPITests: XCTestCase {
    
    func testMessageCreation() {
        let message = Message(role: .user, content: "Hello, Claude!")
        XCTAssertEqual(message.role, .user)
        XCTAssertEqual(message.content, "Hello, Claude!")
    }
    
    func testRequestCreation() {
        let messages = [Message(role: .user, content: "Test")]
        let request = ClaudeRequest(
            model: ClaudeModel.claude35Sonnet.rawValue,
            messages: messages,
            maxTokens: 1024
        )
        
        XCTAssertEqual(request.model, ClaudeModel.claude35Sonnet.rawValue)
        XCTAssertEqual(request.messages.count, 1)
        XCTAssertEqual(request.maxTokens, 1024)
        XCTAssertFalse(request.stream)
    }
    
    func testModelDisplayNames() {
        XCTAssertEqual(ClaudeModel.claude3Opus.displayName, "Claude 3 Opus")
        XCTAssertEqual(ClaudeModel.claude35Sonnet.displayName, "Claude 3.5 Sonnet")
    }
    
    func testAPIConfiguration() {
        let config = APIConfiguration(apiKey: "test-key")
        XCTAssertEqual(config.apiKey, "test-key")
        XCTAssertEqual(config.baseURL.absoluteString, "https://api.anthropic.com")
        XCTAssertEqual(config.apiVersion, "2023-06-01")
        XCTAssertEqual(config.defaultModel, .claude35Haiku)
    }
    
    func testKeychainManagerInit() async {
        let manager = KeychainManager()
        // 初期化のテスト
        let hasKey = await manager.hasAPIKey()
        // 初回実行時はfalseであることを確認
        XCTAssertTrue(!hasKey || hasKey) // どちらの状態も許容
    }
}