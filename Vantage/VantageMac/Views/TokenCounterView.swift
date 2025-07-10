import SwiftUI
import ClaudeAPI

/// トークンカウンター表示ビュー
struct TokenCounterView: View {
    let model: ClaudeModel
    let inputTokens: Int
    let outputTokens: Int
    let isStreaming: Bool
    
    private var totalTokens: Int {
        inputTokens + outputTokens
    }
    
    private var maxTokens: Int {
        model.contextWindow
    }
    
    private var usagePercentage: Double {
        Double(totalTokens) / Double(maxTokens)
    }
    
    private var usageColor: Color {
        if usagePercentage > 0.9 {
            return .red
        } else if usagePercentage > 0.7 {
            return .orange
        } else {
            return .green
        }
    }
    
    var body: some View {
        HStack(spacing: 8) {
            // モデル表示
            Label(model.displayName, systemImage: "cpu")
                .font(.caption2)
                .foregroundColor(.secondary)
            
            Divider()
                .frame(height: 12)
            
            // トークン使用状況
            HStack(spacing: 4) {
                Image(systemName: "chart.bar.fill")
                    .font(.caption2)
                
                if isStreaming {
                    Text("~\(totalTokens)")
                        .font(.caption2)
                        .monospacedDigit()
                } else {
                    Text("\(totalTokens)")
                        .font(.caption2)
                        .monospacedDigit()
                }
                
                Text("/ \(maxTokens)")
                    .font(.caption2)
                    .foregroundColor(.secondary)
                
                // プログレスバー
                GeometryReader { geometry in
                    ZStack(alignment: .leading) {
                        RoundedRectangle(cornerRadius: 2)
                            .fill(Color.gray.opacity(0.2))
                            .frame(height: 4)
                        
                        RoundedRectangle(cornerRadius: 2)
                            .fill(usageColor)
                            .frame(width: geometry.size.width * min(usagePercentage, 1.0), height: 4)
                            .animation(.easeInOut(duration: 0.3), value: usagePercentage)
                    }
                }
                .frame(width: 60, height: 4)
            }
            .foregroundColor(usageColor)
            
            if isStreaming {
                ProgressView()
                    .scaleEffect(0.5)
                    .frame(width: 12, height: 12)
            }
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Color(NSColor.controlBackgroundColor).opacity(0.8))
        .cornerRadius(4)
        .font(.caption2)
    }
}

// プレビュー
#Preview("通常") {
    TokenCounterView(
        model: .claude35Sonnet,
        inputTokens: 1000,
        outputTokens: 500,
        isStreaming: false
    )
    .padding()
}

#Preview("ストリーミング中") {
    TokenCounterView(
        model: .claude35Sonnet,
        inputTokens: 2000,
        outputTokens: 1500,
        isStreaming: true
    )
    .padding()
}

#Preview("使用量多め") {
    TokenCounterView(
        model: .claude35Sonnet,
        inputTokens: 80000,
        outputTokens: 20000,
        isStreaming: false
    )
    .padding()
}