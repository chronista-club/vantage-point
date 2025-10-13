import Foundation
import SwiftUI
import ClaudeIntegration
import Combine

// MARK: - View Model Protocol

@MainActor
protocol ChatViewModelProtocol: ObservableObject {
    var messages: [ChatMessage] { get }
    var isLoading: Bool { get }
    var errorMessage: String? { get }
    var selectedModel: ClaudeModel { get set }
    var hasAPIKey: Bool { get }
    var streamingMessageId: UUID? { get }
    var estimatedInputTokens: Int { get }
    var estimatedOutputTokens: Int { get }
    
    func sendMessage(_ text: String) async
    func setAPIKey(_ key: String) async
    func clearMessages()
    func cancelStreaming()
    func retryLastMessage() async
}

// MARK: - Refactored Implementation

@MainActor
final class ChatViewModel: ObservableObject {
    // MARK: - Published Properties
    
    @Published var isLoading = false
    @Published var errorMessage: String?
    @Published var selectedModel: ClaudeModel = .claude35Sonnet
    @Published var hasAPIKey = false
    @Published var streamingMessageId: UUID?
    @Published var estimatedInputTokens = 0
    @Published var estimatedOutputTokens = 0
    @Published var workingDirectory: URL?
    @Published var workingDirectoryManager = WorkingDirectoryManager()
    @Published var lastError: ClaudeIntegrationError?
    
    // MARK: - Services
    
    let apiService: ClaudeAPIService
    let messageService: MessageService
    let sessionService: SessionService
    let loggingService: LoggingService
    private let sendMessageUseCase: SendMessageUseCase

    // MARK: - Private Properties

    private var cancellables = Set<AnyCancellable>()
    private var _currentStreamTask: Task<Void, Error>?

    var currentStreamTask: Task<Void, Error>? {
        get { _currentStreamTask }
        set { _currentStreamTask = newValue }
    }
    
    // MARK: - Computed Properties
    
    var messages: [ChatMessage] {
        messageService.messages
    }
    
    var consoleLogs: [ConsoleLog] {
        loggingService.logs
    }
    
    var sessions: [ChatSession] {
        sessionService.sessions
    }
    
    var currentSession: ChatSession? {
        sessionService.currentSession
    }
    
    // MARK: - Initialization
    
    init(
        apiService: ClaudeAPIService? = nil,
        messageService: MessageService? = nil,
        sessionService: SessionService? = nil,
        loggingService: LoggingService? = nil
    ) {
        // Initialize services
        self.apiService = apiService ?? ClaudeAPIService()
        self.messageService = messageService ?? MessageService()
        self.sessionService = sessionService ?? SessionService()
        self.loggingService = loggingService ?? LoggingService()
        
        // Initialize use case
        self.sendMessageUseCase = SendMessageUseCase(
            apiService: self.apiService,
            messageService: self.messageService,
            loggingService: self.loggingService,
            retryManager: RetryManager()
        )
        
        // Setup logging bridge
        let loggingBridge = self.loggingService.createAPILoggingBridge()

        // Load saved API key
        Task {
            await self.apiService.setLoggingDelegate(loggingBridge)
            await checkSavedAPIKey()
        }
        
        // Setup bindings
        setupBindings()
        
        // Load current session
        if let currentSession = self.sessionService.currentSession {
            self.messageService.loadMessages(currentSession.messages)
            selectedModel = ClaudeModel.allCases.first { $0.rawValue == currentSession.model } ?? .claude35Sonnet
            self.loggingService.info("セッション「\(currentSession.title)」を読み込みました")
        }

        self.loggingService.info("Vantage for Mac が起動しました")
    }
    
    // MARK: - Setup
    
    private func setupBindings() {
        // Message変更を監視してセッションを更新
        messageService.$messages
            .dropFirst()
            .debounce(for: .seconds(0.5), scheduler: RunLoop.main)
            .sink { [weak self] _ in
                self?.updateCurrentSession()
            }
            .store(in: &cancellables)
        
        // モデル変更も監視
        $selectedModel
            .dropFirst()
            .sink { [weak self] _ in
                self?.updateCurrentSession()
            }
            .store(in: &cancellables)
    }
    
    private func checkSavedAPIKey() async {
        do {
            try await apiService.loadSavedAPIKey()
            hasAPIKey = true
            loggingService.info("保存されたAPIキーを読み込みました")
        } catch {
            loggingService.debug("保存されたAPIキーが見つかりません")
        }
    }
    
    // MARK: - Public Methods
    
    func setAPIKey(_ key: String) async {
        guard !key.isEmpty else { return }
        
        loggingService.info("APIキーを設定しています...")
        
        do {
            try await apiService.setAPIKey(key)
            hasAPIKey = true
            errorMessage = nil
            loggingService.info("APIキーが正常に設定されました")
        } catch {
            errorMessage = "APIキーの保存に失敗しました: \(error.localizedDescription)"
            loggingService.error("APIキーの保存に失敗: \(error.localizedDescription)")
        }
    }
    
    func sendMessage(_ text: String) async {
        guard hasAPIKey else {
            let error = ClaudeIntegrationError.missingAPIKey
            errorMessage = error.userFriendlyMessage
            lastError = error
            loggingService.error("APIキーが設定されていません")
            return
        }
        
        
        // 前のストリーミングをキャンセル
        currentStreamTask?.cancel()
        
        isLoading = true
        errorMessage = nil
        lastError = nil
        
        // システムプロンプトに作業ディレクトリ情報を追加
        var systemPrompt = "あなたは親切で役立つAIアシスタントです。日本語で応答してください。"
        if let workingDir = workingDirectory {
            systemPrompt += "\n\n現在の作業ディレクトリ: \(workingDir.path)"
        }
        
        // ストリーミング処理
        currentStreamTask = Task {
            do {
                let result = try await sendMessageUseCase.execute(
                    text: text,
                    model: selectedModel,
                    systemPrompt: systemPrompt,
                    onStreamUpdate: { @Sendable [weak self] _ in
                        // UIの更新は自動的にMessageServiceが処理
                        Task { @MainActor [weak self] in
                            self?.streamingMessageId = self?.messages.last?.id
                        }
                    }
                )
                
                streamingMessageId = nil
                estimatedInputTokens = result.tokenMetrics.inputTokens
                estimatedOutputTokens = result.tokenMetrics.outputTokens
                
            } catch {
                if let claudeError = error as? ClaudeIntegrationError {
                    errorMessage = claudeError.userFriendlyMessage
                    lastError = claudeError
                } else {
                    errorMessage = "予期しないエラーが発生しました: \(error.localizedDescription)"
                }
            }
            
            isLoading = false
            currentStreamTask = nil
        }
        
        try? await currentStreamTask?.value
    }
    
    func clearMessages() {
        let newSession = sessionService.createNewSession()
        messageService.loadMessages(newSession.messages)
        errorMessage = nil
        estimatedInputTokens = 0
        estimatedOutputTokens = 0
        loggingService.info("新しいセッションを開始しました")
    }
    
    func cancelStreaming() {
        currentStreamTask?.cancel()
        currentStreamTask = nil
        streamingMessageId = nil
        isLoading = false
        
        // 最後のメッセージが空なら削除
        messageService.removeLastMessageIfEmpty()
        
        if let lastMessage = messages.last, !lastMessage.isUser {
            loggingService.info("ストリーミングを中断しました（\(lastMessage.content.count)文字受信済み）")
        } else {
            loggingService.info("ストリーミングを中断しました")
        }
    }
    
    func retryLastMessage() async {
        guard let lastMessage = messageService.lastUserMessage else { return }
        
        lastError = nil
        errorMessage = nil
        
        await sendMessage(lastMessage)
    }
    
    // MARK: - Session Management
    
    func switchToSession(_ sessionId: UUID) {
        sessionService.switchToSession(sessionId)
        if let session = sessionService.currentSession {
            messageService.loadMessages(session.messages)
            selectedModel = ClaudeModel.allCases.first { $0.rawValue == session.model } ?? .claude35Sonnet
        }
    }
    
    func deleteSession(_ sessionId: UUID) {
        sessionService.deleteSession(sessionId)
        if let currentSession = sessionService.currentSession {
            messageService.loadMessages(currentSession.messages)
        }
    }
    
    func exportSession(_ sessionId: UUID) -> String? {
        sessionService.exportSession(sessionId)
    }
    
    // MARK: - Console Management
    
    func clearLogs() {
        loggingService.clearLogs()
    }
    
    // MARK: - Working Directory Management
    
    func setWorkingDirectory(_ url: URL) {
        // WorkingDirectoryManagerを使用してディレクトリを設定
        if workingDirectoryManager.setDirectory(url) {
            workingDirectory = url
            loggingService.info("作業ディレクトリを設定: \(url.path)")
        } else {
            loggingService.error("ディレクトリへのアクセス権限を取得できませんでした")
        }
    }
    
    // 保存されたディレクトリから復元
    func restoreWorkingDirectory(from bookmark: BookmarkedDirectory) {
        if let url = workingDirectoryManager.restoreDirectory(from: bookmark) {
            workingDirectory = url
            loggingService.info("作業ディレクトリを復元: \(url.path)")
        } else {
            loggingService.error("ディレクトリの復元に失敗しました")
        }
    }
    
    // ファイルの内容を読み込んでメッセージとして送信
    func sendFileContent(path: String, fileName: String) async {
        do {
            let content = try String(contentsOfFile: path, encoding: .utf8)
            let message = """
            ファイル: \(fileName)
            パス: \(path)
            
            ```
            \(content)
            ```
            """
            
            await sendMessage(message)
        } catch {
            loggingService.error("ファイルの読み込みに失敗: \(error.localizedDescription)")
        }
    }
    
    // MARK: - Private Methods
    
    private func updateCurrentSession() {
        sessionService.updateCurrentSession(
            messages: messages,
            model: selectedModel.rawValue
        )
    }
}

// MARK: - ChatViewModelProtocol Conformance

extension ChatViewModel: ChatViewModelProtocol {}