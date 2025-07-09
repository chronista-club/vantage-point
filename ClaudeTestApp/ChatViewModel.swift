import Foundation
import SwiftUI
import ClaudeAPI

@MainActor
class ChatViewModel: ObservableObject {
    @Published var messages: [ChatMessage] = []
    @Published var isLoading = false
    @Published var errorMessage: String?
    @Published var selectedModel: ClaudeModel = .claude35Sonnet
    @Published var hasAPIKey = false
    
    private var client: ClaudeClient?
    private let keychainManager = KeychainManager.shared
    
    init() {
        // 初期メッセージ
        messages.append(ChatMessage(
            content: "こんにちは！Claude APIテストクライアントへようこそ。何か質問がありますか？",
            isUser: false
        ))
        
        // 保存されたAPIキーをチェック
        Task { @MainActor in
            await checkSavedAPIKey()
        }
    }
    
    private func checkSavedAPIKey() async {
        do {
            let apiKey = try await keychainManager.loadAPIKey()
            client = ClaudeClient(apiKey: apiKey)
            hasAPIKey = true
        } catch {
            // キーが見つからない場合は無視
        }
    }
    
    func setAPIKey(_ key: String) {
        guard !key.isEmpty else { return }
        
        Task { @MainActor in
            do {
                // Keychainに保存
                try await keychainManager.saveAPIKey(key)
                client = ClaudeClient(apiKey: key)
                hasAPIKey = true
                errorMessage = nil
            } catch {
                errorMessage = "APIキーの保存に失敗しました: \(error.localizedDescription)"
            }
        }
    }
    
    func sendMessage(_ text: String) async {
        guard let client = client else {
            errorMessage = "APIキーが設定されていません"
            return
        }
        
        // ユーザーメッセージを追加
        let userMessage = ChatMessage(content: text, isUser: true)
        messages.append(userMessage)
        
        isLoading = true
        errorMessage = nil
        
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
            
            // ストリーミングレスポンスを取得
            let stream = try await client.streamMessage(
                apiMessages,
                model: selectedModel,
                system: "あなたは親切で役立つAIアシスタントです。日本語で応答してください。"
            )
            
            // アシスタントメッセージを作成
            var assistantMessage = ChatMessage(content: "", isUser: false)
            messages.append(assistantMessage)
            let messageIndex = messages.count - 1
            
            // ストリームを処理
            for try await event in stream {
                switch event {
                case .contentBlockDelta(let delta):
                    if let text = delta.delta.text {
                        messages[messageIndex].content += text
                    }
                case .messageStop:
                    messages[messageIndex].timestamp = Date()
                case .error(let error):
                    errorMessage = error.message
                default:
                    break
                }
            }
            
        } catch ClaudeAPIError.rateLimited(let retryAfter) {
            errorMessage = "レート制限に達しました"
            if let retryAfter = retryAfter {
                errorMessage! += "（\(Int(retryAfter))秒後に再試行）"
            }
            // エラーメッセージを削除
            if messages.last?.isUser == false && messages.last?.content.isEmpty == true {
                messages.removeLast()
            }
        } catch {
            errorMessage = error.localizedDescription
            // エラーメッセージを削除
            if messages.last?.isUser == false && messages.last?.content.isEmpty == true {
                messages.removeLast()
            }
        }
        
        isLoading = false
    }
    
    func clearMessages() {
        messages = [ChatMessage(
            content: "こんにちは！Claude APIテストクライアントへようこそ。何か質問がありますか？",
            isUser: false
        )]
        errorMessage = nil
    }
}

struct ChatMessage: Identifiable {
    let id = UUID()
    var content: String
    let isUser: Bool
    var timestamp: Date?
    
    init(content: String, isUser: Bool, timestamp: Date? = nil) {
        self.content = content
        self.isUser = isUser
        self.timestamp = timestamp
    }
}