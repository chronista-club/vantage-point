import Foundation

/// Claude API直接呼び出しサービス
/// 主にvisionOSで使用される
public actor ClaudeAPIService: ClaudeServiceProtocol {
    
    // MARK: - Properties
    
    private let client: ClaudeClient
    private let apiConfiguration: APIConfiguration
    private var workingDirectory: String?
    private var fileContexts: [String] = []
    
    // MARK: - ClaudeServiceProtocol
    
    public var isAvailable: Bool {
        // API接続は常に利用可能（ネットワーク接続がある限り）
        return true
    }
    
    public var configuration: ClaudeServiceConfiguration {
        ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: apiConfiguration.defaultModel,
            apiKey: apiConfiguration.apiKey,
            timeoutInterval: apiConfiguration.timeoutInterval
        )
    }
    
    // MARK: - Initialization
    
    public init(apiConfiguration: APIConfiguration) {
        self.apiConfiguration = apiConfiguration
        self.client = ClaudeClient(configuration: apiConfiguration)
    }
    
    // MARK: - Message Sending
    
    public func sendMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> ClaudeResponse {
        // システムプロンプトを構築
        let systemPrompt = buildSystemPrompt(options: options)
        
        // メッセージ送信
        return try await client.sendMessage(
            messages,
            model: options?.model ?? apiConfiguration.defaultModel,
            system: systemPrompt,
            maxTokens: options?.maxTokens,
            temperature: options?.temperature
        )
    }
    
    public func streamMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        // システムプロンプトを構築
        let systemPrompt = buildSystemPrompt(options: options)
        
        // ストリーミング送信
        return try await client.streamMessage(
            messages,
            model: options?.model ?? apiConfiguration.defaultModel,
            system: systemPrompt,
            maxTokens: options?.maxTokens,
            temperature: options?.temperature
        )
    }
    
    // MARK: - Context Management
    
    public func setWorkingDirectory(_ path: String?) async {
        self.workingDirectory = path
    }
    
    public func addFileContext(_ paths: [String]) async {
        self.fileContexts.append(contentsOf: paths)
    }
    
    public func checkConnection() async throws {
        // 簡単な接続テスト
        let testMessage = Message(role: .user, content: "Hi")
        _ = try await client.sendMessage(
            [testMessage],
            model: .claude3Haiku,  // 最も安価なモデルでテスト
            maxTokens: 10
        )
    }
    
    // MARK: - Logging
    
    public func setLoggingDelegate(_ delegate: LoggingDelegate?) {
        Task {
            await client.setLoggingDelegate(delegate)
        }
    }
    
    // MARK: - Private Methods
    
    private func buildSystemPrompt(options: MessageOptions?) -> String? {
        var prompts: [String] = []
        
        // デフォルトシステムプロンプト
        if let system = options?.system {
            prompts.append(system)
        }
        
        // 作業ディレクトリコンテキスト
        let workDir = options?.workingDirectory ?? self.workingDirectory
        if let workDir = workDir {
            prompts.append("Working directory: \(workDir)")
        }
        
        // ファイルコンテキスト
        let contexts = (options?.fileContexts ?? []) + self.fileContexts
        if !contexts.isEmpty {
            prompts.append("File contexts: \(contexts.joined(separator: ", "))")
        }
        
        return prompts.isEmpty ? nil : prompts.joined(separator: "\n\n")
    }
}