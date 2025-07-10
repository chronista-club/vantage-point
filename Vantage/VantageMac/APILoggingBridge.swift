import Foundation
import ClaudeAPI

/// ChatViewModelとClaudeAPIのログを橋渡しするクラス
@MainActor
class APILoggingBridge: LoggingDelegate {
    weak var viewModel: ChatViewModel?
    
    private let jsonFormatter = JSONFormatter()
    
    init(viewModel: ChatViewModel) {
        self.viewModel = viewModel
    }
    
    func logRequest(url: URL, method: String, headers: [String: String]?, body: Data?) {
        var logMessages: [String] = []
        
        // リクエスト開始
        logMessages.append("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
        logMessages.append("📤 HTTP Request")
        logMessages.append("\(method) \(url.absoluteString)")
        
        // ヘッダー
        if let headers = headers, !headers.isEmpty {
            logMessages.append("\n📋 Headers:")
            for (key, value) in headers.sorted(by: { $0.key < $1.key }) {
                logMessages.append("  \(key): \(value)")
            }
        }
        
        // ボディ
        if let body = body {
            logMessages.append("\n📦 Request Body:")
            if let prettyJSON = jsonFormatter.prettyPrint(body) {
                logMessages.append(prettyJSON)
            } else if let bodyString = String(data: body, encoding: .utf8) {
                logMessages.append(bodyString)
            } else {
                logMessages.append("  [Binary data: \(body.count) bytes]")
            }
        }
        
        // ログ出力
        for message in logMessages {
            viewModel?.addLog(level: .debug, message: message)
        }
    }
    
    func logResponse(statusCode: Int, headers: [String: String]?, body: Data?, duration: TimeInterval) {
        var logMessages: [String] = []
        
        // レスポンス情報
        logMessages.append("\n📥 HTTP Response")
        logMessages.append("Status: \(statusCode) \(HTTPURLResponse.localizedString(forStatusCode: statusCode))")
        logMessages.append("Duration: \(String(format: "%.3f", duration))s")
        
        // ヘッダー
        if let headers = headers, !headers.isEmpty {
            logMessages.append("\n📋 Response Headers:")
            for (key, value) in headers.sorted(by: { $0.key < $1.key }) {
                logMessages.append("  \(key): \(value)")
            }
        }
        
        // ボディ（大きすぎる場合は要約）
        if let body = body {
            logMessages.append("\n📦 Response Body:")
            if body.count > 1000 {
                // 大きなレスポンスは要約
                if let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                    if let content = (json["content"] as? [[String: Any]])?.first,
                       let text = content["text"] as? String {
                        let preview = String(text.prefix(200))
                        logMessages.append("  Content preview: \(preview)...")
                        logMessages.append("  [Total response: \(body.count) bytes]")
                    }
                } else {
                    logMessages.append("  [Large response: \(body.count) bytes]")
                }
            } else if let prettyJSON = jsonFormatter.prettyPrint(body) {
                logMessages.append(prettyJSON)
            } else if let bodyString = String(data: body, encoding: .utf8) {
                logMessages.append(bodyString)
            }
        }
        
        logMessages.append("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
        
        // ログ出力
        let level: ConsoleLog.LogLevel = statusCode >= 400 ? .error : .debug
        for message in logMessages {
            viewModel?.addLog(level: level, message: message)
        }
    }
    
    func logError(_ error: Error, duration: TimeInterval) {
        viewModel?.addLog(level: .error, message: "❌ Request failed after \(String(format: "%.3f", duration))s")
        viewModel?.addLog(level: .error, message: "Error: \(error.localizedDescription)")
        
        if let apiError = error as? ClaudeAPIError {
            switch apiError {
            case .rateLimited(let retryAfter):
                if let retryAfter = retryAfter {
                    viewModel?.addLog(level: .warning, message: "⏰ Rate limited. Retry after: \(retryAfter)s")
                }
            case .invalidRequest(let message):
                viewModel?.addLog(level: .error, message: "🚫 Invalid request: \(message)")
            case .serverError(let message):
                viewModel?.addLog(level: .error, message: "🔥 Server error: \(message)")
            case .httpError(let statusCode, let message):
                viewModel?.addLog(level: .error, message: "📡 HTTP \(statusCode): \(message ?? "Unknown error")")
            default:
                break
            }
        }
    }
}

/// JSON整形ヘルパー
private struct JSONFormatter {
    func prettyPrint(_ data: Data) -> String? {
        guard let json = try? JSONSerialization.jsonObject(with: data),
              let prettyData = try? JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted, .sortedKeys]),
              let prettyString = String(data: prettyData, encoding: .utf8) else {
            return nil
        }
        
        // インデントを調整（2スペース）
        let lines = prettyString.split(separator: "\n")
        return lines.map { line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            let indentLevel = (line.count - trimmed.count) / 2
            return String(repeating: "  ", count: indentLevel) + trimmed
        }.joined(separator: "\n")
    }
}