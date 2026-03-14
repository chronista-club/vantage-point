import AppKit
import SwiftUI

// MARK: - Font

/// Fira Code Nerd Font → system default フォールバック
private func vpFont(size: CGFloat, weight: Font.Weight = .regular) -> Font {
    if let _ = NSFont(name: "FiraCode Nerd Font", size: size) {
        return .custom("FiraCode Nerd Font", size: size).weight(weight)
    }
    return .system(size: size, weight: weight)
}

/// メニューバーポップオーバー — リスタート中心のシンプルメニュー
struct PopoverView: View {
    @ObservedObject var viewModel: PopoverViewModel
    let onQuit: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            headerView
            Divider()

            // グローバルアクション
            globalActions
            Divider()

            // プロジェクト別
            if viewModel.projects.isEmpty {
                emptyView
            } else {
                projectList
            }

            Divider()
            footerView
        }
        .frame(width: 300)
        .task {
            await viewModel.refresh()
        }
    }

    // MARK: - Header

    private var headerView: some View {
        HStack {
            Text("Vantage Point")
                .font(vpFont(size: 13, weight: .semibold))
            Spacer()
            Circle()
                .fill(viewModel.theWorldState == .connected ? Color.green : Color.red)
                .frame(width: 7, height: 7)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    // MARK: - Global Actions

    private var globalActions: some View {
        VStack(spacing: 0) {
            MenuRow(label: "Restart All Services", icon: "arrow.triangle.2.circlepath",
                    isLoading: viewModel.isRestartingAll) {
                Task { await viewModel.restartAll() }
            }
            MenuRow(label: "Restart Server", icon: "globe",
                    isLoading: viewModel.isRestartingTheWorld) {
                Task { await viewModel.restartTheWorld() }
            }
            MenuRow(label: "Restart App", icon: "arrow.clockwise", isLoading: false) {
                viewModel.restartApp()
            }
        }
    }

    // MARK: - Project List

    private var projectList: some View {
        ScrollView {
            VStack(spacing: 0) {
                ForEach(viewModel.projects) { project in
                    ProjectRow(
                        project: project,
                        onRestart: {
                            Task { await viewModel.restartProcess(projectName: project.name) }
                        },
                        onOpenWindow: {
                            viewModel.openWindow(projectName: project.name, projectPath: project.path)
                        }
                    )
                }
            }
        }
        .frame(maxHeight: 260)
    }

    // MARK: - Empty

    private var emptyView: some View {
        Text("No projects")
            .font(vpFont(size: 12))
            .foregroundColor(.secondary)
            .frame(maxWidth: .infinity, minHeight: 40)
    }

    // MARK: - Footer

    private var footerView: some View {
        HStack {
            Button("Quit") { onQuit() }
                .buttonStyle(.plain)
                .font(vpFont(size: 12))
                .foregroundColor(.secondary)
            Spacer()
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
    }
}

// MARK: - Menu Row（グローバルアクション用）

struct MenuRow: View {
    let label: String
    let icon: String
    let isLoading: Bool
    let action: () -> Void

    @State private var isHovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .font(vpFont(size: 12))
                    .frame(width: 16)
                    .foregroundColor(.secondary)

                Text(label)
                    .font(vpFont(size: 13))

                Spacer()

                if isLoading {
                    ProgressView()
                        .scaleEffect(0.5)
                        .frame(width: 16, height: 16)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 7)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(isHovering ? Color.primary.opacity(0.06) : Color.clear)
        .onHover { isHovering = $0 }
    }
}

// MARK: - Project Row

struct ProjectRow: View {
    let project: ProjectItem
    let onRestart: () -> Void
    let onOpenWindow: () -> Void

    @State private var isHovering = false

    private var isTransitioning: Bool {
        project.status == .starting || project.status == .stopping
    }

    var body: some View {
        HStack(spacing: 8) {
            // ステータスドット
            Circle()
                .fill(statusColor)
                .frame(width: 7, height: 7)

            // プロジェクト名
            Text(project.name)
                .font(vpFont(size: 13))
                .lineLimit(1)

            Spacer()

            if isTransitioning {
                ProgressView()
                    .scaleEffect(0.5)
                    .frame(width: 16, height: 16)
            } else if isHovering {
                // ウィンドウを開くボタン
                IconButton(icon: "macwindow", help: "Open Window") {
                    onOpenWindow()
                }

                // リスタートボタン（稼働中のみ）
                if project.status == .running {
                    IconButton(icon: "arrow.triangle.2.circlepath", help: "Restart") {
                        onRestart()
                    }
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 7)
        .contentShape(Rectangle())
        .background(isHovering ? Color.primary.opacity(0.06) : Color.clear)
        .onHover { isHovering = $0 }
    }

    private var statusColor: Color {
        switch project.status {
        case .running: .green
        case .starting, .stopping: .orange
        case .error: .red
        case .stopped: .gray
        }
    }
}

// MARK: - Icon Button

struct IconButton: View {
    let icon: String
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(vpFont(size: 11))
                .foregroundColor(.secondary)
                .frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }
}
