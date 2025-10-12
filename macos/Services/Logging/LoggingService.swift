import Foundation
import SwiftUI
import ClaudeIntegration

// MARK: - Models

struct ConsoleLog: Identifiable, Hashable {
    let id = UUID()
    let timestamp: Date
    let level: LogLevel
    let message: String
    
    enum LogLevel: String, CaseIterable {
        case debug = "DEBUG"
        case info = "INFO"
        case warning = "WARNING"
        case error = "ERROR"
        
        var color: Color {
            switch self {
            case .debug: return .gray
            case .info: return .primary
            case .warning: return .orange
            case .error: return .red
            }
        }
        
        var icon: String {
            switch self {
            case .debug: return "ant.circle"
            case .info: return "info.circle"
            case .warning: return "exclamationmark.triangle"
            case .error: return "xmark.circle"
            }
        }
    }
}

// MARK: - Protocol

@MainActor
protocol LoggingServiceProtocol: AnyObject {
    var logs: [ConsoleLog] { get }
    var filteredLogs: [ConsoleLog] { get }

    func log(_ level: ConsoleLog.LogLevel, _ message: String)
    func debug(_ message: String)
    func info(_ message: String)
    func warning(_ message: String)
    func error(_ message: String)
    func clearLogs()
    func setFilter(_ levels: Set<ConsoleLog.LogLevel>)
}

// MARK: - Implementation

@MainActor
final class LoggingService: ObservableObject, LoggingServiceProtocol {
    @Published private(set) var logs: [ConsoleLog] = []
    @Published var enabledLevels: Set<ConsoleLog.LogLevel> = Set(ConsoleLog.LogLevel.allCases)
    
    private let maxLogCount = 1000
    private let dateFormatter: DateFormatter
    
    var filteredLogs: [ConsoleLog] {
        logs.filter { enabledLevels.contains($0.level) }
    }
    
    init() {
        self.dateFormatter = DateFormatter()
        self.dateFormatter.dateFormat = "HH:mm:ss.SSS"
        
        // 起動時のログ
        info("LoggingService initialized")
    }
    
    func log(_ level: ConsoleLog.LogLevel, _ message: String) {
        let log = ConsoleLog(
            timestamp: Date(),
            level: level,
            message: message
        )
        logs.append(log)
        
        // ログの最大数を制限
        if logs.count > maxLogCount {
            logs.removeFirst(logs.count - maxLogCount)
        }
        
        // デバッグビルドの場合はコンソールにも出力
        #if DEBUG
        let timestamp = dateFormatter.string(from: log.timestamp)
        print("[\(timestamp)] [\(level.rawValue)] \(message)")
        #endif
    }
    
    func debug(_ message: String) {
        log(.debug, message)
    }
    
    func info(_ message: String) {
        log(.info, message)
    }
    
    func warning(_ message: String) {
        log(.warning, message)
    }
    
    func error(_ message: String) {
        log(.error, message)
    }
    
    func clearLogs() {
        logs.removeAll()
        info("Logs cleared")
    }
    
    func setFilter(_ levels: Set<ConsoleLog.LogLevel>) {
        enabledLevels = levels
    }
    
    func exportLogs() -> String {
        let header = "Vantage for Mac - Console Log Export\n"
        let exportDate = "Export Date: \(Date())\n"
        let separator = String(repeating: "=", count: 50) + "\n\n"
        
        let logEntries = logs.map { log in
            let timestamp = dateFormatter.string(from: log.timestamp)
            return "[\(timestamp)] [\(log.level.rawValue)] \(log.message)"
        }.joined(separator: "\n")
        
        return header + exportDate + separator + logEntries
    }
}

// MARK: - API Logging Bridge Extension

extension LoggingService {
    func createAPILoggingBridge() -> APILoggingBridge {
        return APILoggingBridge(loggingService: self)
    }
}

// MARK: - API Logging Bridge Refactored

final class APILoggingBridge: LoggingDelegate, @unchecked Sendable {
    private weak var loggingService: LoggingServiceProtocol?
    private let jsonFormatter = JSONFormatter()
    
    init(loggingService: LoggingServiceProtocol) {
        self.loggingService = loggingService
    }
    
    func logRequest(url: URL, method: String, headers: [String: String]?, body: Data?) {
        Task { @MainActor in
            loggingService?.debug("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
            loggingService?.debug("📤 HTTP Request")
            loggingService?.debug("\(method) \(url.absoluteString)")
            
            // ヘッダー
            if let headers = headers, !headers.isEmpty {
                loggingService?.debug("\n📋 Headers:")
                for (key, value) in headers.sorted(by: { $0.key < $1.key }) {
                    if key.lowercased() == "x-api-key" {
                        loggingService?.debug("  \(key): [REDACTED]")
                    } else {
                        loggingService?.debug("  \(key): \(value)")
                    }
                }
            }
            
            // ボディ
            if let body = body {
                loggingService?.debug("\n📦 Request Body:")
                if let prettyJSON = jsonFormatter.prettyPrint(body) {
                    loggingService?.debug(prettyJSON)
                } else if let bodyString = String(data: body, encoding: .utf8) {
                    loggingService?.debug(bodyString)
                } else {
                    loggingService?.debug("  [Binary data: \(body.count) bytes]")
                }
            }
        }
    }
    
    func logResponse(statusCode: Int, headers: [String: String]?, body: Data?, duration: TimeInterval) {
        Task { @MainActor in
            loggingService?.debug("\n📥 HTTP Response")
            loggingService?.debug("Status: \(statusCode) \(HTTPURLResponse.localizedString(forStatusCode: statusCode))")
            loggingService?.debug("Duration: \(String(format: "%.3f", duration))s")
            
            // ヘッダー
            if let headers = headers, !headers.isEmpty {
                loggingService?.debug("\n📋 Response Headers:")
                for (key, value) in headers.sorted(by: { $0.key < $1.key }) {
                    loggingService?.debug("  \(key): \(value)")
                }
            }
            
            // ボディ（大きすぎる場合は要約）
            if let body = body {
                loggingService?.debug("\n📦 Response Body:")
                if body.count > 1000 {
                    // 大きなレスポンスは要約
                    if let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                        if let content = (json["content"] as? [[String: Any]])?.first,
                           let text = content["text"] as? String {
                            let preview = String(text.prefix(200))
                            loggingService?.debug("  Content preview: \(preview)...")
                            loggingService?.debug("  [Total response: \(body.count) bytes]")
                        }
                    } else {
                        loggingService?.debug("  [Large response: \(body.count) bytes]")
                    }
                } else if let prettyJSON = jsonFormatter.prettyPrint(body) {
                    loggingService?.debug(prettyJSON)
                } else if let bodyString = String(data: body, encoding: .utf8) {
                    loggingService?.debug(bodyString)
                }
            }
            
            loggingService?.debug("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
        }
    }
    
    func logError(_ error: Error, duration: TimeInterval) {
        Task { @MainActor in
            loggingService?.error("❌ Request failed after \(String(format: "%.3f", duration))s")
            loggingService?.error("Error: \(error.localizedDescription)")
            
            if let apiError = error as? ClaudeIntegrationError {
                switch apiError {
                case .rateLimited(let retryAfter):
                    if let retryAfter = retryAfter {
                        loggingService?.warning("⏰ Rate limited. Retry after: \(retryAfter)s")
                    }
                case .invalidRequest(let message):
                    loggingService?.error("🚫 Invalid request: \(message)")
                case .serverError(let message):
                    loggingService?.error("🔥 Server error: \(message)")
                case .httpError(let statusCode, let message):
                    loggingService?.error("📡 HTTP \(statusCode): \(message ?? "Unknown error")")
                default:
                    break
                }
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