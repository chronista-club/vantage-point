import XCTest
@testable import Vantage_Point_for_Mac

@MainActor
final class WorkingDirectoryManagerTests: XCTestCase {
    
    var sut: WorkingDirectoryManager!
    var testDirectory: URL!
    
    override func setUp() async throws {
        try await super.setUp()
        sut = WorkingDirectoryManager()
        
        // テスト用の一時ディレクトリを作成
        let tempDir = FileManager.default.temporaryDirectory
        testDirectory = tempDir.appendingPathComponent("VantageTestDir_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: testDirectory, withIntermediateDirectories: true)
    }
    
    override func tearDown() async throws {
        // テスト用ディレクトリを削除
        if let testDir = testDirectory {
            try? FileManager.default.removeItem(at: testDir)
        }
        
        // UserDefaultsをクリーンアップ
        UserDefaults.standard.removeObject(forKey: "com.vantage.workingDirectories")
        
        sut = nil
        try await super.tearDown()
    }
    
    func testSetDirectory() async throws {
        // Given
        XCTAssertNil(sut.currentDirectory)
        XCTAssertTrue(sut.bookmarkedDirectories.isEmpty)
        
        // When
        let result = sut.setDirectory(testDirectory)
        
        // Then
        XCTAssertTrue(result)
        XCTAssertEqual(sut.currentDirectory?.path, testDirectory.path)
        XCTAssertEqual(sut.bookmarkedDirectories.count, 1)
        XCTAssertEqual(sut.bookmarkedDirectories.first?.path, testDirectory.path)
    }
    
    func testSetDirectoryUpdatesExistingBookmark() async throws {
        // Given
        _ = sut.setDirectory(testDirectory)
        let initialBookmarkCount = sut.bookmarkedDirectories.count
        let initialBookmark = sut.bookmarkedDirectories.first
        
        // When
        _ = sut.setDirectory(testDirectory)
        
        // Then
        XCTAssertEqual(sut.bookmarkedDirectories.count, initialBookmarkCount)
        XCTAssertEqual(sut.bookmarkedDirectories.first?.path, initialBookmark?.path)
        XCTAssertGreaterThan(sut.bookmarkedDirectories.first?.lastAccessed ?? Date.distantPast, 
                            initialBookmark?.lastAccessed ?? Date.distantPast)
    }
    
    func testBookmarkLimit() async throws {
        // Given
        let maxBookmarks = 10
        
        // When - 11個のディレクトリを追加
        for i in 0..<11 {
            let dir = testDirectory.appendingPathComponent("subdir\(i)")
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            _ = sut.setDirectory(dir)
        }
        
        // Then
        XCTAssertEqual(sut.bookmarkedDirectories.count, maxBookmarks)
        XCTAssertFalse(sut.bookmarkedDirectories.contains { $0.path.contains("subdir0") })
    }
    
    func testRestoreDirectory() async throws {
        // Given
        _ = sut.setDirectory(testDirectory)
        guard let bookmark = sut.bookmarkedDirectories.first else {
            XCTFail("ブックマークが作成されていません")
            return
        }
        
        // 現在のディレクトリをクリア
        sut.stopAccessingCurrentDirectory()
        XCTAssertNil(sut.currentDirectory)
        
        // When
        let restoredURL = sut.restoreDirectory(from: bookmark)
        
        // Then
        XCTAssertNotNil(restoredURL)
        XCTAssertEqual(restoredURL?.path, testDirectory.path)
        XCTAssertEqual(sut.currentDirectory?.path, testDirectory.path)
    }
    
    func testRemoveBookmark() async throws {
        // Given
        _ = sut.setDirectory(testDirectory)
        guard let bookmark = sut.bookmarkedDirectories.first else {
            XCTFail("ブックマークが作成されていません")
            return
        }
        
        // When
        sut.removeBookmark(bookmark)
        
        // Then
        XCTAssertTrue(sut.bookmarkedDirectories.isEmpty)
    }
    
    func testStopAccessingCurrentDirectory() async throws {
        // Given
        _ = sut.setDirectory(testDirectory)
        XCTAssertNotNil(sut.currentDirectory)
        
        // When
        sut.stopAccessingCurrentDirectory()
        
        // Then
        XCTAssertNil(sut.currentDirectory)
    }
    
    func testBookmarkPersistence() async throws {
        // Given
        _ = sut.setDirectory(testDirectory)
        let originalBookmarkCount = sut.bookmarkedDirectories.count
        
        // When - 新しいインスタンスを作成
        let newManager = WorkingDirectoryManager()
        
        // Then
        XCTAssertEqual(newManager.bookmarkedDirectories.count, originalBookmarkCount)
        XCTAssertEqual(newManager.bookmarkedDirectories.first?.path, testDirectory.path)
    }
}

// BookmarkedDirectoryのテスト
final class BookmarkedDirectoryTests: XCTestCase {
    
    func testBookmarkedDirectoryProperties() {
        // Given
        let path = "/Users/test/Documents/Project"
        let bookmarkData = Data()
        let lastAccessed = Date()
        
        // When
        let bookmark = BookmarkedDirectory(
            path: path,
            bookmarkData: bookmarkData,
            lastAccessed: lastAccessed
        )
        
        // Then
        XCTAssertEqual(bookmark.path, path)
        XCTAssertEqual(bookmark.name, "Project")
        XCTAssertEqual(bookmark.bookmarkData, bookmarkData)
        XCTAssertEqual(bookmark.lastAccessed, lastAccessed)
        XCTAssertFalse(bookmark.formattedDate.isEmpty)
    }
    
    func testBookmarkedDirectoryCodable() throws {
        // Given
        let bookmark = BookmarkedDirectory(
            path: "/test/path",
            bookmarkData: Data([1, 2, 3, 4]),
            lastAccessed: Date()
        )
        
        // When
        let encoder = PropertyListEncoder()
        let data = try encoder.encode(bookmark)
        
        let decoder = PropertyListDecoder()
        let decodedBookmark = try decoder.decode(BookmarkedDirectory.self, from: data)
        
        // Then
        XCTAssertEqual(decodedBookmark.path, bookmark.path)
        XCTAssertEqual(decodedBookmark.bookmarkData, bookmark.bookmarkData)
        XCTAssertEqual(decodedBookmark.lastAccessed.timeIntervalSinceReferenceDate,
                      bookmark.lastAccessed.timeIntervalSinceReferenceDate,
                      accuracy: 0.001)
    }
}