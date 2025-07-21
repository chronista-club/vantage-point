import Foundation

/// Claude API 設定
public struct APIConfiguration: Sendable {
    /// APIキー
    public let apiKey: String
    
    /// ベースURL
    public let baseURL: URL
    
    /// APIバージョン
    public let apiVersion: String
    
    /// タイムアウト間隔（秒）
    public let timeoutInterval: TimeInterval
    
    /// リトライ回数
    public let maxRetries: Int
    
    /// デフォルトモデル
    public let defaultModel: ClaudeModel
    
    /// デフォルト設定
    public static let `default` = APIConfiguration(
        apiKey: "",
        baseURL: URL(string: "https://api.anthropic.com")!,
        apiVersion: "2023-06-01",
        timeoutInterval: 30,
        maxRetries: 3,
        defaultModel: .claude35Haiku
    )
    
    /// カスタム設定を作成
    public init(
        apiKey: String,
        baseURL: URL = URL(string: "https://api.anthropic.com")!,
        apiVersion: String = "2023-06-01",
        timeoutInterval: TimeInterval = 30,
        maxRetries: Int = 3,
        defaultModel: ClaudeModel = .claude35Haiku
    ) {
        self.apiKey = apiKey
        self.baseURL = baseURL
        self.apiVersion = apiVersion
        self.timeoutInterval = timeoutInterval
        self.maxRetries = maxRetries
        self.defaultModel = defaultModel
    }
    
    /// メッセージエンドポイントURL
    public var messagesEndpoint: URL {
        baseURL.appendingPathComponent("v1/messages")
    }
}

/// API ヘッダー
public enum APIHeaders {
    public static func headers(for configuration: APIConfiguration) -> [String: String] {
        [
            "x-api-key": configuration.apiKey,
            "anthropic-version": configuration.apiVersion,
            "content-type": "application/json"
        ]
    }
    
    public static func streamingHeaders(for configuration: APIConfiguration) -> [String: String] {
        var headers = headers(for: configuration)
        headers["accept"] = "text/event-stream"
        return headers
    }
}