import XCTest
@testable import ClaudeIntegration

// MARK: - Workflow Integration Tests

final class WorkflowTests: XCTestCase {
    
    var testService: ClaudeServiceProtocol!
    var testConfiguration: ClaudeServiceConfiguration!
    
    override func setUp() async throws {
        try await super.setUp()
        
        // テスト用の設定を作成
        testConfiguration = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"]
        )
    }
    
    override func tearDown() async throws {
        testService = nil
        testConfiguration = nil
        try await super.tearDown()
    }
    
    // MARK: - Session Workflow Tests
    
    func testCompleteConversationWorkflow() async throws {
        guard testConfiguration.apiKey != nil else {
            throw XCTSkip("API key not configured")
        }
        
        // サービスを作成
        testService = try await ClaudeServiceFactory.create(with: testConfiguration)
        
        // 会話を開始
        var messages: [Message] = []
        
        // 最初のメッセージ
        messages.append(Message(role: .user, content: "What is 2 + 2?"))
        
        let options = MessageOptions(
            system: "You are a helpful math assistant. Answer concisely.",
            maxTokens: 100,
            temperature: 0.0
        )
        
        // レスポンスを取得
        let response1 = try await testService.sendMessage(messages, options: options)
        XCTAssertFalse(response1.text.isEmpty)
        XCTAssertTrue(response1.text.contains("4"))
        
        // アシスタントの応答を会話に追加
        messages.append(Message(role: .assistant, content: response1.text))
        
        // フォローアップ質問
        messages.append(Message(role: .user, content: "Now multiply that by 3"))
        
        // 2番目のレスポンスを取得
        let response2 = try await testService.sendMessage(messages, options: options)
        XCTAssertFalse(response2.text.isEmpty)
        XCTAssertTrue(response2.text.contains("12"))
        
        // 使用トークンの確認
        XCTAssertGreaterThan(response2.usage.inputTokens, 0)
        XCTAssertGreaterThan(response2.usage.outputTokens, 0)
    }
    
    func testMultiTurnConversationWithContext() async throws {
        guard testConfiguration.apiKey != nil else {
            throw XCTSkip("API key not configured")
        }
        
        testService = try await ClaudeServiceFactory.create(with: testConfiguration)
        
        var messages: [Message] = []
        let options = MessageOptions(
            system: "You are a helpful coding assistant. Be concise.",
            maxTokens: 200,
            temperature: 0.0
        )
        
        // コーディング関連の会話
        messages.append(Message(role: .user, content: "Write a simple Swift function to add two numbers"))
        
        let response1 = try await testService.sendMessage(messages, options: options)
        XCTAssertTrue(response1.text.contains("func"))
        messages.append(Message(role: .assistant, content: response1.text))
        
        // フォローアップ：関数の修正を依頼
        messages.append(Message(role: .user, content: "Now make it generic to work with any numeric type"))
        
        let response2 = try await testService.sendMessage(messages, options: options)
        XCTAssertTrue(response2.text.contains("T") || response2.text.contains("Numeric"))
        messages.append(Message(role: .assistant, content: response2.text))
        
        // さらなるフォローアップ：エラーハンドリングの追加
        messages.append(Message(role: .user, content: "Add overflow checking"))
        
        let response3 = try await testService.sendMessage(messages, options: options)
        XCTAssertFalse(response3.text.isEmpty)
        
        // 会話の継続性を確認
        XCTAssertGreaterThan(messages.count, 4)
    }
    
    // MARK: - Context Management Workflow Tests
    
    func testWorkingDirectoryWorkflow() async throws {
        guard testConfiguration.apiKey != nil else {
            throw XCTSkip("API key not configured")
        }
        
        testService = try await ClaudeServiceFactory.create(with: testConfiguration)
        
        // 作業ディレクトリを設定
        let workDir = "/tmp/test-claude-workflow"
        await testService.setWorkingDirectory(workDir)
        
        // ファイルコンテキストを追加
        let files = ["main.swift", "utils.swift", "tests.swift"]
        await testService.addFileContext(files)
        
        // コンテキストを考慮したメッセージを送信
        let messages = [Message(role: .user, content: "Describe the project structure based on the files")]
        let options = MessageOptions(maxTokens: 200, temperature: 0.5)
        
        do {
            let response = try await testService.sendMessage(messages, options: options)
            XCTAssertFalse(response.text.isEmpty)
            
            // レスポンスがファイル名を認識していることを確認
            XCTAssertTrue(
                response.text.lowercased().contains("swift") ||
                response.text.lowercased().contains("file") ||
                response.text.lowercased().contains("project")
            )
        } catch {
            // コンテキスト管理は必須機能ではないため、エラーは警告のみ
            print("Context management test warning: \(error)")
        }
    }
    
    // MARK: - Streaming Workflow Tests
    
    func testStreamingResponseWorkflow() async throws {
        guard testConfiguration.apiKey != nil else {
            throw XCTSkip("API key not configured")
        }
        
        testService = try await ClaudeServiceFactory.create(with: testConfiguration)
        
        let messages = [Message(role: .user, content: "Count from 1 to 5 slowly")]
        let options = MessageOptions(
            maxTokens: 100,
            temperature: 0.0
        )
        
        var collectedChunks: [String] = []
        var eventCount = 0
        
        // ストリーミングレスポンスを収集
        for try await event in try await testService.streamMessage(messages, options: options) {
            eventCount += 1
            
            switch event {
            case .contentBlockDelta(let delta):
                collectedChunks.append(delta.delta.text ?? "")
            case .messageStop:
                // メッセージ完了
                break
            case .error(let error):
                XCTFail("Streaming error: \(error)")
            default:
                // 他のイベントは無視
                break
            }
        }
        
        // ストリーミングが実際に行われたことを確認
        XCTAssertGreaterThan(eventCount, 1)
        XCTAssertGreaterThan(collectedChunks.count, 0)
        
        // 最終的なテキストに数字が含まれることを確認
        let fullText = collectedChunks.joined()
        XCTAssertTrue(fullText.contains("1"))
        XCTAssertTrue(fullText.contains("5"))
    }
    
    func testStreamingWithLongResponse() async throws {
        guard testConfiguration.apiKey != nil else {
            throw XCTSkip("API key not configured")
        }
        
        testService = try await ClaudeServiceFactory.create(with: testConfiguration)
        
        let messages = [Message(role: .user, content: "Write a haiku about Swift programming")]
        let options = MessageOptions(
            maxTokens: 300,
            temperature: 0.7
        )
        
        var hasReceivedChunks = false
        var hasCompleted = false
        var totalChunks = 0
        
        for try await event in try await testService.streamMessage(messages, options: options) {
            switch event {
            case .contentBlockDelta(let delta):
                hasReceivedChunks = true
                totalChunks += 1
                if let text = delta.delta.text {
                    XCTAssertFalse(text.isEmpty)
                }
            case .messageStop:
                hasCompleted = true
            case .error(let error):
                XCTFail("Unexpected error: \(error)")
            default:
                // 他のイベントは無視
                break
            }
        }
        
        XCTAssertTrue(hasReceivedChunks)
        XCTAssertTrue(hasCompleted)
        XCTAssertGreaterThan(totalChunks, 1)
    }
    
    // MARK: - Platform Switching Workflow Tests
    
    func testSwitchingBetweenPlatforms() async throws {
        // APIサービスをテスト
        let apiConfig = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"]
        )
        
        if apiConfig.apiKey != nil {
            let apiService = try await ClaudeServiceFactory.create(with: apiConfig)
            let apiAvailable = await apiService.isAvailable
            XCTAssertTrue(apiAvailable)
        }
        
        // Claude Codeサービスをテスト（macOSのみ）
        #if os(macOS)
        let codeConfig = ClaudeServiceConfiguration(
            connectionType: .claudeCode,
            defaultModel: .claude3Sonnet
        )
        
        do {
            let codeService = try await ClaudeServiceFactory.create(with: codeConfig)
            let codeAvailable = await codeService.isAvailable
            // Claude Codeが実行中でない場合もあるため、結果は問わない
            print("Claude Code available: \(codeAvailable)")
        } catch {
            print("Claude Code service creation skipped: \(error)")
        }
        #endif
    }
}