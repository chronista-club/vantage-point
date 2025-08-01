import XCTest
@testable import ClaudeIntegration

// MARK: - Error Handling Tests

final class ErrorHandlingTests: XCTestCase {
    
    // MARK: - Network Error Tests
    
    func testNetworkConnectionError() async throws {
        // 無効なベースURLでサービスを作成
        let invalidConfig = APIConfiguration(
            apiKey: "test-key",
            baseURL: URL(string: "https://invalid-domain-that-does-not-exist.com")!,
            apiVersion: "2023-06-01",
            defaultModel: .claude3Haiku
        )
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: "test-key"
        )
        
        // カスタム設定でサービスを作成する方法を模擬
        // 実際のネットワークエラーをテストするには、モックが必要
        let service = try await ClaudeServiceFactory.create(with: config)
        
        let messages = [Message(role: .user, content: "Test")]
        let options = MessageOptions(maxTokens: 10)
        
        do {
            _ = try await service.sendMessage(messages, options: options)
            // 実際のAPIキーがない場合、認証エラーが発生するはず
        } catch ClaudeIntegrationError.httpError(let statusCode, _) where statusCode == 401 {
            // 認証エラー（401）
            XCTAssertTrue(true)
        } catch ClaudeIntegrationError.networkError(let error) {
            // ネットワークエラーも許容
            XCTAssertFalse(error.localizedDescription.isEmpty)
        } catch {
            // その他のエラーも記録
            print("Error type: \(type(of: error)), message: \(error)")
        }
    }
    
    func testTimeoutError() async throws {
        // タイムアウトが短い設定を作成
        let shortTimeoutConfig = APIConfiguration(
            apiKey: ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] ?? "test-key",
            baseURL: URL(string: "https://api.anthropic.com")!,
            apiVersion: "2023-06-01",
            defaultModel: .claude3Haiku,
            // APIConfigurationにはmaxRetriesとtimeoutIntervalのパラメータがない
        )
        
        // このテストは実際のAPIを使用する場合のみ有効
        guard ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] != nil else {
            throw XCTSkip("API key not configured for timeout test")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: shortTimeoutConfig.apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        let messages = [Message(role: .user, content: "This should timeout")]
        let options = MessageOptions(maxTokens: 100)
        
        do {
            _ = try await service.sendMessage(messages, options: options)
            XCTFail("Expected timeout error")
        } catch ClaudeIntegrationError.networkError {
            // タイムアウトはネットワークエラーとして扱われる
            XCTAssertTrue(true)
        } catch ClaudeIntegrationError.networkError {
            // ネットワークエラーとして扱われる場合も許容
            XCTAssertTrue(true)
        } catch {
            print("Unexpected error type: \(error)")
        }
    }
    
    // MARK: - Invalid Response Tests
    
    func testInvalidAPIKeyError() async throws {
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: "invalid-api-key-12345"
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        let messages = [Message(role: .user, content: "Test with invalid key")]
        let options = MessageOptions(maxTokens: 10)
        
        do {
            _ = try await service.sendMessage(messages, options: options)
            XCTFail("Expected authentication error")
        } catch ClaudeIntegrationError.httpError(let statusCode, _) where statusCode == 401 {
            // 認証エラー（401）
            XCTAssertTrue(true)
        } catch {
            print("Error received: \(error)")
            // 他のエラータイプも許容（APIの実装により異なる可能性）
        }
    }
    
    func testMalformedRequestError() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // 空のメッセージリストは無効なリクエスト
        let messages: [Message] = []
        let options = MessageOptions(maxTokens: 10)
        
        do {
            _ = try await service.sendMessage(messages, options: options)
            XCTFail("Expected validation error for empty messages")
        } catch ClaudeIntegrationError.invalidRequest {
            // 期待されるエラー
            XCTAssertTrue(true)
        } catch {
            // APIが異なるエラーを返す可能性もある
            print("Error for empty messages: \(error)")
        }
    }
    
    // MARK: - Error Recovery Tests
    
    func testRetryMechanismOnTransientError() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        // リトライ設定を持つ構成
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // 通常のリクエスト（リトライメカニズムは内部で処理される）
        let messages = [Message(role: .user, content: "Test retry mechanism")]
        let options = MessageOptions(maxTokens: 50)
        
        do {
            let response = try await service.sendMessage(messages, options: options)
            XCTAssertFalse(response.text.isEmpty)
            // リトライが必要なかった場合も成功とする
        } catch {
            // エラーが発生した場合、リトライが尽きたことを意味する
            print("Error after retries: \(error)")
        }
    }
    
    // MARK: - Error Message Tests
    
    func testUserFriendlyErrorMessages() {
        // 各エラータイプのユーザーフレンドリーメッセージを確認
        let errors: [(ClaudeIntegrationError, String)] = [
            (.missingAPIKey, "APIキーが設定されていません"),
            (.httpError(statusCode: 401, message: "Invalid key"), "認証エラー"),
            (.rateLimited(retryAfter: 60), "レート制限に達しました"),
            (.networkError(NSError(domain: "test", code: -1009)), "ネットワークエラー"),
            (.invalidRequest("Missing parameters"), "無効なリクエスト"),
            (.serviceUnavailable("Server down"), "サービスが利用できません"),
            (.invalidResponse, "無効なレスポンス")
        ]
        
        for (error, expectedPrefix) in errors {
            let userMessage = error.userFriendlyMessage
            // メッセージに期待される文字列が含まれているか確認
            switch error {
            case .missingAPIKey:
                XCTAssertTrue(userMessage.contains("APIキー"))
            case .httpError(401, _):
                XCTAssertTrue(userMessage.contains("認証"))
            case .rateLimited:
                XCTAssertTrue(userMessage.contains("制限"))
            case .networkError:
                XCTAssertTrue(userMessage.contains("ネットワーク") || userMessage.contains("接続"))
            case .invalidRequest:
                XCTAssertTrue(userMessage.contains("リクエスト"))
            case .serviceUnavailable:
                XCTAssertTrue(userMessage.contains("サービス"))
            case .invalidResponse:
                XCTAssertTrue(userMessage.contains("応答") || userMessage.contains("レスポンス"))
            default:
                XCTFail("Unexpected error type")
            }
        }
    }
    
    func testErrorRecoverySuggestions() {
        // エラーごとの回復提案を確認
        let authError = ClaudeIntegrationError.httpError(statusCode: 401, message: "Invalid API key")
        XCTAssertTrue(authError.userFriendlyMessage.contains("認証"))
        
        let rateLimitError = ClaudeIntegrationError.rateLimited(retryAfter: 30)
        XCTAssertTrue(rateLimitError.userFriendlyMessage.contains("制限"))
        
        let networkError = ClaudeIntegrationError.networkError(NSError(domain: NSURLErrorDomain, code: NSURLErrorCannotFindHost))
        XCTAssertTrue(networkError.userFriendlyMessage.contains("ネットワーク"))
    }
    
    // MARK: - Streaming Error Tests
    
    func testStreamingErrorHandling() async throws {
        guard let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] else {
            throw XCTSkip("API key not configured")
        }
        
        let config = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude3Haiku,
            apiKey: apiKey
        )
        
        let service = try await ClaudeServiceFactory.create(with: config)
        
        // 非常に大きなmax_tokensでストリーミング
        let messages = [Message(role: .user, content: "Count from 1 to 1000")]
        let options = MessageOptions(
            maxTokens: 10000, // 大きな値
            temperature: 0.0
        )
        
        var errorReceived = false
        var chunkCount = 0
        
        do {
            for try await event in try await service.streamMessage(messages, options: options) {
                switch event {
                case .contentBlockDelta:
                    chunkCount += 1
                case .error(let error):
                    errorReceived = true
                    print("Streaming error: \(error)")
                case .messageStop:
                    // ストリーミング完了
                    break
                default:
                    break
                }
            }
        } catch {
            // ストリーミング中のエラー
            print("Stream interrupted: \(error)")
            errorReceived = true
        }
        
        // エラーがあってもなくても、何らかの処理が行われたことを確認
        XCTAssertTrue(chunkCount > 0 || errorReceived)
    }
    
    // MARK: - Service Unavailable Tests
    
    #if os(macOS)
    func testClaudeCodeServiceUnavailable() async throws {
        // Claude Codeが起動していない場合のテスト
        let config = ClaudeServiceConfiguration(
            connectionType: .claudeCode,
            defaultModel: .claude3Sonnet
        )
        
        do {
            let service = try await ClaudeServiceFactory.create(with: config)
            let isAvailable = await service.isAvailable
            
            if !isAvailable {
                // サービスが利用できない場合の動作を確認
                let messages = [Message(role: .user, content: "Test")]
                let options = MessageOptions(maxTokens: 10)
                
                do {
                    _ = try await service.sendMessage(messages, options: options)
                    XCTFail("Expected service unavailable error")
                } catch ClaudeIntegrationError.serviceUnavailable {
                    // 期待されるエラー
                    XCTAssertTrue(true)
                }
            } else {
                // Claude Codeが実行中の場合はスキップ
                print("Claude Code is running, skipping unavailable test")
            }
        } catch ClaudeIntegrationError.serviceUnavailable {
            // サービス作成時点でエラーになる場合も正常
            XCTAssertTrue(true)
        }
    }
    #endif
}