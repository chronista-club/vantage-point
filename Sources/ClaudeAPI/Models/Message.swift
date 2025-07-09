import Foundation

/// Claude APIのメッセージモデル
public struct Message: Codable, Equatable, Sendable {
    /// メッセージの役割
    public enum Role: String, Codable, Sendable {
        case user
        case assistant
        case system
    }
    
    /// メッセージの役割
    public let role: Role
    
    /// メッセージの内容
    public let content: String
    
    /// メッセージを作成
    public init(role: Role, content: String) {
        self.role = role
        self.content = content
    }
}

/// メッセージのコンテンツタイプ
public enum ContentType: String, Codable, Sendable {
    case text
    case image
}

/// 拡張メッセージコンテンツ（将来の画像サポート用）
public struct MessageContent: Codable, Equatable, Sendable {
    public let type: ContentType
    public let text: String?
    public let source: ImageSource?
    
    /// テキストコンテンツを作成
    public static func text(_ content: String) -> MessageContent {
        MessageContent(type: .text, text: content, source: nil)
    }
    
    /// 画像コンテンツを作成
    public static func image(mediaType: String, data: Data) -> MessageContent {
        MessageContent(
            type: .image,
            text: nil,
            source: ImageSource(type: "base64", mediaType: mediaType, data: data.base64EncodedString())
        )
    }
}

/// 画像ソース
public struct ImageSource: Codable, Equatable, Sendable {
    public let type: String
    public let mediaType: String
    public let data: String
    
    enum CodingKeys: String, CodingKey {
        case type
        case mediaType = "media_type"
        case data
    }
}