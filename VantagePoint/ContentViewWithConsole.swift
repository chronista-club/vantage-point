import SwiftUI
import ClaudeIntegration

struct ContentViewWithConsole: View {
    @StateObject private var viewModel = ChatViewModel()
    @State private var messageText = ""
    @State private var apiKey = ""
    @State private var showingAPIKeyAlert = false
    @State private var showConsole = false
    @State private var consoleHeight: CGFloat = 200
    @State private var showErrorBanner = false
    @State private var currentError: ClaudeIntegrationError?
    @State private var messageHistoryIndex = -1
    @State private var showingShortcutHelp = false
    @State private var showingSessionList = false
    @FocusState private var isInputFocused: Bool
    
    var body: some View {
        VSplitView {
            // メインチャットビュー
            VStack(spacing: 0) {
                // ヘッダー
                headerView
                
                // エラーバナー
                if showErrorBanner, let error = currentError {
                    ErrorBannerView(
                        error: error,
                        onDismiss: {
                            withAnimation {
                                showErrorBanner = false
                                currentError = nil
                            }
                        },
                        onRetry: error.isRetryable ? {
                            Task {
                                await viewModel.retryLastMessage()
                            }
                        } : nil
                    )
                    .padding(.horizontal)
                    .padding(.top, 8)
                    .transition(.move(edge: .top).combined(with: .opacity))
                }
                
                Divider()
                
                // チャット履歴
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(viewModel.messages) { message in
                                StreamingMessageView(
                                    message: message,
                                    isStreaming: viewModel.streamingMessageId == message.id
                                )
                                .id(message.id)
                            }
                        }
                        .padding()
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color(NSColor.controlBackgroundColor))
                    .onChange(of: viewModel.messages.count) { _, _ in
                        withAnimation {
                            proxy.scrollTo(viewModel.messages.last?.id, anchor: .bottom)
                        }
                    }
                }
                
                Divider()
                
                // 入力エリア
                inputView
            }
            .frame(minHeight: 300)
            
            if showConsole {
                // コンソールビュー
                ConsoleView(logs: viewModel.consoleLogs)
                    .frame(height: consoleHeight)
                    .frame(minHeight: 100, maxHeight: 400)
            }
        }
        .frame(width: 900, height: 700)
        .onAppear {
            // APIキーがない場合はアラートを表示
            if !viewModel.hasAPIKey {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                    showingAPIKeyAlert = true
                }
            }
            // 入力欄にフォーカス
            isInputFocused = true
        }
        .background(
            Group {
                Button("") {
                    withAnimation {
                        showConsole.toggle()
                    }
                }
                .keyboardShortcut("/", modifiers: .command)
                .hidden()
                
                Button("") {
                    viewModel.clearMessages()
                    messageText = ""
                    messageHistoryIndex = -1
                }
                .keyboardShortcut("n", modifiers: .command)
                .hidden()
                
                Button("") {
                    showingSessionList = true
                }
                .keyboardShortcut("h", modifiers: [.command, .shift])
                .hidden()
            }
        )
        .onChange(of: viewModel.lastError) { newError in
            if let error = newError {
                withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) {
                    currentError = error
                    showErrorBanner = true
                }
            }
        }
        .alert("APIキーを入力", isPresented: $showingAPIKeyAlert) {
            SecureField("Claude API Key", text: $apiKey)
            Button("設定") {
                viewModel.setAPIKey(apiKey)
                showingAPIKeyAlert = false
            }
            .disabled(apiKey.isEmpty)
            Button("キャンセル") {
                showingAPIKeyAlert = false
            }
        } message: {
            Text("Claude APIを使用するにはAPIキーが必要です")
        }
        .toolbar {
            ToolbarItem(placement: .automatic) {
                Button {
                    showingSessionList = true
                } label: {
                    Label("履歴", systemImage: "clock.arrow.circlepath")
                }
            }
            
            ToolbarItem(placement: .automatic) {
                Button {
                    withAnimation {
                        showConsole.toggle()
                    }
                } label: {
                    Label("コンソール", systemImage: showConsole ? "terminal.fill" : "terminal")
                }
            }
            
            ToolbarItem(placement: .automatic) {
                Menu {
                    ForEach(ClaudeModel.allCases, id: \.self) { model in
                        Button(model.displayName) {
                            viewModel.selectedModel = model
                        }
                    }
                } label: {
                    Label(viewModel.selectedModel.displayName, systemImage: "cpu")
                }
            }
            
            ToolbarItem(placement: .automatic) {
                Button {
                    showingAPIKeyAlert = true
                } label: {
                    Label("APIキー設定", systemImage: "key")
                }
            }
            
            ToolbarItem(placement: .automatic) {
                Button {
                    showingShortcutHelp = true
                } label: {
                    Label("ショートカット", systemImage: "keyboard")
                }
            }
        }
        .sheet(isPresented: $showingShortcutHelp) {
            ShortcutHelpView()
        }
        .sheet(isPresented: $showingSessionList) {
            SessionListView(
                sessionManager: viewModel.sessionManager,
                showingSessionList: $showingSessionList,
                onSessionSelected: { sessionId in
                    viewModel.sessionManager.switchToSession(sessionId)
                    if let session = viewModel.sessionManager.currentSession {
                        viewModel.messages = session.messages
                        viewModel.selectedModel = ClaudeModel.allCases.first { $0.rawValue == session.model } ?? .claude35Sonnet
                        messageText = ""
                        messageHistoryIndex = -1
                    }
                },
                onNewSession: {
                    viewModel.clearMessages()
                    messageText = ""
                    messageHistoryIndex = -1
                }
            )
        }
    }
    
    private var headerView: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Vantage for Mac")
                    .font(.headline)
                
                if let session = viewModel.sessionManager.currentSession {
                    Text(session.title)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                }
            }
            
            Spacer()
            
            if let error = viewModel.errorMessage {
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
            }
        }
        .padding()
        .background(Color(NSColor.windowBackgroundColor))
    }
    
    private var inputView: some View {
        HStack(alignment: .bottom, spacing: 12) {
            inputField
            sendButton
        }
        .padding()
        .background(Color(NSColor.windowBackgroundColor))
    }
    
    private var inputField: some View {
        ZStack(alignment: .topLeading) {
            TextEditor(text: $messageText)
                .font(.body)
                .focused($isInputFocused)
                .frame(minHeight: 36, maxHeight: 120)
                .fixedSize(horizontal: false, vertical: true)
                .padding(4)
                .background(Color(NSColor.textBackgroundColor))
                .cornerRadius(6)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(Color(NSColor.separatorColor), lineWidth: 1)
                )
                .onKeyPress(.return, modifiers: .command) {
                    sendMessage()
                    return .handled
                }
                .onKeyPress(.k, modifiers: .command) {
                    messageText = ""
                    return .handled
                }
                .onKeyPress(.upArrow, modifiers: .command) {
                    navigateMessageHistory(direction: .up)
                    return .handled
                }
                .onKeyPress(.downArrow, modifiers: .command) {
                    navigateMessageHistory(direction: .down)
                    return .handled
                }
            
            if messageText.isEmpty {
                Text("メッセージを入力... (Cmd+Enterで送信)")
                    .foregroundColor(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 12)
                    .allowsHitTesting(false)
            }
        }
        .disabled(viewModel.isLoading || !viewModel.hasAPIKey)
    }
    
    private var sendButton: some View {
        Group {
            if viewModel.isLoading && viewModel.streamingMessageId != nil {
                Button(action: {
                    viewModel.cancelStreaming()
                }) {
                    Image(systemName: "stop.circle.fill")
                        .foregroundColor(.red)
                }
                .buttonStyle(.plain)
            } else {
                Button(action: sendMessage) {
                    Image(systemName: "paperplane.fill")
                        .foregroundColor(.accentColor)
                }
                .buttonStyle(.plain)
                .disabled(messageText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || viewModel.isLoading || !viewModel.hasAPIKey)
            }
        }
    }
    
    private func sendMessage() {
        let trimmedText = messageText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedText.isEmpty else { return }
        
        // メッセージ履歴に追加
        viewModel.addToMessageHistory(trimmedText)
        messageHistoryIndex = -1
        
        messageText = ""
        
        Task {
            await viewModel.sendMessage(trimmedText)
        }
    }
    
    private func navigateMessageHistory(direction: NavigationDirection) {
        let history = viewModel.messageHistory
        guard !history.isEmpty else { return }
        
        switch direction {
        case .up:
            if messageHistoryIndex < history.count - 1 {
                messageHistoryIndex += 1
                messageText = history[history.count - 1 - messageHistoryIndex]
            }
        case .down:
            if messageHistoryIndex > 0 {
                messageHistoryIndex -= 1
                messageText = history[history.count - 1 - messageHistoryIndex]
            } else if messageHistoryIndex == 0 {
                messageHistoryIndex = -1
                messageText = ""
            }
        }
    }
}

enum NavigationDirection {
    case up, down
}

// コンソールビュー
struct ConsoleView: View {
    let logs: [ConsoleLog]
    @State private var searchText = ""
    
    var filteredLogs: [ConsoleLog] {
        if searchText.isEmpty {
            return logs
        }
        return logs.filter { log in
            log.message.localizedCaseInsensitiveContains(searchText) ||
            log.level.rawValue.localizedCaseInsensitiveContains(searchText)
        }
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // ヘッダー
            HStack {
                Label("コンソール", systemImage: "terminal")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Spacer()
                
                // 検索フィールド
                HStack {
                    Image(systemName: "magnifyingglass")
                        .foregroundColor(.secondary)
                    TextField("フィルタ...", text: $searchText)
                        .textFieldStyle(.plain)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(Color(NSColor.textBackgroundColor))
                .cornerRadius(4)
                .frame(width: 200)
                
                // クリアボタン
                Button {
                    if let viewModel = ConsoleLog.sharedViewModel {
                        viewModel.clearLogs()
                    }
                } label: {
                    Image(systemName: "trash")
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
            .background(Color(NSColor.windowBackgroundColor))
            
            Divider()
            
            // ログリスト
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(filteredLogs) { log in
                            ConsoleLogRow(log: log)
                                .id(log.id)
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color(NSColor.controlBackgroundColor))
                .onChange(of: logs.count) { _, _ in
                    withAnimation {
                        proxy.scrollTo(logs.last?.id, anchor: .bottom)
                    }
                }
            }
        }
    }
}

// コンソールログの行
struct ConsoleLogRow: View {
    let log: ConsoleLog
    
    var levelColor: Color {
        switch log.level {
        case .debug: return .gray
        case .info: return .primary
        case .warning: return .orange
        case .error: return .red
        }
    }
    
    var levelIcon: String {
        switch log.level {
        case .debug: return "ladybug"
        case .info: return "info.circle"
        case .warning: return "exclamationmark.triangle"
        case .error: return "xmark.circle"
        }
    }
    
    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            // タイムスタンプ
            Text(log.timestamp, style: .time)
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(.secondary)
                .frame(width: 80, alignment: .leading)
            
            // レベルアイコン
            Image(systemName: levelIcon)
                .foregroundColor(levelColor)
                .frame(width: 16)
            
            // メッセージ
            Text(log.message)
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(levelColor)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal)
        .padding(.vertical, 2)
        .background(log.level == .error ? Color.red.opacity(0.1) : Color.clear)
    }
}

// コンソールログモデル
struct ConsoleLog: Identifiable {
    let id = UUID()
    let timestamp: Date
    let level: LogLevel
    let message: String
    
    @MainActor static weak var sharedViewModel: ChatViewModel?
    
    enum LogLevel: String {
        case debug = "DEBUG"
        case info = "INFO"
        case warning = "WARNING"
        case error = "ERROR"
    }
}

#Preview {
    ContentViewWithConsole()
}