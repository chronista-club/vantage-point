import XCTest
@testable import ClaudeIntegration

// MARK: - Basic Model Tests

final class ClaudeIntegrationTests: XCTestCase {
    
    // MARK: - Message Tests
    
    func testMessageCreation() {
        let message = Message(role: .user, content: "Hello, Claude!")
        XCTAssertEqual(message.role, .user)
        XCTAssertEqual(message.content, "Hello, Claude!")
    }
    
    func testMessageWithSystemRole() {
        let message = Message(role: .system, content: "You are a helpful assistant.")
        XCTAssertEqual(message.role, .system)
        XCTAssertEqual(message.content, "You are a helpful assistant.")
    }
    
    func testMessageWithAssistantRole() {
        let message = Message(role: .assistant, content: "I'm here to help!")
        XCTAssertEqual(message.role, .assistant)
        XCTAssertEqual(message.content, "I'm here to help!")
    }
    
    // MARK: - Request Model Tests
    
    func testBasicRequestCreation() {
        let messages = [Message(role: .user, content: "Test")]
        let request = ClaudeRequest(
            model: ClaudeModel.claude3Haiku.rawValue,
            messages: messages,
            maxTokens: 1024
        )
        
        XCTAssertEqual(request.model, ClaudeModel.claude3Haiku.rawValue)
        XCTAssertEqual(request.messages.count, 1)
        XCTAssertEqual(request.maxTokens, 1024)
        XCTAssertFalse(request.stream)
    }
    
    func testStreamingRequestCreation() {
        let messages = [Message(role: .user, content: "Stream test")]
        let request = ClaudeRequest(
            model: ClaudeModel.claude3Sonnet.rawValue,
            messages: messages,
            maxTokens: 2048,
            stream: true
        )
        
        XCTAssertTrue(request.stream)
        XCTAssertEqual(request.maxTokens, 2048)
    }
    
    // MARK: - Model Tests
    
    func testModelDisplayNames() {
        XCTAssertEqual(ClaudeModel.claude3Opus.displayName, "Claude 3 Opus")
        XCTAssertEqual(ClaudeModel.claude35Sonnet.displayName, "Claude 3.5 Sonnet")
        XCTAssertEqual(ClaudeModel.claude35Haiku.displayName, "Claude 3.5 Haiku")
    }
    
    func testModelRawValues() {
        XCTAssertEqual(ClaudeModel.claude3Opus.rawValue, "claude-3-opus-20240229")
        XCTAssertEqual(ClaudeModel.claude35Sonnet.rawValue, "claude-3-5-sonnet-20241022")
        XCTAssertEqual(ClaudeModel.claude35Haiku.rawValue, "claude-3-5-haiku-20241022")
    }
    
    // MARK: - Configuration Tests
    
    func testServiceConfigurationWithAPI() {
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: "test-api-key"
        )
        
        XCTAssertEqual(config.connectionType, .api)
        XCTAssertEqual(config.defaultModel, .claude3Haiku)
        XCTAssertEqual(config.apiKey, "test-api-key")
    }
    
    func testServiceConfigurationWithClaudeCode() {
        let config = ClaudeServiceConfiguration(
            connectionType: .claudeCode,
            defaultModel: .claude3Sonnet
        )
        
        XCTAssertEqual(config.connectionType, .claudeCode)
        XCTAssertEqual(config.defaultModel, .claude3Sonnet)
        XCTAssertNil(config.apiKey)
    }
    
    // MARK: - Message Options Tests
    
    func testMessageOptionsDefaults() {
        let options = MessageOptions()
        
        // MessageOptionsのプロパティを確認
        XCTAssertNil(options.model)
        XCTAssertNil(options.system)
        XCTAssertNil(options.maxTokens)
        XCTAssertNil(options.temperature)
    }
    
    func testMessageOptionsCustomValues() {
        let options = MessageOptions(
            model: .claude35Haiku,
            system: "You are a code assistant.",
            maxTokens: 8192,
            temperature: 0.5
        )
        
        XCTAssertEqual(options.maxTokens, 8192)
        XCTAssertEqual(options.temperature, 0.5)
        XCTAssertEqual(options.model, .claude35Haiku)
        XCTAssertEqual(options.system, "You are a code assistant.")
    }
}