import Foundation
import ClaudeIntegration

// MARK: - Protocol

protocol SendMessageUseCaseProtocol {
    func execute(
        text: String,
        model: ClaudeModel,
        systemPrompt: String,
        onStreamUpdate: @escaping (String) -> Void
    ) async throws -> MessageSendResult
}

// MARK: - Result Model

struct MessageSendResult {
    let userMessage: ChatMessage
    let assistantMessage: ChatMessage
    let tokenMetrics: TokenMetrics
}

struct TokenMetrics {
    let inputTokens: Int
    let outputTokens: Int
    let charactersReceived: Int
    let duration: TimeInterval
    
    var charactersPerSecond: Double {
        duration > 0 ? Double(charactersReceived) / duration : 0
    }
}

// MARK: - Implementation

final class SendMessageUseCase: SendMessageUseCaseProtocol {
    private let apiService: ClaudeAPIServiceProtocol
    private let messageService: MessageServiceProtocol
    private let loggingService: LoggingServiceProtocol
    private let retryManager: RetryManager
    
    init(
        apiService: ClaudeAPIServiceProtocol,
        messageService: MessageServiceProtocol,
        loggingService: LoggingServiceProtocol,
        retryManager: RetryManager = RetryManager()
    ) {
        self.apiService = apiService
        self.messageService = messageService
        self.loggingService = loggingService
        self.retryManager = retryManager
    }
    
    func execute(
        text: String,
        model: ClaudeModel,
        systemPrompt: String,
        onStreamUpdate: @escaping (String) -> Void
    ) async throws -> MessageSendResult {
        
        // ユーザーメッセージを追加
        let userMessage = await MainActor.run {
            messageService.addUserMessage(text)
        }
        
        loggingService.info("メッセージを送信: \(text.prefix(50))\(text.count > 50 ? "..." : "")")
        
        // API用のメッセージを取得
        let apiMessages = await MainActor.run {
            messageService.getAPIMessages()
        }
        
        // 入力トークンの推定（簡易計算：4文字≒1トークン）
        let inputText = apiMessages.map { $0.content }.joined(separator: " ")
        let estimatedInputTokens = inputText.count / 4
        
        // アシスタントメッセージを作成
        let assistantMessage = await MainActor.run {
            messageService.addAssistantMessage()
        }
        
        // ストリーミング処理
        let startTime = Date()
        var totalCharacters = 0
        
        do {
            loggingService.info("Claude API (\(model.displayName)) にリクエストを送信中...")
            
            let stream = try await retryManager.performWithRetry(
                operation: {
                    try await self.apiService.streamMessage(
                        apiMessages,
                        model: model,
                        system: systemPrompt
                    )
                },
                onError: { error, retryCount in
                    self.loggingService.warning("リトライ \(retryCount)回目: \(error.localizedDescription)")
                }
            )
            
            loggingService.debug("レスポンスのストリーミングを開始")
            
            for try await event in stream {
                switch event {
                case .contentBlockDelta(let delta):
                    if let text = delta.delta.text {
                        totalCharacters += text.count
                        await MainActor.run {
                            self.messageService.appendToMessage(id: assistantMessage.id, content: text)
                        }
                        onStreamUpdate(text)
                    }
                    
                case .messageStop:
                    await MainActor.run {
                        self.messageService.updateMessageTimestamp(id: assistantMessage.id)
                    }
                    
                case .error(let error):
                    loggingService.error("ストリーミングエラー: \(error.message)")
                    throw ClaudeIntegrationError.serverError(error.message)
                    
                default:
                    break
                }
            }
            
            let duration = Date().timeIntervalSince(startTime)
            let estimatedOutputTokens = totalCharacters / 4
            
            let metrics = TokenMetrics(
                inputTokens: estimatedInputTokens,
                outputTokens: estimatedOutputTokens,
                charactersReceived: totalCharacters,
                duration: duration
            )
            
            loggingService.info(
                "レスポンスを受信完了 (約\(totalCharacters)文字, " +
                "\(String(format: "%.1f", metrics.charactersPerSecond))文字/秒, " +
                "推定\(metrics.inputTokens + metrics.outputTokens)トークン)"
            )
            
            // 最終的なメッセージを取得
            let finalAssistantMessage = await MainActor.run {
                messageService.messages.first { $0.id == assistantMessage.id } ?? assistantMessage
            }
            
            return MessageSendResult(
                userMessage: userMessage,
                assistantMessage: finalAssistantMessage,
                tokenMetrics: metrics
            )
            
        } catch {
            // エラー時は空のアシスタントメッセージを削除
            await MainActor.run {
                self.messageService.removeLastMessageIfEmpty()
            }
            
            if let claudeError = error as? ClaudeIntegrationError {
                let logLevel: ConsoleLog.LogLevel = claudeError.severity == .critical || claudeError.severity == .error ? .error : .warning
                loggingService.log(logLevel, "APIエラー: \(claudeError.localizedDescription)")
                
                if let suggestion = claudeError.suggestedAction {
                    loggingService.info("対処法: \(suggestion)")
                }
                
                throw claudeError
            } else {
                loggingService.error("予期しないエラー: \(error.localizedDescription)")
                throw error
            }
        }
    }
}