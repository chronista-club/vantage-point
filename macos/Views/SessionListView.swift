import SwiftUI

struct SessionListView: View {
    @ObservedObject var sessionManager: SessionService
    @Binding var showingSessionList: Bool
    var onSessionSelected: (UUID) -> Void
    var onNewSession: () -> Void
    
    @State private var searchText = ""
    @State private var showingDeleteConfirmation = false
    @State private var sessionToDelete: UUID?
    
    var filteredSessions: [ChatSession] {
        if searchText.isEmpty {
            return sessionManager.sessions
        }
        return sessionManager.sessions.filter { session in
            session.title.localizedCaseInsensitiveContains(searchText) ||
            session.messages.contains { $0.content.localizedCaseInsensitiveContains(searchText) }
        }
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // ヘッダー
            HStack {
                Text("チャット履歴")
                    .font(.headline)
                
                Spacer()
                
                Button {
                    showingSessionList = false
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding()
            .background(Color(NSColor.windowBackgroundColor))
            
            Divider()
            
            // 検索バー
            HStack {
                Image(systemName: "magnifyingglass")
                    .foregroundColor(.secondary)
                TextField("セッションを検索...", text: $searchText)
                    .textFieldStyle(.plain)
            }
            .padding(8)
            .background(Color(NSColor.textBackgroundColor))
            .cornerRadius(6)
            .padding(.horizontal)
            .padding(.top)
            
            // 新規セッションボタン
            Button(action: {
                onNewSession()
                showingSessionList = false
            }) {
                HStack {
                    Image(systemName: "plus.circle.fill")
                    Text("新しいチャット")
                    Spacer()
                }
                .padding()
                .background(Color.accentColor.opacity(0.1))
                .cornerRadius(8)
            }
            .buttonStyle(.plain)
            .padding(.horizontal)
            .padding(.vertical, 8)
            
            Divider()
            
            // セッション一覧
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(filteredSessions) { session in
                        SessionRowView(
                            session: session,
                            isSelected: session.id == sessionManager.currentSessionId,
                            onSelect: {
                                onSessionSelected(session.id)
                                showingSessionList = false
                            },
                            onDelete: {
                                sessionToDelete = session.id
                                showingDeleteConfirmation = true
                            },
                            onExport: { exportSession(session) }
                        )
                    }
                }
            }
            .frame(maxHeight: .infinity)
        }
        .frame(width: 350, height: 500)
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(12)
        .shadow(radius: 10)
        .confirmationDialog(
            "セッションを削除",
            isPresented: $showingDeleteConfirmation,
            presenting: sessionToDelete
        ) { sessionId in
            Button("削除", role: .destructive) {
                sessionManager.deleteSession(sessionId)
            }
            Button("キャンセル", role: .cancel) {}
        } message: { _ in
            Text("このセッションを削除してもよろしいですか？この操作は取り消せません。")
        }
    }
    
    private func exportSession(_ session: ChatSession) {
        guard let markdown = sessionManager.exportSession(session.id) else { return }
        
        let savePanel = NSSavePanel()
        savePanel.allowedContentTypes = [.plainText]
        savePanel.nameFieldStringValue = "\(session.title).md"
        
        savePanel.begin { response in
            if response == .OK, let url = savePanel.url {
                do {
                    try markdown.write(to: url, atomically: true, encoding: .utf8)
                } catch {
                    print("Failed to save file: \(error)")
                }
            }
        }
    }
}

struct SessionRowView: View {
    let session: ChatSession
    let isSelected: Bool
    let onSelect: () -> Void
    let onDelete: () -> Void
    let onExport: () -> Void
    
    @State private var isHovered = false
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(session.title)
                    .font(.system(size: 13))
                    .lineLimit(1)
                    .foregroundColor(isSelected ? .white : .primary)
                
                HStack {
                    Text(formatDate(session.updatedAt))
                        .font(.caption)
                        .foregroundColor(isSelected ? .white.opacity(0.8) : .secondary)
                    
                    Text("•")
                        .foregroundColor(isSelected ? .white.opacity(0.8) : .secondary)
                    
                    Text("\(session.messages.count)メッセージ")
                        .font(.caption)
                        .foregroundColor(isSelected ? .white.opacity(0.8) : .secondary)
                }
            }
            
            Spacer()
            
            if isHovered {
                HStack(spacing: 8) {
                    Button(action: onExport) {
                        Image(systemName: "square.and.arrow.up")
                            .foregroundColor(isSelected ? .white : .secondary)
                    }
                    .buttonStyle(.plain)
                    
                    Button(action: onDelete) {
                        Image(systemName: "trash")
                            .foregroundColor(isSelected ? .white : .secondary)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(isSelected ? Color.accentColor : (isHovered ? Color.gray.opacity(0.1) : Color.clear))
        )
        .onTapGesture {
            onSelect()
        }
        .onHover { hovering in
            isHovered = hovering
        }
    }
    
    private func formatDate(_ date: Date) -> String {
        let formatter = DateFormatter()
        let calendar = Calendar.current
        
        if calendar.isDateInToday(date) {
            formatter.dateFormat = "HH:mm"
            return "今日 " + formatter.string(from: date)
        } else if calendar.isDateInYesterday(date) {
            formatter.dateFormat = "HH:mm"
            return "昨日 " + formatter.string(from: date)
        } else if calendar.isDate(date, equalTo: Date(), toGranularity: .weekOfYear) {
            formatter.dateFormat = "E HH:mm"
            formatter.locale = Locale(identifier: "ja_JP")
            return formatter.string(from: date)
        } else {
            formatter.dateFormat = "MM/dd"
            return formatter.string(from: date)
        }
    }
}

#Preview {
    SessionListView(
        sessionManager: SessionService(),
        showingSessionList: .constant(true),
        onSessionSelected: { _ in },
        onNewSession: {}
    )
}