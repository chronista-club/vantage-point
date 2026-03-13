import AppKit
import SwiftUI

/// プロジェクト設定管理ビュー
struct ProjectSettingsView: View {
    @StateObject private var viewModel = ProjectSettingsViewModel()
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            // ヘッダー
            HStack {
                Text("Projects")
                    .font(.headline)
                Spacer()
                Text(ConfigManager.shared.configPath.path)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .padding()

            Divider()

            // プロジェクトリスト
            if viewModel.projects.isEmpty {
                emptyState
            } else {
                projectList
            }

            Divider()

            // フッター
            footerActions
        }
        .frame(width: 520, height: 420)
        .onAppear {
            viewModel.load()
        }
    }

    // MARK: - Project List

    private var projectList: some View {
        List {
            ForEach($viewModel.projects) { $project in
                ProjectSettingsRow(
                    project: $project,
                    onBrowse: { viewModel.browseFolder(for: project.id) },
                    onDelete: { viewModel.removeProject(id: project.id) }
                )
            }
            .onMove { offsets, destination in
                viewModel.projects.move(fromOffsets: offsets, toOffset: destination)
                viewModel.save()
            }
        }
        .listStyle(.inset(alternatesRowBackgrounds: true))
    }

    // MARK: - Empty State

    private var emptyState: some View {
        VStack(spacing: 12) {
            Image(systemName: "folder.badge.plus")
                .font(.system(size: 36))
                .foregroundColor(.secondary)
            Text("No projects configured")
                .font(.system(size: 14))
                .foregroundColor(.secondary)
            Text("Add a project to get started")
                .font(.system(size: 12))
                .foregroundColor(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Footer

    private var footerActions: some View {
        HStack {
            Button(
                action: { viewModel.addProject() },
                label: {
                    HStack(spacing: 4) {
                        Image(systemName: "plus")
                        Text("Add Project")
                    }
                }
            )

            Button(
                action: { viewModel.addProjectFromFolder() },
                label: {
                    HStack(spacing: 4) {
                        Image(systemName: "folder")
                        Text("Browse...")
                    }
                }
            )

            Spacer()

            if viewModel.hasUnsavedChanges {
                Text("Unsaved changes")
                    .font(.system(size: 10))
                    .foregroundColor(.orange)
            }

            Button("Save") {
                viewModel.save()
            }
            .disabled(!viewModel.hasUnsavedChanges)
            .keyboardShortcut("s", modifiers: .command)
        }
        .padding()
    }
}

/// プロジェクト1行の編集ビュー
struct ProjectSettingsRow: View {
    @Binding var project: ConfigManager.ProjectEntry
    let onBrowse: () -> Void
    let onDelete: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                // プロジェクト名
                HStack(spacing: 4) {
                    Image(systemName: "folder.fill")
                        .foregroundColor(.accentColor)
                        .font(.system(size: 12))
                    TextField("Project Name", text: $project.name)
                        .textFieldStyle(.plain)
                        .font(.system(size: 13, weight: .medium))
                }

                Spacer()

                // ポート（オプション）
                HStack(spacing: 2) {
                    Text("Port:")
                        .font(.system(size: 10))
                        .foregroundColor(.secondary)
                    TextField("auto", text: Binding(
                        get: { project.port.map { String($0) } ?? "" },
                        set: { project.port = UInt16($0) }
                    ))
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 60)
                    .font(.system(size: 11, design: .monospaced))
                }

                // 削除ボタン
                Button(action: onDelete) {
                    Image(systemName: "trash")
                        .font(.system(size: 11))
                        .foregroundColor(.red)
                }
                .buttonStyle(.plain)
            }

            // パス
            HStack(spacing: 4) {
                TextField("Project Path", text: $project.path)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 11, design: .monospaced))

                Button(action: onBrowse) {
                    Image(systemName: "folder.badge.gearshape")
                        .font(.system(size: 12))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.vertical, 4)
    }
}

// MARK: - ViewModel

@MainActor
class ProjectSettingsViewModel: ObservableObject {
    @Published var projects: [ConfigManager.ProjectEntry] = []
    @Published var hasUnsavedChanges = false

    private var originalProjects: [ConfigManager.ProjectEntry] = []
    private let configManager = ConfigManager.shared

    func load() {
        let config = configManager.load()
        projects = config.projects
        originalProjects = config.projects
        hasUnsavedChanges = false
    }

    func save() {
        var config = configManager.load()
        config.projects = projects
        do {
            try configManager.save(config)
            originalProjects = projects
            hasUnsavedChanges = false
        } catch {
            print("[ProjectSettings] Save failed: \(error)")
        }
    }

    func addProject() {
        let newProject = ConfigManager.ProjectEntry(
            name: "new-project",
            path: ""
        )
        projects.append(newProject)
        hasUnsavedChanges = true
    }

    func addProjectFromFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Select a project directory"

        if panel.runModal() == .OK, let url = panel.url {
            let name = url.lastPathComponent
            let path = url.path
            let newProject = ConfigManager.ProjectEntry(name: name, path: path)
            projects.append(newProject)
            hasUnsavedChanges = true
            save() // フォルダ選択時は即保存
        }
    }

    func browseFolder(for id: UUID) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false

        if panel.runModal() == .OK, let url = panel.url {
            if let idx = projects.firstIndex(where: { $0.id == id }) {
                projects[idx].path = url.path
                if projects[idx].name.isEmpty || projects[idx].name == "new-project" {
                    projects[idx].name = url.lastPathComponent
                }
                hasUnsavedChanges = true
            }
        }
    }

    func removeProject(id: UUID) {
        projects.removeAll { $0.id == id }
        hasUnsavedChanges = true
        save() // 削除は即保存
    }
}

// MARK: - Settings Window Controller

@MainActor
class SettingsWindowController {
    private var window: NSWindow?

    func show() {
        if let window {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let settingsView = ProjectSettingsView()
        let hostingController = NSHostingController(rootView: settingsView)

        let window = NSWindow(contentViewController: hostingController)
        window.title = "Vantage Point - Project Settings"
        window.styleMask = [.titled, .closable, .resizable]
        window.setContentSize(NSSize(width: 520, height: 420))
        window.minSize = NSSize(width: 400, height: 300)
        window.center()
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)

        NSApp.activate(ignoringOtherApps: true)

        self.window = window
    }

    func close() {
        window?.close()
        window = nil
    }
}
