import SwiftUI
import ClaudeAPI

/// エラーバナー表示ビュー
struct ErrorBannerView: View {
    let error: ClaudeAPIError
    let onDismiss: () -> Void
    let onRetry: (() -> Void)?
    
    @State private var showDetails = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // メインエラー表示
            HStack(alignment: .top, spacing: 12) {
                // アイコン
                Image(systemName: iconName)
                    .font(.title3)
                    .foregroundColor(iconColor)
                    .frame(width: 24)
                
                // エラー内容
                VStack(alignment: .leading, spacing: 4) {
                    Text(error.userFriendlyMessage)
                        .font(.body)
                        .foregroundColor(.primary)
                    
                    if let suggestion = error.suggestedAction {
                        Text(suggestion)
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                
                // アクションボタン
                HStack(spacing: 8) {
                    if error.isRetryable, let onRetry = onRetry {
                        Button("再試行") {
                            onRetry()
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                    }
                    
                    Button {
                        showDetails.toggle()
                    } label: {
                        Image(systemName: showDetails ? "chevron.up" : "chevron.down")
                            .font(.caption)
                    }
                    .buttonStyle(.plain)
                    
                    Button {
                        onDismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.caption)
                    }
                    .buttonStyle(.plain)
                }
            }
            
            // 詳細表示
            if showDetails {
                Divider()
                
                VStack(alignment: .leading, spacing: 4) {
                    Label("技術的な詳細", systemImage: "info.circle")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    Text(error.localizedDescription)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundColor(.secondary)
                        .textSelection(.enabled)
                        .padding(8)
                        .background(Color(NSColor.textBackgroundColor))
                        .cornerRadius(4)
                    
                    if case .rateLimited(let retryAfter) = error, let retryAfter = retryAfter {
                        HStack {
                            Text("再試行可能まで:")
                            RetryCountdownView(seconds: Int(retryAfter))
                        }
                        .font(.caption)
                        .foregroundColor(.secondary)
                    }
                }
            }
        }
        .padding()
        .background(backgroundColour)
        .cornerRadius(8)
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(borderColor, lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.1), radius: 2, x: 0, y: 1)
    }
    
    private var iconName: String {
        switch error.severity {
        case .critical:
            return "exclamationmark.octagon.fill"
        case .error:
            return "xmark.circle.fill"
        case .warning:
            return "exclamationmark.triangle.fill"
        case .temporary:
            return "clock.fill"
        }
    }
    
    private var iconColor: Color {
        switch error.severity {
        case .critical, .error:
            return .red
        case .warning:
            return .orange
        case .temporary:
            return .blue
        }
    }
    
    private var backgroundColour: Color {
        switch error.severity {
        case .critical, .error:
            return Color.red.opacity(0.1)
        case .warning:
            return Color.orange.opacity(0.1)
        case .temporary:
            return Color.blue.opacity(0.1)
        }
    }
    
    private var borderColor: Color {
        switch error.severity {
        case .critical, .error:
            return Color.red.opacity(0.3)
        case .warning:
            return Color.orange.opacity(0.3)
        case .temporary:
            return Color.blue.opacity(0.3)
        }
    }
}

/// 再試行カウントダウン表示
struct RetryCountdownView: View {
    let seconds: Int
    @State private var remainingSeconds: Int
    
    init(seconds: Int) {
        self.seconds = seconds
        self._remainingSeconds = State(initialValue: seconds)
    }
    
    var body: some View {
        Text("\(remainingSeconds)秒")
            .monospacedDigit()
            .onAppear {
                startCountdown()
            }
    }
    
    private func startCountdown() {
        Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { timer in
            if remainingSeconds > 0 {
                remainingSeconds -= 1
            } else {
                timer.invalidate()
            }
        }
    }
}

// プレビュー
#Preview("Rate Limited Error") {
    ErrorBannerView(
        error: .rateLimited(retryAfter: 30),
        onDismiss: {},
        onRetry: {}
    )
    .padding()
    .frame(width: 600)
}

#Preview("Network Error") {
    ErrorBannerView(
        error: .networkError(NSError(domain: NSURLErrorDomain, code: NSURLErrorNotConnectedToInternet)),
        onDismiss: {},
        onRetry: {}
    )
    .padding()
    .frame(width: 600)
}