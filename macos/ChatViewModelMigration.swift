import Foundation
import SwiftUI
import ClaudeIntegration

/// 既存のChatViewModelと互換性を保つための拡張
/// リファクタリング完了後は削除予定
extension ChatViewModel {
    
    // MARK: - Legacy Properties
    
    var sessionManager: SessionService {
        sessionService
    }
    
    var messageHistory: [String] {
        messageService.messageHistory
    }
    
    var retryManager: RetryManager {
        RetryManager()
    }

    // MARK: - Legacy Methods
    
    func addLog(level: ConsoleLog.LogLevel, message: String) {
        loggingService.log(level, message)
    }
    
    func addToMessageHistory(_ message: String) {
        messageService.addToHistory(message)
    }
}