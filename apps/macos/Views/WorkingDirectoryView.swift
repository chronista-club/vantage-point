import SwiftUI

struct WorkingDirectoryView: View {
    @ObservedObject var viewModel: ChatViewModel
    @State private var showingFilePicker = false
    @State private var showingBookmarks = false
    
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // 現在のディレクトリ
            HStack {
                Label("作業ディレクトリ", systemImage: "folder.fill")
                    .font(.headline)
                
                Spacer()
                
                if viewModel.workingDirectory != nil {
                    Button(action: {
                        viewModel.workingDirectoryManager.stopAccessingCurrentDirectory()
                        viewModel.workingDirectory = nil
                    }) {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help("作業ディレクトリをクリア")
                }
            }
            
            if let currentDir = viewModel.workingDirectory {
                VStack(alignment: .leading, spacing: 4) {
                    Text(currentDir.lastPathComponent)
                        .font(.system(.body, design: .monospaced))
                        .fontWeight(.medium)
                    
                    Text(currentDir.path)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .padding(8)
                .background(Color(NSColor.controlBackgroundColor))
                .cornerRadius(6)
            } else {
                Text("ディレクトリが選択されていません")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .padding(8)
                    .frame(maxWidth: .infinity)
                    .background(Color(NSColor.controlBackgroundColor))
                    .cornerRadius(6)
            }
            
            // アクションボタン
            HStack(spacing: 8) {
                Button(action: { showingFilePicker = true }) {
                    Label("ディレクトリを選択", systemImage: "folder.badge.plus")
                }
                .buttonStyle(.borderedProminent)
                
                if !viewModel.workingDirectoryManager.bookmarkedDirectories.isEmpty {
                    Button(action: { showingBookmarks.toggle() }) {
                        Label("履歴", systemImage: "clock")
                    }
                }
            }
            
            // ブックマーク履歴
            if showingBookmarks && !viewModel.workingDirectoryManager.bookmarkedDirectories.isEmpty {
                Divider()
                
                VStack(alignment: .leading, spacing: 8) {
                    Text("最近使用したディレクトリ")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    ForEach(viewModel.workingDirectoryManager.bookmarkedDirectories) { bookmark in
                        BookmarkRow(bookmark: bookmark, viewModel: viewModel)
                    }
                }
            }
        }
        .padding()
        .background(Color(NSColor.windowBackgroundColor))
        .cornerRadius(8)
        .fileImporter(
            isPresented: $showingFilePicker,
            allowedContentTypes: [.folder],
            allowsMultipleSelection: false
        ) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first {
                    viewModel.setWorkingDirectory(url)
                }
            case .failure(let error):
                viewModel.addLog(level: .error, message: "ディレクトリ選択エラー: \(error.localizedDescription)")
            }
        }
    }
}

struct BookmarkRow: View {
    let bookmark: BookmarkedDirectory
    @ObservedObject var viewModel: ChatViewModel
    
    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(bookmark.name)
                    .font(.system(.body, design: .monospaced))
                    .lineLimit(1)
                
                Text(bookmark.formattedDate)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            
            Spacer()
            
            Button(action: {
                viewModel.restoreWorkingDirectory(from: bookmark)
            }) {
                Text("開く")
                    .font(.caption)
            }
            .buttonStyle(.borderless)
            
            Button(action: {
                viewModel.workingDirectoryManager.removeBookmark(bookmark)
            }) {
                Image(systemName: "trash")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 8)
        .background(Color(NSColor.controlBackgroundColor))
        .cornerRadius(4)
    }
}

#Preview {
    WorkingDirectoryView(viewModel: ChatViewModel())
        .frame(width: 350)
}