import SwiftUI

/// ストリーミング中のメッセージ表示ビュー
struct StreamingMessageView: View {
    let message: ChatMessage
    let isStreaming: Bool
    @State private var displayedText = ""
    @State private var cursorVisible = true
    
    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            // アイコン
            Image(systemName: message.isUser ? "person.circle.fill" : "cpu")
                .font(.title2)
                .foregroundColor(message.isUser ? .blue : .green)
                .frame(width: 30)
            
            // メッセージ内容
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(message.isUser ? "You" : "Claude")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    if isStreaming && !message.isUser {
                        StreamingIndicator()
                    }
                }
                
                HStack(alignment: .bottom, spacing: 0) {
                    Text(displayedText)
                        .textSelection(.enabled)
                        .font(.body)
                    
                    if isStreaming && !message.isUser && cursorVisible {
                        Rectangle()
                            .fill(Color.accentColor)
                            .frame(width: 2, height: 16)
                            .opacity(cursorVisible ? 1 : 0)
                    }
                }
                
                if let timestamp = message.timestamp {
                    Text(timestamp, style: .time)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            
            Spacer()
        }
        .padding(.vertical, 4)
        .onAppear {
            if isStreaming {
                startCursorAnimation()
            }
            displayedText = message.content
        }
        .onChange(of: message.content) { oldValue, newValue in
            // アニメーションなしで即座に更新（パフォーマンス向上）
            displayedText = newValue
        }
        .onChange(of: isStreaming) { _, newValue in
            if !newValue {
                cursorVisible = false
            }
        }
    }
    
    private func startCursorAnimation() {
        let timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { _ in
            Task { @MainActor in
                if !isStreaming {
                    cursorVisible = false
                } else {
                    cursorVisible.toggle()
                }
            }
        }
        
        // タイマーを保持して、ストリーミングが終了したら無効化
        Task { @MainActor in
            while isStreaming {
                try? await Task.sleep(nanoseconds: 100_000_000) // 0.1秒
            }
            timer.invalidate()
        }
    }
}

/// ストリーミング中のインジケーター
struct StreamingIndicator: View {
    @State private var animationAmount = 0.0
    
    var body: some View {
        HStack(spacing: 3) {
            ForEach(0..<3) { index in
                Circle()
                    .fill(Color.green)
                    .frame(width: 4, height: 4)
                    .scaleEffect(animationAmount)
                    .opacity(animationAmount)
                    .animation(
                        .easeInOut(duration: 0.6)
                        .repeatForever()
                        .delay(Double(index) * 0.2),
                        value: animationAmount
                    )
            }
        }
        .onAppear {
            animationAmount = 1.0
        }
    }
}

// プレビュー
#Preview("通常メッセージ") {
    StreamingMessageView(
        message: ChatMessage(
            content: "これは通常のメッセージです。",
            isUser: false,
            timestamp: Date()
        ),
        isStreaming: false
    )
    .padding()
}

#Preview("ストリーミング中") {
    StreamingMessageView(
        message: ChatMessage(
            content: "これはストリーミング中のメッセージ",
            isUser: false
        ),
        isStreaming: true
    )
    .padding()
}