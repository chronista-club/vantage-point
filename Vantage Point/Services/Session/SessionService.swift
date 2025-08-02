import Foundation

// MARK: - Protocol

protocol SessionServiceProtocol {
    var sessions: [ChatSession] { get }
    var currentSessionId: UUID? { get }
    var currentSession: ChatSession? { get }
    
    func createNewSession() -> ChatSession
    func deleteSession(_ sessionId: UUID)
    func switchToSession(_ sessionId: UUID)
    func updateSession(_ session: ChatSession)
    func exportSession(_ sessionId: UUID) -> String?
}

// MARK: - Implementation

@MainActor
final class SessionService: ObservableObject, SessionServiceProtocol {
    @Published private(set) var sessions: [ChatSession] = []
    @Published var currentSessionId: UUID?
    
    private let repository: SessionRepositoryProtocol
    
    var currentSession: ChatSession? {
        sessions.first { $0.id == currentSessionId }
    }
    
    init(repository: SessionRepositoryProtocol = SessionRepository()) {
        self.repository = repository
        loadSessions()
    }
    
    private func loadSessions() {
        sessions = repository.loadSessions()
        
        // 最新のセッションを選択
        if let latestSession = sessions.max(by: { $0.updatedAt < $1.updatedAt }) {
            currentSessionId = latestSession.id
        } else if sessions.isEmpty {
            // セッションがない場合は新規作成
            createNewSession()
        }
    }
    
    @discardableResult
    func createNewSession() -> ChatSession {
        let welcomeMessage = ChatMessage(
            content: "こんにちは！Vantage for Macへようこそ。何かお手伝いできることはありますか？",
            isUser: false,
            timestamp: Date()
        )
        let newSession = ChatSession(messages: [welcomeMessage])
        sessions.insert(newSession, at: 0)
        currentSessionId = newSession.id
        repository.saveSessions(sessions)
        return newSession
    }
    
    func deleteSession(_ sessionId: UUID) {
        sessions.removeAll { $0.id == sessionId }
        
        if currentSessionId == sessionId {
            if let firstSession = sessions.first {
                currentSessionId = firstSession.id
            } else {
                createNewSession()
            }
        }
        
        repository.saveSessions(sessions)
    }
    
    func switchToSession(_ sessionId: UUID) {
        if sessions.contains(where: { $0.id == sessionId }) {
            currentSessionId = sessionId
        }
    }
    
    func updateSession(_ session: ChatSession) {
        guard let index = sessions.firstIndex(where: { $0.id == session.id }) else { return }
        
        var updatedSession = session
        updatedSession.updatedAt = Date()
        
        // タイトルが未設定なら自動生成
        if updatedSession.title == "新しいチャット" {
            updatedSession.generateTitle()
        }
        
        sessions[index] = updatedSession
        repository.saveSessions(sessions)
    }
    
    func updateCurrentSession(messages: [ChatMessage], model: String) {
        guard let currentId = currentSessionId,
              var session = sessions.first(where: { $0.id == currentId }) else { return }
        
        session.messages = messages
        session.model = model
        updateSession(session)
    }
    
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
    
    private func formatDate(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        formatter.locale = Locale(identifier: "ja_JP")
        return formatter.string(from: date)
    }
}

// MARK: - Repository Protocol

protocol SessionRepositoryProtocol {
    func loadSessions() -> [ChatSession]
    func saveSessions(_ sessions: [ChatSession])
}

// MARK: - Repository Implementation

final class SessionRepository: SessionRepositoryProtocol {
    private let userDefaults = UserDefaults.standard
    private let sessionsKey = "VantageChatSessions"
    
    func loadSessions() -> [ChatSession] {
        guard let data = userDefaults.data(forKey: sessionsKey),
              let sessions = try? JSONDecoder().decode([ChatSession].self, from: data) else {
            return []
        }
        return sessions
    }
    
    func saveSessions(_ sessions: [ChatSession]) {
        guard let data = try? JSONEncoder().encode(sessions) else { return }
        userDefaults.set(data, forKey: sessionsKey)
    }
}