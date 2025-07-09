import Foundation

/// Claude API リクエストモデル
public struct ClaudeRequest: Codable, Sendable {
    /// 使用するモデル
    public let model: String
    
    /// メッセージの配列
    public let messages: [Message]
    
    /// システムプロンプト
    public let system: String?
    
    /// 最大生成トークン数
    public let maxTokens: Int
    
    /// 温度パラメータ（0.0-1.0）
    public let temperature: Double?
    
    /// Top-p サンプリング
    public let topP: Double?
    
    /// Top-k サンプリング
    public let topK: Int?
    
    /// ストップシーケンス
    public let stopSequences: [String]?
    
    /// ストリーミングレスポンス
    public let stream: Bool
    
    /// メタデータ
    public let metadata: [String: String]?
    
    private enum CodingKeys: String, CodingKey {
        case model
        case messages
        case system
        case maxTokens = "max_tokens"
        case temperature
        case topP = "top_p"
        case topK = "top_k"
        case stopSequences = "stop_sequences"
        case stream
        case metadata
    }
    
    /// リクエストを作成
    public init(
        model: String,
        messages: [Message],
        system: String? = nil,
        maxTokens: Int = 1024,
        temperature: Double? = nil,
        topP: Double? = nil,
        topK: Int? = nil,
        stopSequences: [String]? = nil,
        stream: Bool = false,
        metadata: [String: String]? = nil
    ) {
        self.model = model
        self.messages = messages
        self.system = system
        self.maxTokens = maxTokens
        self.temperature = temperature
        self.topP = topP
        self.topK = topK
        self.stopSequences = stopSequences
        self.stream = stream
        self.metadata = metadata
    }
}

/// サポートされているモデル
public enum ClaudeModel: String, CaseIterable, Sendable {
    case claude3Opus = "claude-3-opus-20240229"
    case claude3Sonnet = "claude-3-sonnet-20240229"
    case claude3Haiku = "claude-3-haiku-20240307"
    case claude35Sonnet = "claude-3-5-sonnet-20241022"
    case claude35Haiku = "claude-3-5-haiku-20241022"
    
    /// モデルの表示名
    public var displayName: String {
        switch self {
        case .claude3Opus:
            return "Claude 3 Opus"
        case .claude3Sonnet:
            return "Claude 3 Sonnet"
        case .claude3Haiku:
            return "Claude 3 Haiku"
        case .claude35Sonnet:
            return "Claude 3.5 Sonnet"
        case .claude35Haiku:
            return "Claude 3.5 Haiku"
        }
    }
    
    /// デフォルトの最大トークン数
    public var defaultMaxTokens: Int {
        switch self {
        case .claude3Opus, .claude35Sonnet:
            return 4096
        case .claude3Sonnet:
            return 4096
        case .claude3Haiku, .claude35Haiku:
            return 4096
        }
    }
}