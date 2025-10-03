import Foundation
import ClaudeIntegration

// MARK: - Protocol

protocol ClaudeAPIServiceProtocol {
    func setAPIKey(_ key: String) async throws
    func streamMessage(_ messages: [Message], model: ClaudeModel, system: String?) async throws -> AsyncThrowingStream<StreamEvent, Error>
    func validateAPIKey() async throws -> Bool
}

// MARK: - Implementation

final class ClaudeAPIService: ClaudeAPIServiceProtocol {
    private var client: ClaudeClient?
    private let keychainManager: KeychainManager
    private weak var loggingDelegate: APILoggingBridge?
    
    init(keychainManager: KeychainManager = .shared) {
        self.keychainManager = keychainManager
    }
    
    func setLoggingDelegate(_ delegate: APILoggingBridge?) {
        self.loggingDelegate = delegate
        Task {
            await client?.setLoggingDelegate(delegate)
        }
    }
    
    func setAPIKey(_ key: String) async throws {
        guard !key.isEmpty else {
            throw ClaudeIntegrationError.invalidAPIKey
        }
        
        // Keychainに保存
        try await keychainManager.saveAPIKey(key)
        
        // クライアントを作成
        client = ClaudeClient(apiKey: key)
        await client?.setLoggingDelegate(loggingDelegate)
    }
    
    func loadSavedAPIKey() async throws {
        let apiKey = try await keychainManager.loadAPIKey()
        client = ClaudeClient(apiKey: apiKey)
        await client?.setLoggingDelegate(loggingDelegate)
    }
    
    func streamMessage(_ messages: [Message], model: ClaudeModel, system: String?) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        guard let client = client else {
            throw ClaudeIntegrationError.missingAPIKey
        }
        
        return try await client.streamMessage(messages, model: model, system: system)
    }
    
    func validateAPIKey() async throws -> Bool {
        guard client != nil else {
            return false
        }
        
        // 簡単なメッセージを送信してAPIキーの有効性を確認
        do {
            let testMessages = [Message(role: .user, content: "test")]
            let stream = try await streamMessage(testMessages, model: .claude35Sonnet, system: nil)
            
            // ストリームから最初のイベントを取得
            for try await event in stream {
                switch event {
                case .messageStart, .contentBlockStart, .contentBlockDelta:
                    return true
                case .error:
                    return false
                default:
                    continue
                }
            }
            return true
        } catch {
            return false
        }
    }
    
    var hasAPIKey: Bool {
        client != nil
    }
}