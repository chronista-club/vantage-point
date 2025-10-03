import SwiftUI
import UniformTypeIdentifiers

struct FileBrowserView: View {
    @ObservedObject var viewModel: ChatViewModel
    @State private var files: [FileItem] = []
    @State private var selectedFile: FileItem?
    @State private var isLoading = false
    @State private var errorMessage: String?
    
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // ヘッダー
            HStack {
                Label("ファイル", systemImage: "doc.text")
                    .font(.headline)
                
                Spacer()
                
                Button(action: refreshFiles) {
                    Image(systemName: "arrow.clockwise")
                        .font(.caption)
                }
                .buttonStyle(.plain)
                .disabled(isLoading || viewModel.workingDirectory == nil)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
            
            Divider()
            
            // ファイルリスト
            if let _ = viewModel.workingDirectory {
                if isLoading {
                    VStack {
                        ProgressView()
                            .scaleEffect(0.8)
                        Text("読み込み中...")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let error = errorMessage {
                    VStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.largeTitle)
                            .foregroundColor(.secondary)
                        Text(error)
                            .font(.caption)
                            .foregroundColor(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding()
                } else if files.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: "folder")
                            .font(.largeTitle)
                            .foregroundColor(.secondary)
                        Text("ファイルがありません")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ScrollView {
                        LazyVStack(spacing: 0) {
                            ForEach(files) { file in
                                FileRow(file: file, isSelected: selectedFile?.id == file.id)
                                    .onTapGesture {
                                        selectedFile = file
                                    }
                                    .contextMenu {
                                        Button("パスをコピー") {
                                            NSPasteboard.general.clearContents()
                                            NSPasteboard.general.setString(file.path, forType: .string)
                                        }
                                        
                                        if file.type == .file {
                                            Button("内容をチャットに追加") {
                                                includeFileInChat(file)
                                            }
                                        }
                                    }
                            }
                        }
                    }
                }
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "folder.badge.questionmark")
                        .font(.largeTitle)
                        .foregroundColor(.secondary)
                    Text("作業ディレクトリを選択してください")
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding()
            }
        }
        .background(Color(NSColor.controlBackgroundColor))
        .onAppear {
            refreshFiles()
        }
        .onChange(of: viewModel.workingDirectory) { _, _ in
            refreshFiles()
        }
    }
    
    private func refreshFiles() {
        guard let directory = viewModel.workingDirectory else {
            files = []
            return
        }
        
        isLoading = true
        errorMessage = nil
        
        Task {
            do {
                let items = try await loadFiles(from: directory)
                await MainActor.run {
                    self.files = items
                    self.isLoading = false
                }
            } catch {
                await MainActor.run {
                    self.errorMessage = error.localizedDescription
                    self.isLoading = false
                }
            }
        }
    }
    
    private func loadFiles(from directory: URL) async throws -> [FileItem] {
        let fileManager = FileManager.default
        let urls = try fileManager.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey],
            options: [.skipsHiddenFiles]
        )
        
        return urls.compactMap { url in
            guard let resourceValues = try? url.resourceValues(forKeys: [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey]) else {
                return nil
            }
            
            let isDirectory = resourceValues.isDirectory ?? false
            let fileSize = resourceValues.fileSize ?? 0
            let modificationDate = resourceValues.contentModificationDate ?? Date()
            
            return FileItem(
                name: url.lastPathComponent,
                path: url.path,
                type: isDirectory ? .directory : .file,
                size: Int64(fileSize),
                modificationDate: modificationDate
            )
        }.sorted { item1, item2 in
            // ディレクトリを先に、その後名前順
            if item1.type != item2.type {
                return item1.type == .directory
            }
            return item1.name.localizedCaseInsensitiveCompare(item2.name) == .orderedAscending
        }
    }
    
    private func includeFileInChat(_ file: FileItem) {
        Task {
            await viewModel.sendFileContent(path: file.path, fileName: file.name)
        }
    }
}

struct FileItem: Identifiable {
    let id = UUID()
    let name: String
    let path: String
    let type: FileType
    let size: Int64
    let modificationDate: Date
    
    enum FileType {
        case file
        case directory
    }
    
    var icon: String {
        switch type {
        case .directory:
            return "folder.fill"
        case .file:
            if name.hasSuffix(".swift") {
                return "swift"
            } else if name.hasSuffix(".md") {
                return "doc.text"
            } else if name.hasSuffix(".json") {
                return "doc.badge.gearshape"
            } else {
                return "doc"
            }
        }
    }
    
    var formattedSize: String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter.string(fromByteCount: size)
    }
}

struct FileRow: View {
    let file: FileItem
    let isSelected: Bool
    
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: file.icon)
                .font(.body)
                .foregroundColor(file.type == .directory ? .blue : .secondary)
            
            VStack(alignment: .leading, spacing: 2) {
                Text(file.name)
                    .font(.system(.body, design: .monospaced))
                    .lineLimit(1)
                
                if file.type == .file {
                    Text(file.formattedSize)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(isSelected ? Color.accentColor.opacity(0.1) : Color.clear)
    }
}

#Preview {
    FileBrowserView(viewModel: ChatViewModel())
        .frame(width: 300, height: 400)
}