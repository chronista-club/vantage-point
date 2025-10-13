import Foundation
import AppKit

/// 作業ディレクトリの管理とSecurity Scoped Bookmarkの処理を行うクラス
@MainActor
class WorkingDirectoryManager: ObservableObject {
    @Published var currentDirectory: URL?
    @Published var bookmarkedDirectories: [BookmarkedDirectory] = []
    
    private let bookmarkKey = "com.vantage.workingDirectories"
    private let maxBookmarks = 10
    
    init() {
        loadBookmarks()
    }
    
    /// ディレクトリを設定し、ブックマークとして保存
    func setDirectory(_ url: URL) -> Bool {
        // アクセス権限を開始
        guard url.startAccessingSecurityScopedResource() else {
            print("Failed to access security scoped resource")
            return false
        }
        
        defer {
            url.stopAccessingSecurityScopedResource()
        }
        
        // ブックマークを作成
        do {
            let bookmarkData = try url.bookmarkData(
                options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
                includingResourceValuesForKeys: nil,
                relativeTo: nil
            )
            
            // 既存のブックマークをチェック
            if let existingIndex = bookmarkedDirectories.firstIndex(where: { $0.path == url.path }) {
                // 既存のブックマークを更新
                bookmarkedDirectories[existingIndex] = BookmarkedDirectory(
                    path: url.path,
                    bookmarkData: bookmarkData,
                    lastAccessed: Date()
                )
            } else {
                // 新しいブックマークを追加
                let bookmark = BookmarkedDirectory(
                    path: url.path,
                    bookmarkData: bookmarkData,
                    lastAccessed: Date()
                )
                bookmarkedDirectories.insert(bookmark, at: 0)
                
                // 最大数を超えた場合は古いものを削除
                if bookmarkedDirectories.count > maxBookmarks {
                    bookmarkedDirectories.removeLast()
                }
            }
            
            currentDirectory = url
            saveBookmarks()
            return true
            
        } catch {
            print("Failed to create bookmark: \(error)")
            return false
        }
    }
    
    /// 保存されたブックマークからディレクトリを復元
    func restoreDirectory(from bookmark: BookmarkedDirectory) -> URL? {
        var isStale = false
        
        do {
            let url = try URL(
                resolvingBookmarkData: bookmark.bookmarkData,
                options: .withSecurityScope,
                relativeTo: nil,
                bookmarkDataIsStale: &isStale
            )
            
            if isStale {
                // ブックマークが古い場合は再作成を試みる
                print("Bookmark is stale, attempting to refresh")
                if setDirectory(url) {
                    return url
                }
            }
            
            // アクセス権限を開始
            guard url.startAccessingSecurityScopedResource() else {
                print("Failed to access restored URL")
                return nil
            }
            
            currentDirectory = url
            
            // 最終アクセス日時を更新
            if let index = bookmarkedDirectories.firstIndex(where: { $0.path == bookmark.path }) {
                bookmarkedDirectories[index].lastAccessed = Date()
                saveBookmarks()
            }
            
            return url
            
        } catch {
            print("Failed to restore bookmark: \(error)")
            return nil
        }
    }
    
    /// 現在のディレクトリのアクセスを終了
    func stopAccessingCurrentDirectory() {
        currentDirectory?.stopAccessingSecurityScopedResource()
        currentDirectory = nil
    }
    
    /// ブックマークを削除
    func removeBookmark(_ bookmark: BookmarkedDirectory) {
        bookmarkedDirectories.removeAll { $0.id == bookmark.id }
        saveBookmarks()
    }
    
    /// ブックマークをUserDefaultsに保存
    private func saveBookmarks() {
        let encoder = PropertyListEncoder()
        if let data = try? encoder.encode(bookmarkedDirectories) {
            UserDefaults.standard.set(data, forKey: bookmarkKey)
        }
    }
    
    /// UserDefaultsからブックマークを読み込み
    private func loadBookmarks() {
        guard let data = UserDefaults.standard.data(forKey: bookmarkKey) else { return }
        
        let decoder = PropertyListDecoder()
        if let bookmarks = try? decoder.decode([BookmarkedDirectory].self, from: data) {
            bookmarkedDirectories = bookmarks
        }
    }
}

/// ブックマークされたディレクトリの情報
struct BookmarkedDirectory: Identifiable, Codable {
    let id = UUID()
    let path: String
    let bookmarkData: Data
    var lastAccessed: Date
    
    var name: String {
        URL(fileURLWithPath: path).lastPathComponent
    }
    
    var formattedDate: String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: lastAccessed)
    }
}