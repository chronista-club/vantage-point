import Foundation
import Logging

/// コンソール出力を管理するロガー
public class ConsoleLogger {
    private let logger: Logger
    private let useColor: Bool
    private let isQuiet: Bool
    private let isVerbose: Bool
    
    public init(
        label: String = "com.vantage.cli",
        useColor: Bool = true,
        isQuiet: Bool = false,
        isVerbose: Bool = false
    ) {
        self.logger = Logger(label: label)
        self.useColor = useColor
        self.isQuiet = isQuiet
        self.isVerbose = isVerbose
    }
    
    // MARK: - Logging Methods
    
    public func debug(_ message: String) {
        guard isVerbose && !isQuiet else { return }
        print(format(message, level: .debug))
    }
    
    public func info(_ message: String) {
        guard !isQuiet else { return }
        print(format(message, level: .info))
    }
    
    public func success(_ message: String) {
        guard !isQuiet else { return }
        print(format(message, level: .success))
    }
    
    public func warning(_ message: String) {
        guard !isQuiet else { return }
        fputs(format(message, level: .warning) + "\n", stderr)
    }
    
    public func error(_ message: String) {
        fputs(format(message, level: .error) + "\n", stderr)
    }
    
    // MARK: - Formatting
    
    private func format(_ message: String, level: LogLevel) -> String {
        guard useColor else {
            return message
        }
        
        let color = level.color
        return "\u{001B}[\(color.rawValue)m\(message)\u{001B}[0m"
    }
    
    // MARK: - Progress Indicators
    
    public func showProgress(_ message: String, current: Int, total: Int) {
        guard !isQuiet else { return }
        
        let percentage = Int((Double(current) / Double(total)) * 100)
        let progressBar = createProgressBar(percentage: percentage)
        
        // カーソルを行頭に戻してから出力
        print("\r\(message): \(progressBar) \(percentage)%", terminator: "")
        fflush(stdout)
        
        if current >= total {
            print() // 改行
        }
    }
    
    private func createProgressBar(percentage: Int, width: Int = 30) -> String {
        let filled = Int(Double(width) * Double(percentage) / 100.0)
        let empty = width - filled
        
        let filledBar = String(repeating: "█", count: filled)
        let emptyBar = String(repeating: "░", count: empty)
        
        return "[\(filledBar)\(emptyBar)]"
    }
}

// MARK: - Supporting Types

private enum LogLevel {
    case debug
    case info
    case success
    case warning
    case error
    
    var color: ANSIColor {
        switch self {
        case .debug: return .gray
        case .info: return .white
        case .success: return .green
        case .warning: return .yellow
        case .error: return .red
        }
    }
}

private enum ANSIColor: String {
    case black = "30"
    case red = "31"
    case green = "32"
    case yellow = "33"
    case blue = "34"
    case magenta = "35"
    case cyan = "36"
    case white = "37"
    case gray = "90"
}