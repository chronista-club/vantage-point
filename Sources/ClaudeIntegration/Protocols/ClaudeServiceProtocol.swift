import Foundation

/// Claude連携サービスのプロトコル
/// プラットフォームに応じて異なる実装を提供する
public protocol ClaudeServiceProtocol: Actor {
    /// サービスが利用可能かどうか
    var isAvailable: Bool { get async }
    
    /// 現在の設定
    var configuration: ClaudeServiceConfiguration { get async }
    
    /// メッセージを送信
    /// - Parameters:
    ///   - messages: 送信するメッセージ履歴
    ///   - options: 送信オプション
    /// - Returns: Claudeからのレスポンス
    func sendMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> ClaudeResponse
    
    /// ストリーミングでメッセージを送信
    /// - Parameters:
    ///   - messages: 送信するメッセージ履歴
    ///   - options: 送信オプション
    /// - Returns: ストリーミングイベントの非同期ストリーム
    func streamMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> AsyncThrowingStream<StreamEvent, Error>
    
    /// 作業ディレクトリを設定
    /// - Parameter path: 作業ディレクトリのパス
    func setWorkingDirectory(_ path: String?) async
    
    /// ファイルコンテキストを追加
    /// - Parameter paths: 含めるファイルパスの配列
    func addFileContext(_ paths: [String]) async
    
    /// サービスの接続状態を確認
    func checkConnection() async throws
}

/// メッセージ送信オプション
public struct MessageOptions: Sendable {
    public let model: ClaudeModel?
    public let system: String?
    public let maxTokens: Int?
    public let temperature: Double?
    public let workingDirectory: String?
    public let fileContexts: [String]?
    
    public init(
        model: ClaudeModel? = nil,
        system: String? = nil,
        maxTokens: Int? = nil,
        temperature: Double? = nil,
        workingDirectory: String? = nil,
        fileContexts: [String]? = nil
    ) {
        self.model = model
        self.system = system
        self.maxTokens = maxTokens
        self.temperature = temperature
        self.workingDirectory = workingDirectory
        self.fileContexts = fileContexts
    }
}

/// Claude連携サービスの設定
public struct ClaudeServiceConfiguration: Sendable {
    public enum ConnectionType: String, Sendable {
        case api = "API"
        case claudeCode = "Claude Code"
    }
    
    public let connectionType: ConnectionType
    public let defaultModel: ClaudeModel
    public let apiKey: String?
    public let timeoutInterval: TimeInterval
    
    public init(
        connectionType: ConnectionType,
        defaultModel: ClaudeModel,
        apiKey: String? = nil,
        timeoutInterval: TimeInterval = 60.0
    ) {
        self.connectionType = connectionType
        self.defaultModel = defaultModel
        self.apiKey = apiKey
        self.timeoutInterval = timeoutInterval
    }
}

/// プラットフォーム別のデフォルト設定
public extension ClaudeServiceConfiguration {
    static var platformDefault: ClaudeServiceConfiguration {
        #if os(macOS)
        return ClaudeServiceConfiguration(
            connectionType: .claudeCode,
            defaultModel: .claude35Sonnet
        )
        #else
        return ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: .claude35Sonnet,
            apiKey: nil  // Keychainから読み込む
        )
        #endif
    }
}