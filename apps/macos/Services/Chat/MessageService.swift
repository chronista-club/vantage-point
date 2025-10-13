import Foundation
import ClaudeIntegration

// MARK: - Protocol

protocol MessageServiceProtocol {
    var messages: [ChatMessage] { get }
    var messageHistory: [String] { get }
    
    func addUserMessage(_ content: String) -> ChatMessage
    func addAssistantMessage() -> ChatMessage
    func updateMessage(id: UUID, content: String)
    func removeMessage(id: UUID)
    func clearMessages()
    func getAPIMessages() -> [Message]
    func addToHistory(_ message: String)
}

// MARK: - Implementation

@MainActor
final class MessageService: ObservableObject, MessageServiceProtocol {
    @Published private(set) var messages: [ChatMessage] = []
    @Published private(set) var messageHistory: [String] = []
    
    private let maxHistoryCount = 50
    
    init() {}
    
    func addUserMessage(_ content: String) -> ChatMessage {
        let message = ChatMessage(content: content, isUser: true)
        messages.append(message)
        addToHistory(content)
        return message
    }
    
    func addAssistantMessage() -> ChatMessage {
        let message = ChatMessage(content: "", isUser: false)
        messages.append(message)
        return message
    }
    
    func updateMessage(id: UUID, content: String) {
        if let index = messages.firstIndex(where: { $0.id == id }) {
            messages[index].content = content
        }
    }
    
    func updateMessageTimestamp(id: UUID) {
        if let index = messages.firstIndex(where: { $0.id == id }) {
            messages[index].timestamp = Date()
        }
    }
    
    func appendToMessage(id: UUID, content: String) {
        if let index = messages.firstIndex(where: { $0.id == id }) {
            messages[index].content += content
        }
    }
    
    func removeMessage(id: UUID) {
        messages.removeAll { $0.id == id }
    }
    
    func removeLastMessageIfEmpty() {
        if let lastMessage = messages.last,
           !lastMessage.isUser && lastMessage.content.isEmpty {
            messages.removeLast()
        }
    }
    
    func clearMessages() {
        messages.removeAll()
        // ウェルカムメッセージを追加
        let welcomeMessage = ChatMessage(
            content: "こんにちは！Vantage for Macへようこそ。何かお手伝いできることはありますか？",
            isUser: false,
            timestamp: Date()
        )
        messages.append(welcomeMessage)
    }
    
    func getAPIMessages() -> [Message] {
        messages.compactMap { msg -> Message? in
            if msg.isUser {
                return Message(role: .user, content: msg.content)
            } else if messages.firstIndex(where: { $0.id == msg.id }) != 0 {
                // 最初のウェルカムメッセージは除外
                return Message(role: .assistant, content: msg.content)
            }
            return nil
        }
    }
    
    func addToHistory(_ message: String) {
        // 重複を避ける
        if let lastMessage = messageHistory.last, lastMessage == message {
            return
        }
        
        messageHistory.append(message)
        
        // 履歴の最大数を制限
        if messageHistory.count > maxHistoryCount {
            messageHistory.removeFirst(messageHistory.count - maxHistoryCount)
        }
    }
    
    func loadMessages(_ messages: [ChatMessage]) {
        self.messages = messages
    }
    
    var lastUserMessage: String? {
        messages.last(where: { $0.isUser })?.content
    }
    
    func getMessageIndex(by id: UUID) -> Int? {
        messages.firstIndex(where: { $0.id == id })
    }
}