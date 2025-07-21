import Foundation

/// Claude Integration エラー
public enum ClaudeIntegrationError: Error, LocalizedError, Sendable {
    /// ネットワークエラー
    case networkError(Error)
    
    /// 無効なレスポンス
    case invalidResponse
    
    /// HTTPエラー
    case httpError(statusCode: Int, message: String?)
    
    /// デコードエラー
    case decodingError(Error)
    
    /// APIキーが未設定
    case missingAPIKey
    
    /// レート制限
    case rateLimited(retryAfter: TimeInterval?)
    
    /// 無効なリクエスト
    case invalidRequest(String)
    
    /// サーバーエラー
    case serverError(String)
    
    /// ストリーミングエラー
    case streamingError(String)
    
    public var errorDescription: String? {
        switch self {
        case .networkError(let error):
            return "ネットワークエラー: \(error.localizedDescription)"
        case .invalidResponse:
            return "無効なレスポンスを受信しました"
        case .httpError(let statusCode, let message):
            return "HTTPエラー (\(statusCode)): \(message ?? "不明なエラー")"
        case .decodingError(let error):
            return "レスポンスの解析エラー: \(error.localizedDescription)"
        case .missingAPIKey:
            return "APIキーが設定されていません"
        case .rateLimited(let retryAfter):
            if let retryAfter = retryAfter {
                return "レート制限に達しました。\(Int(retryAfter))秒後に再試行してください"
            } else {
                return "レート制限に達しました"
            }
        case .invalidRequest(let message):
            return "無効なリクエスト: \(message)"
        case .serverError(let message):
            return "サーバーエラー: \(message)"
        case .streamingError(let message):
            return "ストリーミングエラー: \(message)"
        }
    }
}

/// HTTP ステータスコードの拡張
extension Int {
    /// 成功ステータスコードかどうか
    var isSuccessHTTPCode: Bool {
        return 200...299 ~= self
    }
    
    /// クライアントエラーかどうか
    var isClientErrorHTTPCode: Bool {
        return 400...499 ~= self
    }
    
    /// サーバーエラーかどうか
    var isServerErrorHTTPCode: Bool {
        return 500...599 ~= self
    }
}