import Foundation

/// チャットセッションを表すモデル
struct ChatSession: Identifiable, Codable {
    let id: UUID
    var title: String
    var messages: [ChatMessage]
    let createdAt: Date
    var updatedAt: Date
    var model: String
    
    init(id: UUID = UUID(), title: String = "新しいチャット", messages: [ChatMessage] = [], createdAt: Date = Date(), updatedAt: Date = Date(), model: String = "claude-3-5-sonnet") {
        self.id = id
        self.title = title
        self.messages = messages
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.model = model
    }
    
    /// 最初のユーザーメッセージから自動的にタイトルを生成
    mutating func generateTitle() {
        if let firstUserMessage = messages.first(where: { $0.isUser }) {
            let content = firstUserMessage.content
            // 最大50文字に制限
            if content.count > 50 {
                self.title = String(content.prefix(47)) + "..."
            } else {
                self.title = content
            }
        }
    }
}

/// セッション管理クラス
@MainActor
class SessionManager: ObservableObject {
    @Published var sessions: [ChatSession] = []
    @Published var currentSessionId: UUID?
    
    private let userDefaults = UserDefaults.standard
    private let sessionsKey = "VantageChatSessions"
    
    var currentSession: ChatSession? {
        get {
            sessions.first { $0.id == currentSessionId }
        }
        set {
            if let newValue = newValue,
               let index = sessions.firstIndex(where: { $0.id == newValue.id }) {
                sessions[index] = newValue
                saveSessions()
            }
        }
    }
    
    init() {
        loadSessions()
    }
    
    /// セッションを読み込む
    func loadSessions() {
        guard let data = userDefaults.data(forKey: sessionsKey),
              let decodedSessions = try? JSONDecoder().decode([ChatSession].self, from: data) else {
            // デフォルトセッションを作成
            createNewSession()
            return
        }
        
        sessions = decodedSessions
        
        // 最新のセッションを選択
        if let latestSession = sessions.max(by: { $0.updatedAt < $1.updatedAt }) {
            currentSessionId = latestSession.id
        } else if !sessions.isEmpty {
            currentSessionId = sessions[0].id
        }
    }
    
    /// セッションを保存する
    func saveSessions() {
        guard let data = try? JSONEncoder().encode(sessions) else { return }
        userDefaults.set(data, forKey: sessionsKey)
    }
    
    /// 新しいセッションを作成
    func createNewSession() {
        let welcomeMessage = ChatMessage(
            content: "こんにちは！Claude APIテストクライアントへようこそ。何か質問がありますか？",
            isUser: false
        )
        let newSession = ChatSession(messages: [welcomeMessage])
        sessions.insert(newSession, at: 0)
        currentSessionId = newSession.id
        saveSessions()
    }
    
    /// セッションを削除
    func deleteSession(_ sessionId: UUID) {
        sessions.removeAll { $0.id == sessionId }
        
        if currentSessionId == sessionId {
            if let firstSession = sessions.first {
                currentSessionId = firstSession.id
            } else {
                createNewSession()
            }
        }
        
        saveSessions()
    }
    
    /// セッションをエクスポート
    func exportSession(_ sessionId: UUID) -> String? {
        guard let session = sessions.first(where: { $0.id == sessionId }) else { return nil }
        
        var markdown = "# \(session.title)\n\n"
        markdown += "作成日時: \(formatDate(session.createdAt))\n"
        markdown += "更新日時: \(formatDate(session.updatedAt))\n"
        markdown += "モデル: \(session.model)\n\n"
        markdown += "---\n\n"
        
        for message in session.messages {
            if message.isUser {
                markdown += "## You\n\n"
            } else {
                markdown += "## Claude\n\n"
            }
            markdown += "\(message.content)\n\n"
            if let timestamp = message.timestamp {
                markdown += "_\(formatDate(timestamp))_\n\n"
            }
            markdown += "---\n\n"
        }
        
        return markdown
    }
    
    /// セッションを切り替え
    func switchToSession(_ sessionId: UUID) {
        if sessions.contains(where: { $0.id == sessionId }) {
            currentSessionId = sessionId
        }
    }
    
    /// 現在のセッションを更新
    func updateCurrentSession(messages: [ChatMessage], model: String) {
        guard let currentId = currentSessionId,
              let index = sessions.firstIndex(where: { $0.id == currentId }) else { return }
        
        sessions[index].messages = messages
        sessions[index].model = model
        sessions[index].updatedAt = Date()
        
        // タイトルが未設定なら自動生成
        if sessions[index].title == "新しいチャット" {
            sessions[index].generateTitle()
        }
        
        saveSessions()
    }
    
    private func formatDate(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        formatter.locale = Locale(identifier: "ja_JP")
        return formatter.string(from: date)
    }
}