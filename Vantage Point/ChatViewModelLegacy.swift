import Foundation
import SwiftUI
import ClaudeIntegration
import Combine

@MainActor
class ChatViewModel: ObservableObject {
    @Published var messages: [ChatMessage] = []
    @Published var isLoading = false
    @Published var errorMessage: String?
    @Published var selectedModel: ClaudeModel = .claude35Sonnet
    @Published var hasAPIKey = false
    @Published var consoleLogs: [ConsoleLog] = []
    @Published var lastError: ClaudeIntegrationError?
    @Published var retryManager = RetryManager()
    @Published var messageHistory: [String] = []
    @Published var streamingMessageId: UUID?
    @Published var currentStreamTask: Task<Void, Error>?
    @Published var estimatedInputTokens = 0
    @Published var estimatedOutputTokens = 0
    @Published var sessionManager = SessionManager()
    
    private var client: ClaudeClient?
    private let keychainManager = KeychainManager.shared
    private var loggingBridge: APILoggingBridge?
    private var lastUserMessage: String?
    private let maxMessageHistory = 50
    
    init() {
        // ConsoleLogのsharedViewModelを設定
        ConsoleLog.sharedViewModel = self
        
        // ログブリッジを設定
        self.loggingBridge = APILoggingBridge(viewModel: self)
        
        // セッションから現在のメッセージを読み込む
        if let currentSession = sessionManager.currentSession {
            self.messages = currentSession.messages
            self.selectedModel = ClaudeModel.allCases.first { $0.rawValue == currentSession.model } ?? .claude35Sonnet
            addLog(level: .info, message: "セッション「\(currentSession.title)」を読み込みました")
        }
        
        addLog(level: .info, message: "Vantage for Mac が起動しました")
        
        // 保存されたAPIキーをチェック
        Task { @MainActor in
            await checkSavedAPIKey()
        }
        
        // メッセージの変更を監視してセッションを更新
        $messages
            .dropFirst()
            .debounce(for: .seconds(0.5), scheduler: RunLoop.main)
            .sink { [weak self] _ in
                self?.updateCurrentSession()
            }
            .store(in: &cancellables)
        
        // モデルの変更も監視
        $selectedModel
            .dropFirst()
            .sink { [weak self] _ in
                self?.updateCurrentSession()
            }
            .store(in: &cancellables)
    }
    
    private var cancellables = Set<AnyCancellable>()
    
    private func checkSavedAPIKey() async {
        do {
            let apiKey = try await keychainManager.loadAPIKey()
            client = ClaudeClient(apiKey: apiKey)
            await client?.setLoggingDelegate(loggingBridge)
            hasAPIKey = true
            addLog(level: .info, message: "保存されたAPIキーを読み込みました")
        } catch {
            // キーが見つからない場合は無視
            addLog(level: .debug, message: "保存されたAPIキーが見つかりません")
        }
    }
    
    func setAPIKey(_ key: String) {
        guard !key.isEmpty else { return }
        
        addLog(level: .info, message: "APIキーを設定しています...")
        
        Task { @MainActor in
            do {
                // Keychainに保存
                try await keychainManager.saveAPIKey(key)
                client = ClaudeClient(apiKey: key)
                await client?.setLoggingDelegate(loggingBridge)
                hasAPIKey = true
                errorMessage = nil
                addLog(level: .info, message: "APIキーが正常に設定されました")
            } catch {
                errorMessage = "APIキーの保存に失敗しました: \(error.localizedDescription)"
                addLog(level: .error, message: "APIキーの保存に失敗: \(error.localizedDescription)")
            }
        }
    }
    
    func sendMessage(_ text: String) async {
        guard let client = client else {
            let error = ClaudeIntegrationError.missingAPIKey
            errorMessage = error.userFriendlyMessage
            lastError = error
            addLog(level: .error, message: "APIキーが設定されていません")
            return
        }
        
        // 前のストリーミングをキャンセル
        currentStreamTask?.cancel()
        
        // ユーザーメッセージを追加
        let userMessage = ChatMessage(content: text, isUser: true)
        messages.append(userMessage)
        lastUserMessage = text
        addToMessageHistory(text)
        addLog(level: .info, message: "メッセージを送信: \(text.prefix(50))\(text.count > 50 ? "..." : "")")
        
        isLoading = true
        errorMessage = nil
        lastError = nil
        
        do {
            // Claude APIメッセージを構築
            let apiMessages = messages.compactMap { msg -> Message? in
                if msg.isUser {
                    return Message(role: .user, content: msg.content)
                } else if messages.firstIndex(where: { $0.id == msg.id }) != 0 {
                    // 最初のウェルカムメッセージは除外
                    return Message(role: .assistant, content: msg.content)
                }
                return nil
            }
            
            // 入力トークンの推定（簡易計算：4文字≒1トークン）
            let inputText = apiMessages.map { $0.content }.joined(separator: " ")
            estimatedInputTokens = inputText.count / 4
            
            // ストリーミングレスポンスを取得
            addLog(level: .info, message: "Claude API (\(selectedModel.displayName)) にリクエストを送信中...")
            let stream = try await client.streamMessage(
                apiMessages,
                model: selectedModel,
                system: "あなたは親切で役立つAIアシスタントです。日本語で応答してください。"
            )
            
            // アシスタントメッセージを作成
            var assistantMessage = ChatMessage(content: "", isUser: false)
            messages.append(assistantMessage)
            let messageIndex = messages.count - 1
            let messageId = messages[messageIndex].id
            streamingMessageId = messageId
            addLog(level: .debug, message: "レスポンスのストリーミングを開始")
            
            // ストリームを処理するタスクを作成
            currentStreamTask = Task {
                var tokenCount = 0
                let startTime = Date()
                
                for try await event in stream {
                    // タスクがキャンセルされたかチェック
                    if Task.isCancelled { break }
                    
                    switch event {
                    case .contentBlockDelta(let delta):
                        if let text = delta.delta.text {
                            messages[messageIndex].content += text
                            tokenCount += text.count
                            estimatedOutputTokens = tokenCount / 4
                        }
                    case .messageStop:
                        messages[messageIndex].timestamp = Date()
                        let duration = Date().timeIntervalSince(startTime)
                        let tokensPerSecond = Double(tokenCount) / max(duration, 0.1)
                        let totalTokens = estimatedInputTokens + estimatedOutputTokens
                        addLog(level: .info, message: "レスポンスを受信完了 (約\(tokenCount)文字, \(String(format: "%.1f", tokensPerSecond))文字/秒, 推定\(totalTokens)トークン)")
                        streamingMessageId = nil
                        currentStreamTask = nil
                    case .error(let error):
                        errorMessage = error.message
                        lastError = ClaudeIntegrationError.serverError(error.message)
                        addLog(level: .error, message: "ストリーミングエラー: \(error.message)")
                        streamingMessageId = nil
                        currentStreamTask = nil
                        // 空のメッセージを削除
                        if messages[messageIndex].content.isEmpty {
                            messages.remove(at: messageIndex)
                        }
                    default:
                        break
                    }
                }
            }
            
            try? await currentStreamTask?.value
            
        } catch let error as ClaudeIntegrationError {
            // ClaudeIntegrationErrorの場合
            errorMessage = error.userFriendlyMessage
            lastError = error
            
            // ログレベルを決定
            let logLevel: ConsoleLog.LogLevel
            switch error.severity {
            case .critical:
                logLevel = .error
            case .error:
                logLevel = .error
            case .warning:
                logLevel = .warning
            case .temporary:
                logLevel = .info
            }
            
            addLog(level: logLevel, message: "APIエラー: \(error.localizedDescription)")
            
            // 対処法があれば追加でログ
            if let suggestion = error.suggestedAction {
                addLog(level: .info, message: "対処法: \(suggestion)")
            }
            
            // 空のアシスタントメッセージを削除
            if messages.last?.isUser == false && messages.last?.content.isEmpty == true {
                messages.removeLast()
            }
        } catch {
            // その他のエラー
            errorMessage = "予期しないエラーが発生しました: \(error.localizedDescription)"
            addLog(level: .error, message: "予期しないエラー: \(error.localizedDescription)")
            
            // 空のアシスタントメッセージを削除
            if messages.last?.isUser == false && messages.last?.content.isEmpty == true {
                messages.removeLast()
            }
        }
        
        isLoading = false
    }
    
    func clearMessages() {
        sessionManager.createNewSession()
        if let currentSession = sessionManager.currentSession {
            messages = currentSession.messages
        }
        errorMessage = nil
        estimatedInputTokens = 0
        estimatedOutputTokens = 0
        addLog(level: .info, message: "新しいセッションを開始しました")
    }
    
    private func updateCurrentSession() {
        sessionManager.updateCurrentSession(messages: messages, model: selectedModel.rawValue)
    }
    
    // ログ機能
    func addLog(level: ConsoleLog.LogLevel, message: String) {
        let log = ConsoleLog(
            timestamp: Date(),
            level: level,
            message: message
        )
        consoleLogs.append(log)
    }
    
    func clearLogs() {
        consoleLogs.removeAll()
        addLog(level: .info, message: "ログをクリアしました")
    }
    
    // リトライ機能
    func retryLastMessage() async {
        guard let lastMessage = lastUserMessage else { return }
        
        // 最後のエラーをクリア
        lastError = nil
        errorMessage = nil
        
        // リトライマネージャーを使用して再送信
        do {
            try await retryManager.performWithRetry(
                operation: { [weak self] in
                    guard let self = self else { return }
                    try await self.sendMessageWithoutRetry(lastMessage)
                },
                onError: { [weak self] error, retryCount in
                    Task { @MainActor in
                        self?.addLog(level: .info, message: "リトライ \(retryCount)回目: \(error.localizedDescription)")
                    }
                }
            )
        } catch {
            // リトライも失敗
            addLog(level: .error, message: "リトライが全て失敗しました")
        }
    }
    
    // リトライなしでメッセージ送信（内部用）
    private func sendMessageWithoutRetry(_ text: String) async throws {
        guard let client = client else {
            throw ClaudeIntegrationError.missingAPIKey
        }
        
        isLoading = true
        errorMessage = nil
        lastError = nil
        
        // Claude APIメッセージを構築
        let apiMessages = messages.compactMap { msg -> Message? in
            if msg.isUser {
                return Message(role: .user, content: msg.content)
            } else if messages.firstIndex(where: { $0.id == msg.id }) != 0 {
                // 最初のウェルカムメッセージは除外
                return Message(role: .assistant, content: msg.content)
            }
            return nil
        }
        
        // ストリーミングレスポンスを取得
        addLog(level: .info, message: "Claude API (\(selectedModel.displayName)) にリクエストを送信中...")
        let stream = try await client.streamMessage(
            apiMessages,
            model: selectedModel,
            system: "あなたは親切で役立つAIアシスタントです。日本語で応答してください。"
        )
        
        // アシスタントメッセージを作成
        let assistantMessage = ChatMessage(content: "", isUser: false)
        messages.append(assistantMessage)
        let messageIndex = messages.count - 1
        let messageId = messages[messageIndex].id
        streamingMessageId = messageId
        addLog(level: .debug, message: "レスポンスのストリーミングを開始")
        
        // ストリームを処理
        var tokenCount = 0
        let startTime = Date()
        
        for try await event in stream {
            switch event {
            case .contentBlockDelta(let delta):
                if let text = delta.delta.text {
                    messages[messageIndex].content += text
                    tokenCount += text.count // 簡易的なトークンカウント
                    estimatedOutputTokens = tokenCount / 4 // 4文字≒1トークン
                }
            case .messageStop:
                messages[messageIndex].timestamp = Date()
                let duration = Date().timeIntervalSince(startTime)
                let tokensPerSecond = Double(tokenCount) / max(duration, 0.1)
                let totalTokens = estimatedInputTokens + estimatedOutputTokens
                addLog(level: .info, message: "レスポンスを受信完了 (約\(tokenCount)文字, \(String(format: "%.1f", tokensPerSecond))文字/秒, 推定\(totalTokens)トークン)")
                streamingMessageId = nil
                currentStreamTask = nil
            case .error(let error):
                errorMessage = error.message
                addLog(level: .error, message: "ストリーミングエラー: \(error.message)")
                streamingMessageId = nil
                throw ClaudeIntegrationError.serverError(error.message)
            default:
                break
            }
        }
        
        isLoading = false
    }
    
    // メッセージ履歴管理
    func addToMessageHistory(_ message: String) {
        // 重複を避ける
        if let lastMessage = messageHistory.last, lastMessage == message {
            return
        }
        
        messageHistory.append(message)
        
        // 履歴の最大数を制限
        if messageHistory.count > maxMessageHistory {
            messageHistory.removeFirst(messageHistory.count - maxMessageHistory)
        }
    }
    
    // ストリーミングの中断
    func cancelStreaming() {
        currentStreamTask?.cancel()
        currentStreamTask = nil
        streamingMessageId = nil
        isLoading = false
        
        // 最後のメッセージが空なら削除
        if let lastMessage = messages.last, !lastMessage.isUser && lastMessage.content.isEmpty {
            messages.removeLast()
            addLog(level: .info, message: "ストリーミングを中断しました（空のメッセージを削除）")
        } else if let lastMessage = messages.last, !lastMessage.isUser {
            // タイムスタンプを設定
            messages[messages.count - 1].timestamp = Date()
            addLog(level: .info, message: "ストリーミングを中断しました（\(lastMessage.content.count)文字受信済み）")
        } else {
            addLog(level: .info, message: "ストリーミングを中断しました")
        }
    }
}

struct ChatMessage: Identifiable, Codable {
    let id: UUID
    var content: String
    let isUser: Bool
    var timestamp: Date?
    
    init(id: UUID = UUID(), content: String, isUser: Bool, timestamp: Date? = nil) {
        self.id = id
        self.content = content
        self.isUser = isUser
        self.timestamp = timestamp
    }
}