import SwiftUI

/// プロジェクト1行分の表示
struct ProjectRowView: View {
    let project: ProjectItem
    let onStart: () -> Void
    let onStop: () -> Void
    let onOpenPPWindow: () -> Void
    let onOpenWebUI: () -> Void
    let onOpenTUI: () -> Void

    @State private var isHovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // プロジェクト名 + ステータス
            HStack(spacing: 8) {
                statusIndicator
                Text(project.name)
                    .font(.system(size: 14, weight: .medium))
                if project.status == .stopped {
                    Button(action: onStart) {
                        Image(systemName: "play.fill")
                            .font(.system(size: 10))
                            .foregroundColor(.green)
                    }
                    .buttonStyle(.plain)
                } else if project.status == .starting {
                    ProgressView()
                        .scaleEffect(0.5)
                }
                Spacer()
                if let port = project.port {
                    Text("[::1]:\(String(port))")
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundColor(.secondary)
                }
            }

            // パス表示
            Text(abbreviatePath(project.path))
                .font(.system(size: 11))
                .foregroundColor(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)

            // アクションボタン
            if isHovering || project.status == .running {
                HStack(spacing: 9) {
                    if project.status == .running, project.port != nil {
                        ActionButton(title: "TUI", icon: "terminal") {
                            onOpenTUI()
                        }
                        ActionButton(title: "Canvas", icon: "macwindow") {
                            onOpenPPWindow()
                        }
                        ActionButton(title: "WebUI", icon: "globe") {
                            onOpenWebUI()
                        }
                        Spacer()
                        ActionButton(title: "Stop", icon: "stop.fill", tint: .red) {
                            onStop()
                        }
                    } else if project.status == .stopping {
                        ProgressView()
                            .scaleEffect(0.65)
                        Text("Stopping...")
                            .font(.system(size: 11))
                            .foregroundColor(.secondary)
                        Spacer()
                    }
                }
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .background(isHovering ? Color.primary.opacity(0.05) : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .onHover { hovering in
            withAnimation(.easeInOut(duration: 0.15)) {
                isHovering = hovering
            }
        }
    }

    /// ステータスインジケーター（丸いドット）
    private var statusIndicator: some View {
        Circle()
            .fill(statusColor)
            .frame(width: 9, height: 9)
    }

    private var statusColor: Color {
        switch project.status {
        case .running: .green
        case .starting, .stopping: .orange
        case .error: .red
        case .stopped: .gray
        }
    }

    /// パスを短縮表示
    private func abbreviatePath(_ path: String) -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        if path.hasPrefix(home) {
            return "~" + path.dropFirst(home.count)
        }
        return path
    }
}

/// 小さいアクションボタン
struct ActionButton: View {
    let title: String
    let icon: String
    var tint: Color = .accentColor
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 3) {
                Image(systemName: icon)
                    .font(.system(size: 10))
                Text(title)
                    .font(.system(size: 11))
            }
            .foregroundColor(tint)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(tint.opacity(0.1))
            .clipShape(RoundedRectangle(cornerRadius: 4))
        }
        .buttonStyle(.plain)
    }
}
