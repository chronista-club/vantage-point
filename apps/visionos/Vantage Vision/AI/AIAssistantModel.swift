import SwiftUI
import Observation
import ClaudeIntegration

/// AIアシスタントの状態を管理するモデル
@Observable
class AIAssistantModel {
    /// チャットメッセージの履歴
    var messages: [ChatMessage] = []
    
    /// 現在の入力テキスト
    var inputText = ""
    
    /// ローディング状態
    var isLoading = false
    
    /// エラーメッセージ
    var errorMessage: String?
    
    /// AIアシスタントが表示されているか
    var isShowing = false
    
    /// APIキーが設定されているか
    var hasAPIKey = false
    
    /// 選択されたモデル
    var selectedModel = "claude-3-5-sonnet-20241022"
    
    /// ClaudeAPIクライアント
    private var claudeClient: ClaudeClient?
    
    /// Keychainマネージャー
    private let keychainManager = KeychainManager()
    
    /// 初期化
    init() {
        // ウェルカムメッセージを追加
        messages.append(ChatMessage(
            content: "こんにちは！Vantage AIアシスタントです。コーディングのお手伝いをさせていただきます。",
            isUser: false,
            timestamp: Date()
        ))
        
        // APIキーをロードして初期化
        Task { @MainActor in
            await initializeClient()
        }
    }
    
    /// ClaudeClientを初期化
    @MainActor
    private func initializeClient() async {
        do {
            let apiKey = try await keychainManager.loadAPIKey()
            claudeClient = ClaudeClient(apiKey: apiKey)
            hasAPIKey = true
        } catch {
            hasAPIKey = false
            print("APIキーの読み込みに失敗しました: \(error)")
        }
    }
    
    /// メッセージを送信
    func sendMessage(_ text: String) async {
        guard !text.isEmpty, let claudeClient = claudeClient else { return }
        
        // ユーザーメッセージを追加
        let userMessage = ChatMessage(
            content: text,
            isUser: true,
            timestamp: Date()
        )
        messages.append(userMessage)
        
        // 入力をクリア
        inputText = ""
        isLoading = true
        errorMessage = nil
        
        do {
            // メッセージ履歴をClaude API形式に変換
            let apiMessages = messages.compactMap { msg -> Message? in
                if msg.content == "こんにちは！Vantage AIアシスタントです。コーディングのお手伝いをさせていただきます。" {
                    return nil // ウェルカムメッセージは送信しない
                }
                return Message(
                    role: msg.isUser ? .user : .assistant,
                    content: msg.content
                )
            }
            
            // システムプロンプトを追加
            let systemMessage = Message(
                role: .system,
                content: "あなたはVantageアプリケーション開発を支援するAIアシスタントです。" +
                        "visionOS、Swift、Metal、RealityKitに精通しており、" +
                        "ユーザーのコーディングタスクをサポートします。"
            )
            
            let allMessages = [systemMessage] + apiMessages
            
            // ストリーミングレスポンスを使用
            let stream = try await claudeClient.streamMessage(
                allMessages,
                model: selectedModel,
                maxTokens: 1024
            )
            
            var responseContent = ""
            let assistantMessage = ChatMessage(
                content: "",
                isUser: false,
                timestamp: Date()
            )
            messages.append(assistantMessage)
            let lastIndex = messages.count - 1
            
            // ストリーミングレスポンスを処理
            for try await event in stream {
                switch event {
                case .chunk(let chunk):
                    responseContent += chunk
                    // メッセージを更新
                    if lastIndex < messages.count {
                        messages[lastIndex] = ChatMessage(
                            content: responseContent,
                            isUser: false,
                            timestamp: assistantMessage.timestamp
                        )
                    }
                case .done:
                    break
                case .error(let error):
                    throw error
                }
            }
        } catch {
            errorMessage = "エラーが発生しました: \(error.localizedDescription)"
            // エラーメッセージを表示
            if let lastMessage = messages.last, !lastMessage.isUser {
                messages[messages.count - 1] = ChatMessage(
                    content: "申し訳ございません。エラーが発生しました: \(error.localizedDescription)",
                    isUser: false,
                    timestamp: Date()
                )
            }
        }
        
        isLoading = false
    }
    
    /// APIキーを設定
    func setAPIKey(_ apiKey: String) async {
        do {
            try await keychainManager.saveAPIKey(apiKey)
            claudeClient = ClaudeClient(apiKey: apiKey)
            hasAPIKey = true
            errorMessage = nil
        } catch {
            errorMessage = "APIキーの保存に失敗しました: \(error.localizedDescription)"
        }
    }
    
    /// チャット履歴をクリア
    func clearMessages() {
        messages = [ChatMessage(
            content: "こんにちは！Vantage AIアシスタントです。コーディングのお手伝いをさせていただきます。",
            isUser: false,
            timestamp: Date()
        )]
        errorMessage = nil
    }
}

/// チャットメッセージ
struct ChatMessage: Identifiable {
    let id = UUID()
    let content: String
    let isUser: Bool
    let timestamp: Date
}