import SwiftUI
import RealityKit

/// AIアシスタントのチャットビュー
struct AIAssistantView: View {
    @Bindable var model: AIAssistantModel
    @State private var scrollToBottom = false
    @FocusState private var isInputFocused: Bool
    
    var body: some View {
        ZStack {
            // 背景（半透明のガラス効果）
            RoundedRectangle(cornerRadius: 20)
                .fill(.regularMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 20)
                        .strokeBorder(.white.opacity(0.2), lineWidth: 1)
                )
            
            VStack(spacing: 0) {
                // ヘッダー
                headerView
                
                Divider()
                    .foregroundColor(.white.opacity(0.2))
                
                // チャット履歴
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(model.messages) { message in
                                MessageBubble(message: message)
                                    .id(message.id)
                            }
                            
                            if model.isLoading {
                                LoadingIndicator()
                            }
                        }
                        .padding()
                    }
                    .onChange(of: model.messages.count) { _, _ in
                        withAnimation {
                            proxy.scrollTo(model.messages.last?.id, anchor: .bottom)
                        }
                    }
                }
                
                Divider()
                    .foregroundColor(.white.opacity(0.2))
                
                // 入力エリア
                inputView
            }
        }
        .frame(width: 600, height: 800)
        .frame(depth: 50)
    }
    
    private var headerView: some View {
        HStack {
            Image(systemName: "cpu")
                .font(.title2)
                .foregroundColor(.blue)
            
            Text("AI アシスタント")
                .font(.headline)
            
            Spacer()
            
            Button {
                withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) {
                    model.isShowing = false
                }
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title2)
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
        }
        .padding()
    }
    
    private var inputView: some View {
        HStack(spacing: 12) {
            TextField("メッセージを入力...", text: $model.inputText, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...3)
                .focused($isInputFocused)
                .onSubmit {
                    sendMessage()
                }
                .disabled(model.isLoading || !model.hasAPIKey)
            
            Button(action: sendMessage) {
                Image(systemName: "paperplane.fill")
                    .font(.title3)
            }
            .disabled(model.inputText.isEmpty || model.isLoading || !model.hasAPIKey)
        }
        .padding()
        .background(Color.gray.opacity(0.1))
        .cornerRadius(10)
        .padding()
    }
    
    private func sendMessage() {
        guard !model.inputText.isEmpty else { return }
        
        let text = model.inputText
        Task {
            await model.sendMessage(text)
        }
    }
}

/// メッセージバブル
struct MessageBubble: View {
    let message: ChatMessage
    
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            if message.isUser {
                Spacer(minLength: 60)
            }
            
            VStack(alignment: message.isUser ? .trailing : .leading, spacing: 4) {
                Text(message.content)
                    .font(.body)
                    .foregroundColor(message.isUser ? .white : .primary)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(
                        RoundedRectangle(cornerRadius: 16)
                            .fill(message.isUser ? Color.blue : Color.gray.opacity(0.2))
                    )
                
                Text(message.timestamp, style: .time)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            
            if !message.isUser {
                Spacer(minLength: 60)
            }
        }
    }
}

/// ローディングインジケーター
struct LoadingIndicator: View {
    @State private var isAnimating = false
    
    var body: some View {
        HStack(spacing: 4) {
            ForEach(0..<3) { index in
                Circle()
                    .fill(Color.blue)
                    .frame(width: 8, height: 8)
                    .scaleEffect(isAnimating ? 1.0 : 0.5)
                    .animation(
                        .easeInOut(duration: 0.6)
                        .repeatForever()
                        .delay(Double(index) * 0.2),
                        value: isAnimating
                    )
            }
        }
        .padding(.leading, 16)
        .onAppear {
            isAnimating = true
        }
    }
}