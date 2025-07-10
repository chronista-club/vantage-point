import SwiftUI

/// 入力補助情報を表示するビュー
struct InputStatusView: View {
    let text: String
    let isLoading: Bool
    let characterLimit: Int = 4000 // Claude APIの一般的な制限
    
    var characterCount: Int {
        text.count
    }
    
    var isNearLimit: Bool {
        characterCount > characterLimit * 3 / 4
    }
    
    var isOverLimit: Bool {
        characterCount > characterLimit
    }
    
    var body: some View {
        HStack(spacing: 12) {
            // 文字数カウンター
            if !text.isEmpty {
                HStack(spacing: 4) {
                    Image(systemName: "character.cursor.ibeam")
                        .font(.caption2)
                    Text("\(characterCount)")
                        .font(.caption2)
                        .monospacedDigit()
                    
                    if characterLimit > 0 {
                        Text("/ \(characterLimit)")
                            .font(.caption2)
                            .foregroundColor(.secondary)
                    }
                }
                .foregroundColor(colorForCharacterCount)
            }
            
            // 入力中インジケーター
            if isLoading {
                HStack(spacing: 4) {
                    ProgressView()
                        .scaleEffect(0.7)
                        .frame(width: 12, height: 12)
                    Text("送信中...")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            
            // ヒント表示
            if text.isEmpty && !isLoading {
                Text("Cmd+Enter で送信")
                    .font(.caption2)
                    .foregroundColor(.tertiary)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: text.isEmpty)
        .animation(.easeInOut(duration: 0.2), value: isLoading)
    }
    
    private var colorForCharacterCount: Color {
        if isOverLimit {
            return .red
        } else if isNearLimit {
            return .orange
        } else {
            return .secondary
        }
    }
}

// プレビュー
#Preview("通常") {
    InputStatusView(text: "これはテストメッセージです", isLoading: false)
        .padding()
}

#Preview("文字数多め") {
    InputStatusView(text: String(repeating: "あ", count: 3500), isLoading: false)
        .padding()
}

#Preview("送信中") {
    InputStatusView(text: "メッセージ", isLoading: true)
        .padding()
}