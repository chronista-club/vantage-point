import Foundation

/// API通信のログを記録するインターセプター
public protocol LoggingDelegate: AnyObject {
    func logRequest(url: URL, method: String, headers: [String: String]?, body: Data?)
    func logResponse(statusCode: Int, headers: [String: String]?, body: Data?, duration: TimeInterval)
    func logError(_ error: Error, duration: TimeInterval)
}

/// ログ出力のためのヘルパー
public struct RequestLogger {
    weak var delegate: LoggingDelegate?
    
    private let dateFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
    
    /// JSONデータを整形
    func prettyPrintJSON(_ data: Data) -> String? {
        guard let json = try? JSONSerialization.jsonObject(with: data),
              let prettyData = try? JSONSerialization.data(withJSONObject: json, options: .prettyPrinted),
              let prettyString = String(data: prettyData, encoding: .utf8) else {
            return nil
        }
        return prettyString
    }
    
    /// ヘッダーを整形
    func formatHeaders(_ headers: [String: String]?) -> String {
        guard let headers = headers else { return "None" }
        return headers.map { "\($0.key): \($0.value)" }.joined(separator: "\n")
    }
    
    /// リクエストをログ
    func logRequest(url: URL, method: String, headers: [String: String]?, body: Data?) {
        delegate?.logRequest(url: url, method: method, headers: headers, body: body)
    }
    
    /// レスポンスをログ
    func logResponse(statusCode: Int, headers: [String: String]?, body: Data?, duration: TimeInterval) {
        delegate?.logResponse(statusCode: statusCode, headers: headers, body: body, duration: duration)
    }
    
    /// エラーをログ
    func logError(_ error: Error, duration: TimeInterval) {
        delegate?.logError(error, duration: duration)
    }
}

/// APIヘッダーのマスキング
extension RequestLogger {
    /// センシティブな情報をマスク
    func maskSensitiveHeaders(_ headers: [String: String]?) -> [String: String]? {
        guard var headers = headers else { return nil }
        
        // APIキーをマスク
        if let apiKey = headers["X-API-Key"] {
            let maskedKey = String(apiKey.prefix(7)) + "..." + String(apiKey.suffix(4))
            headers["X-API-Key"] = maskedKey
        }
        
        // Authorizationヘッダーをマスク
        if let auth = headers["Authorization"] {
            let components = auth.split(separator: " ")
            if components.count >= 2 {
                let token = String(components[1])
                let maskedToken = String(token.prefix(10)) + "..." + String(token.suffix(4))
                headers["Authorization"] = "\(components[0]) \(maskedToken)"
            }
        }
        
        return headers
    }
}