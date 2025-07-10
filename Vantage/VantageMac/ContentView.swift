import SwiftUI
import ClaudeAPI

struct ContentView: View {
    @StateObject private var viewModel = ChatViewModel()
    @State private var messageText = ""
    @State private var apiKey = ""
    @State private var showingAPIKeyAlert = false
    @FocusState private var isInputFocused: Bool
    
    var body: some View {
        VStack(spacing: 0) {
            // ヘッダー
            headerView
            
            Divider()
            
            // チャット履歴
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(viewModel.messages) { message in
                            MessageView(message: message)
                                .id(message.id)
                        }
                        
                        if viewModel.isLoading {
                            LoadingMessageView()
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
        .frame(width: 700, height: 600)
        .onAppear {
            // APIキーがない場合はアラートを表示
            if !viewModel.hasAPIKey {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                    showingAPIKeyAlert = true
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
        }
    }
    
    private var headerView: some View {
        HStack {
            Text("Vantage Mac")
                .font(.headline)
            
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
        HStack(spacing: 12) {
            TextField("メッセージを入力...", text: $messageText, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...5)
                .focused($isInputFocused)
                .onSubmit {
                    sendMessage()
                }
                .disabled(viewModel.isLoading || !viewModel.hasAPIKey)
            
            Button(action: sendMessage) {
                Image(systemName: "paperplane.fill")
                    .foregroundColor(.accentColor)
            }
            .buttonStyle(.plain)
            .disabled(messageText.isEmpty || viewModel.isLoading || !viewModel.hasAPIKey)
        }
        .padding()
        .background(Color(NSColor.controlBackgroundColor))
    }
    
    private func sendMessage() {
        guard !messageText.isEmpty else { return }
        
        let text = messageText
        messageText = ""
        
        Task {
            await viewModel.sendMessage(text)
        }
    }
}

struct MessageView: View {
    let message: ChatMessage
    
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            // アイコン
            Image(systemName: message.isUser ? "person.circle.fill" : "cpu")
                .font(.title2)
                .foregroundColor(message.isUser ? .blue : .green)
                .frame(width: 30)
            
            // メッセージ内容
            VStack(alignment: .leading, spacing: 4) {
                Text(message.isUser ? "You" : "Claude")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Text(message.content)
                    .textSelection(.enabled)
                    .font(.body)
                
                if let timestamp = message.timestamp {
                    Text(timestamp, style: .time)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            
            Spacer()
        }
        .padding(.vertical, 4)
    }
}

struct LoadingMessageView: View {
    @State private var dots = ""
    
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "cpu")
                .font(.title2)
                .foregroundColor(.green)
                .frame(width: 30)
            
            VStack(alignment: .leading, spacing: 4) {
                Text("Claude")
                    .font(.caption)
                    .foregroundColor(.secondary)
                
                Text("考えています\(dots)")
                    .font(.body)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
        }
        .padding(.vertical, 4)
        .onAppear {
            animateDots()
        }
    }
    
    private func animateDots() {
        Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { timer in
            if dots.count >= 3 {
                dots = ""
            } else {
                dots += "."
            }
        }
    }
}