import Foundation

/// Claude API レスポンスモデル
public struct ClaudeResponse: Codable, Sendable {
    /// レスポンスID
    public let id: String
    
    /// レスポンスタイプ
    public let type: String
    
    /// 使用されたモデル
    public let model: String
    
    /// メッセージの役割（通常は "assistant"）
    public let role: String
    
    /// コンテンツの配列
    public let content: [ResponseContent]
    
    /// 停止理由
    public let stopReason: String?
    
    /// 停止シーケンス
    public let stopSequence: String?
    
    /// 使用量情報
    public let usage: Usage
    
    private enum CodingKeys: String, CodingKey {
        case id
        case type
        case model
        case role
        case content
        case stopReason = "stop_reason"
        case stopSequence = "stop_sequence"
        case usage
    }
    
    /// レスポンスからテキストを取得
    public var text: String {
        content.compactMap { $0.text }.joined()
    }
}

/// レスポンスコンテンツ
public struct ResponseContent: Codable, Sendable {
    /// コンテンツタイプ
    public let type: String
    
    /// テキストコンテンツ
    public let text: String?
}

/// 使用量情報
public struct Usage: Codable, Sendable {
    /// 入力トークン数
    public let inputTokens: Int
    
    /// 出力トークン数
    public let outputTokens: Int
    
    private enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
    }
    
    /// 合計トークン数
    public var totalTokens: Int {
        inputTokens + outputTokens
    }
}

/// ストリーミングレスポンスのイベント
public enum StreamEvent: Sendable {
    case messageStart(MessageStart)
    case contentBlockStart(ContentBlockStart)
    case contentBlockDelta(ContentBlockDelta)
    case contentBlockStop(ContentBlockStop)
    case messageStop(MessageStop)
    case error(ClaudeError)
}

/// メッセージ開始イベント
public struct MessageStart: Codable, Sendable {
    public let type: String
    public let message: PartialMessage
}

/// 部分的なメッセージ
public struct PartialMessage: Codable, Sendable {
    public let id: String
    public let type: String
    public let role: String
    public let model: String
    public let usage: Usage?
}

/// コンテンツブロック開始イベント
public struct ContentBlockStart: Codable, Sendable {
    public let type: String
    public let index: Int
    public let contentBlock: ContentBlock
    
    private enum CodingKeys: String, CodingKey {
        case type
        case index
        case contentBlock = "content_block"
    }
}

/// コンテンツブロック
public struct ContentBlock: Codable, Sendable {
    public let type: String
    public let text: String?
}

/// コンテンツブロックデルタイベント
public struct ContentBlockDelta: Codable, Sendable {
    public let type: String
    public let index: Int
    public let delta: Delta
}

/// デルタ
public struct Delta: Codable, Sendable {
    public let type: String
    public let text: String?
}

/// コンテンツブロック停止イベント
public struct ContentBlockStop: Codable, Sendable {
    public let type: String
    public let index: Int
}

/// メッセージ停止イベント
public struct MessageStop: Codable, Sendable {
    public let type: String
}

/// エラーレスポンス
public struct ClaudeError: Error, Codable, Sendable {
    public let type: String
    public let message: String
    
    /// エラーの詳細
    public var error: ErrorDetail?
    
    public struct ErrorDetail: Codable, Sendable {
        public let type: String
        public let message: String
    }
}