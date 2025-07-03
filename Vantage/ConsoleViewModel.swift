//
//  ConsoleViewModel.swift
//  Vantage
//
//  Created by Makoto Itoh on 2025/07/03.
//

import SwiftUI
import Observation

/// コンソールログのレベル
enum LogLevel: String, CaseIterable {
    case debug = "DEBUG"
    case info = "INFO"
    case warning = "WARNING"
    case error = "ERROR"
    
    var color: Color {
        switch self {
        case .debug:
            return .gray
        case .info:
            return .white
        case .warning:
            return .yellow
        case .error:
            return .red
        }
    }
    
    var symbol: String {
        switch self {
        case .debug:
            return "🔍"
        case .info:
            return "ℹ️"
        case .warning:
            return "⚠️"
        case .error:
            return "❌"
        }
    }
}

/// コンソールログメッセージ
struct ConsoleMessage: Identifiable {
    let id = UUID()
    let timestamp: Date
    let level: LogLevel
    let category: String
    let message: String
    
    var formattedTimestamp: String {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss.SSS"
        return formatter.string(from: timestamp)
    }
    
    var formattedMessage: String {
        "[\(formattedTimestamp)] \(level.symbol) [\(category)] \(message)"
    }
}

/// コンソールのビューモデル
@Observable
class ConsoleViewModel {
    /// 保存するログメッセージ
    private(set) var messages: [ConsoleMessage] = []
    
    /// 最大メッセージ数
    var maxMessages: Int = 1000
    
    /// 自動スクロールの有効/無効
    var autoScroll: Bool = true
    
    /// フィルタリングレベル
    var filterLevel: LogLevel? = nil
    
    /// カテゴリフィルター
    var filterCategory: String? = nil
    
    /// フィルター済みメッセージ
    var filteredMessages: [ConsoleMessage] {
        messages.filter { message in
            // レベルフィルター
            if let filterLevel = filterLevel, message.level != filterLevel {
                return false
            }
            
            // カテゴリフィルター
            if let filterCategory = filterCategory, !filterCategory.isEmpty,
               !message.category.localizedCaseInsensitiveContains(filterCategory) {
                return false
            }
            
            return true
        }
    }
    
    /// すべてのカテゴリを取得
    var allCategories: [String] {
        Array(Set(messages.map { $0.category })).sorted()
    }
    
    /// 初期化
    init() {
        // デモメッセージを追加
        addDemoMessages()
    }
    
    /// ログメッセージを追加
    func log(_ message: String, level: LogLevel = .info, category: String = "General") {
        let consoleMessage = ConsoleMessage(
            timestamp: Date(),
            level: level,
            category: category,
            message: message
        )
        
        messages.append(consoleMessage)
        
        // 最大数を超えた場合、古いメッセージを削除
        if messages.count > maxMessages {
            messages.removeFirst(messages.count - maxMessages)
        }
    }
    
    /// デバッグログ
    func debug(_ message: String, category: String = "General") {
        log(message, level: .debug, category: category)
    }
    
    /// 情報ログ
    func info(_ message: String, category: String = "General") {
        log(message, level: .info, category: category)
    }
    
    /// 警告ログ
    func warning(_ message: String, category: String = "General") {
        log(message, level: .warning, category: category)
    }
    
    /// エラーログ
    func error(_ message: String, category: String = "General") {
        log(message, level: .error, category: category)
    }
    
    /// すべてのメッセージをクリア
    func clear() {
        messages.removeAll()
    }
    
    /// デモメッセージを追加
    private func addDemoMessages() {
        info("コンソールシステムが初期化されました", category: "System")
        debug("デバッグモードが有効です", category: "System")
        info("Vision Proデバイスが検出されました", category: "Device")
        warning("メモリ使用量が50%を超えています", category: "Performance")
        info("3Dレンダリングエンジンを起動中...", category: "Renderer")
        debug("Metal APIバージョン: 3.0", category: "Renderer")
        info("ARKitセッションを開始しました", category: "ARKit")
        error("ネットワーク接続がありません", category: "Network")
        info("アプリケーションの準備が完了しました", category: "System")
    }
}

/// グローバルコンソールインスタンス
let globalConsole = ConsoleViewModel()