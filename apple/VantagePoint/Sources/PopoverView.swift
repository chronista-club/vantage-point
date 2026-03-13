import SwiftUI

/// メニューバーポップオーバーのメインビュー
struct PopoverView: View {
    @ObservedObject var viewModel: PopoverViewModel
    let onCheckUpdates: () -> Void
    let onSettings: () -> Void
    let onOpenWindow: () -> Void
    let onQuit: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            // ヘッダー
            headerView
            Divider()

            // TheWorld 未接続時のバナー
            if viewModel.theWorldState != .connected {
                theWorldBanner
                Divider()
            }

            // プロジェクトリスト
            if viewModel.isLoading, viewModel.projects.isEmpty {
                loadingView
            } else if viewModel.theWorldState == .disconnected, viewModel.projects.isEmpty {
                disconnectedView
            } else if viewModel.projects.isEmpty {
                emptyView
            } else {
                projectListView
            }

            Divider()

            // フッター
            footerView
        }
        .frame(width: 340, height: min(CGFloat(max(viewModel.projects.count, 1)) * 85 + 135, 520))
        .task {
            await viewModel.refresh()
        }
    }

    // MARK: - Header

    private var headerView: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Vantage Point")
                    .font(.system(size: 14, weight: .semibold))
                HStack(spacing: 4) {
                    Circle()
                        .fill(theWorldStatusColor)
                        .frame(width: 6, height: 6)
                    Text(theWorldStatusText)
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                }
            }

            Spacer()

            // TheWorld リスタートボタン（接続中のみ）
            if viewModel.theWorldState == .connected {
                Button(
                    action: { Task { await viewModel.restartTheWorld() } },
                    label: {
                        Image(systemName: "arrow.triangle.2.circlepath")
                            .font(.system(size: 12))
                    }
                )
                .buttonStyle(.plain)
                .foregroundColor(.secondary)
                .help("Restart TheWorld")
            }

            Button(
                action: { Task { await viewModel.refresh() } },
                label: {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 13))
                }
            )
            .buttonStyle(.plain)
            .foregroundColor(.secondary)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 11)
    }

    /// TheWorld 未接続時のバナー
    private var theWorldBanner: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 12))
                .foregroundColor(.orange)

            Text("TheWorld is not running")
                .font(.system(size: 12))
                .foregroundColor(.secondary)

            Spacer()

            if viewModel.theWorldState == .starting {
                ProgressView()
                    .scaleEffect(0.6)
            } else {
                Button("Start") {
                    Task { await viewModel.startTheWorld() }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Color.orange.opacity(0.08))
    }

    /// TheWorld ステータスカラー
    private var theWorldStatusColor: Color {
        switch viewModel.theWorldState {
        case .connected: .green
        case .disconnected: .red
        case .starting: .orange
        }
    }

    /// TheWorld ステータステキスト
    private var theWorldStatusText: String {
        switch viewModel.theWorldState {
        case .connected: "TheWorld connected"
        case .disconnected: "TheWorld offline"
        case .starting: "Starting TheWorld..."
        }
    }

    // MARK: - Project List

    private var projectListView: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                let running = viewModel.projects.filter { $0.status == .running || $0.status == .starting }
                let stopped = viewModel.projects.filter {
                    $0.status == .stopped || $0.status == .error || $0.status == .stopping
                }

                if !running.isEmpty {
                    sectionHeader("Running")
                    ForEach(running) { project in
                        projectRow(project)
                    }
                }

                if !stopped.isEmpty {
                    if !running.isEmpty {
                        Divider().padding(.vertical, 4)
                    }
                    sectionHeader("Projects")
                    ForEach(stopped) { project in
                        projectRow(project)
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        HStack {
            Text(title)
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(.secondary)
                .textCase(.uppercase)
            Spacer()
        }
        .padding(.horizontal, 14)
        .padding(.top, 5)
        .padding(.bottom, 2)
    }

    private func projectRow(_ project: ProjectItem) -> some View {
        ProjectRowView(
            project: project,
            onStart: {
                Task { await viewModel.startProcess(projectName: project.name) }
            },
            onStop: {
                Task { await viewModel.stopProcess(projectName: project.name) }
            },
            onOpenPPWindow: {
                Task { await viewModel.openPointView(projectName: project.name) }
            },
            onOpenWebUI: {
                if let port = project.port {
                    viewModel.openWebUI(port: port)
                }
            },
            onOpenTUI: {
                viewModel.openTUI(projectPath: project.path)
            }
        )
    }

    // MARK: - Empty, Loading & Disconnected

    private var loadingView: some View {
        VStack(spacing: 9) {
            ProgressView()
            Text("Loading projects...")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: 120)
    }

    private var disconnectedView: some View {
        VStack(spacing: 9) {
            Image(systemName: "bolt.slash")
                .font(.system(size: 26))
                .foregroundColor(.secondary)
            Text("Start TheWorld to manage projects")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: 120)
    }

    private var emptyView: some View {
        VStack(spacing: 9) {
            Image(systemName: "folder.badge.questionmark")
                .font(.system(size: 26))
                .foregroundColor(.secondary)
            Text("No projects registered")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
            Text("Add projects to ~/.config/vp/config.toml")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: 120)
    }

    // MARK: - Footer

    private var footerView: some View {
        HStack {
            Button(action: onSettings) {
                Image(systemName: "gearshape")
                    .font(.system(size: 13))
            }
            .buttonStyle(.plain)
            .foregroundColor(.secondary)
            .help("Project Settings")

            Button(action: onOpenWindow) {
                Image(systemName: "macwindow")
                    .font(.system(size: 13))
            }
            .buttonStyle(.plain)
            .foregroundColor(.secondary)
            .help("Open Window")

            Button("Updates...") {
                onCheckUpdates()
            }
            .buttonStyle(.plain)
            .font(.system(size: 12))
            .foregroundColor(.secondary)

            Spacer()

            if let error = viewModel.errorMessage {
                Text(error)
                    .font(.system(size: 10))
                    .foregroundColor(.red)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: 100)
            }

            Spacer()

            Button("Quit") {
                onQuit()
            }
            .buttonStyle(.plain)
            .font(.system(size: 12))
            .foregroundColor(.secondary)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
    }
}
