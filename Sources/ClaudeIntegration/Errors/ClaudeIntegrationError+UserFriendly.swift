import Foundation

/// ClaudeIntegrationErrorのユーザーフレンドリーな拡張
public extension ClaudeIntegrationError {
    /// ユーザー向けのエラーメッセージ
    var userFriendlyMessage: String {
        switch self {
        case .missingAPIKey:
            return "APIキーが設定されていません。設定メニューからAPIキーを入力してください。"
            
        case .invalidRequest(let message):
            return "リクエストが無効です: \(message)"
            
        case .rateLimited(let retryAfter):
            if let retryAfter = retryAfter {
                return "APIの利用制限に達しました。\(Int(retryAfter))秒後に再試行してください。"
            } else {
                return "APIの利用制限に達しました。しばらく待ってから再試行してください。"
            }
            
        case .serverError(let message):
            return "サーバーエラーが発生しました: \(message)\n少し時間をおいてから再試行してください。"
            
        case .networkError(let error):
            if (error as NSError).code == NSURLErrorNotConnectedToInternet {
                return "インターネット接続がありません。接続を確認してください。"
            } else if (error as NSError).code == NSURLErrorTimedOut {
                return "接続がタイムアウトしました。ネットワーク接続を確認してください。"
            }
            return "ネットワークエラー: \(error.localizedDescription)"
            
        case .decodingError:
            return "レスポンスの解析に失敗しました。APIの仕様が変更された可能性があります。"
            
        case .httpError(let statusCode, let message):
            switch statusCode {
            case 401:
                return "認証エラー: APIキーが無効です。正しいAPIキーを設定してください。"
            case 403:
                return "アクセスが拒否されました。APIキーの権限を確認してください。"
            case 404:
                return "エンドポイントが見つかりません。APIの仕様を確認してください。"
            default:
                return "HTTPエラー (\(statusCode)): \(message ?? "不明なエラー")"
            }
            
        case .invalidResponse:
            return "サーバーからの応答が無効です。しばらく待ってから再試行してください。"
            
        case .streamingError(let message):
            return "ストリーミングエラー: \(message)"
        }
    }
    
    /// エラーの対処法
    var suggestedAction: String? {
        switch self {
        case .missingAPIKey:
            return "メニューバーの鍵アイコンをクリックしてAPIキーを設定してください。"
            
        case .rateLimited:
            return "しばらく待ってから再試行するか、使用頻度を減らしてください。"
            
        case .networkError:
            return "インターネット接続を確認し、必要に応じてVPNやプロキシ設定を確認してください。"
            
        case .httpError(let statusCode, _):
            if statusCode == 401 || statusCode == 403 {
                return "Claude APIのダッシュボードでAPIキーの状態を確認してください。"
            }
            return nil
            
        default:
            return nil
        }
    }
    
    /// リトライ可能かどうか
    var isRetryable: Bool {
        switch self {
        case .rateLimited, .serverError, .networkError:
            return true
        case .httpError(let statusCode, _):
            return statusCode >= 500 || statusCode == 429
        default:
            return false
        }
    }
    
    /// エラーの深刻度
    var severity: ErrorSeverity {
        switch self {
        case .missingAPIKey, .httpError(401, _), .httpError(403, _):
            return .critical
        case .rateLimited:
            return .warning
        case .serverError, .networkError:
            return .temporary
        default:
            return .error
        }
    }
}

/// エラーの深刻度
public enum ErrorSeverity {
    case critical   // 設定が必要
    case error      // 通常のエラー
    case warning    // 警告
    case temporary  // 一時的なエラー
}