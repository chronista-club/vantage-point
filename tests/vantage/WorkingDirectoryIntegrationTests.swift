import XCTest
@testable import Vantage_Point_for_Mac

@MainActor
final class WorkingDirectoryIntegrationTests: XCTestCase {
    
    var chatViewModel: ChatViewModel!
    var testDirectory: URL!
    
    override func setUp() async throws {
        try await super.setUp()
        chatViewModel = ChatViewModel()
        
        // テスト用ディレクトリを作成
        let tempDir = FileManager.default.temporaryDirectory
        testDirectory = tempDir.appendingPathComponent("IntegrationTest_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: testDirectory, withIntermediateDirectories: true)
        
        // テストファイルを作成
        let testFiles = [
            ("README.md", "# Test Project\n\nThis is a test project."),
            ("main.swift", "import Foundation\n\nprint(\"Hello, World!\")"),
            ("config.json", "{\n  \"name\": \"test\",\n  \"version\": \"1.0.0\"\n}")
        ]
        
        for (filename, content) in testFiles {
            let filePath = testDirectory.appendingPathComponent(filename)
            try content.write(to: filePath, atomically: true, encoding: .utf8)
        }
    }
    
    override func tearDown() async throws {
        // テストディレクトリを削除
        if let testDir = testDirectory {
            try? FileManager.default.removeItem(at: testDir)
        }
        
        // UserDefaultsをクリーンアップ
        UserDefaults.standard.removeObject(forKey: "com.vantage.workingDirectories")
        
        chatViewModel = nil
        try await super.tearDown()
    }
    
    func testCompleteWorkingDirectoryWorkflow() async throws {
        // 1. 作業ディレクトリを設定
        chatViewModel.setWorkingDirectory(testDirectory)
        XCTAssertEqual(chatViewModel.workingDirectory?.path, testDirectory.path)
        
        // 2. ブックマークが作成されていることを確認
        XCTAssertFalse(chatViewModel.workingDirectoryManager.bookmarkedDirectories.isEmpty)
        let bookmark = chatViewModel.workingDirectoryManager.bookmarkedDirectories.first!
        
        // 3. 作業ディレクトリをクリア
        chatViewModel.workingDirectoryManager.stopAccessingCurrentDirectory()
        chatViewModel.workingDirectory = nil
        
        // 4. ブックマークから復元
        chatViewModel.restoreWorkingDirectory(from: bookmark)
        XCTAssertEqual(chatViewModel.workingDirectory?.path, testDirectory.path)
        
        // 5. ファイルの内容を読み込んでメッセージとして送信
        let readmePath = testDirectory.appendingPathComponent("README.md").path
        await chatViewModel.sendFileContent(path: readmePath, fileName: "README.md")
        
        // 6. メッセージが追加されたことを確認
        XCTAssertTrue(chatViewModel.messages.contains { message in
            message.content.contains("# Test Project")
        })
    }
    
    func testWorkingDirectoryPersistenceAcrossInstances() async throws {
        // 1. 最初のインスタンスで作業ディレクトリを設定
        let firstViewModel = ChatViewModel()
        firstViewModel.setWorkingDirectory(testDirectory)
        
        // 2. 新しいインスタンスを作成
        let secondViewModel = ChatViewModel()
        
        // 3. ブックマークが引き継がれていることを確認
        XCTAssertEqual(
            secondViewModel.workingDirectoryManager.bookmarkedDirectories.count,
            firstViewModel.workingDirectoryManager.bookmarkedDirectories.count
        )
        
        // 4. 同じパスのブックマークが存在することを確認
        XCTAssertTrue(secondViewModel.workingDirectoryManager.bookmarkedDirectories.contains { bookmark in
            bookmark.path == testDirectory.path
        })
    }
    
    func testMultipleFileHandling() async throws {
        // Given
        chatViewModel.setAPIKey("test-api-key")
        chatViewModel.setWorkingDirectory(testDirectory)
        
        let files = ["README.md", "main.swift", "config.json"]
        let initialMessageCount = chatViewModel.messages.count
        
        // When - 複数のファイルを順番に送信
        for filename in files {
            let filePath = testDirectory.appendingPathComponent(filename).path
            await chatViewModel.sendFileContent(path: filePath, fileName: filename)
        }
        
        // Then
        XCTAssertEqual(chatViewModel.messages.count, initialMessageCount + files.count)
        
        // 各ファイルの内容が含まれていることを確認
        XCTAssertTrue(chatViewModel.messages.contains { $0.content.contains("# Test Project") })
        XCTAssertTrue(chatViewModel.messages.contains { $0.content.contains("Hello, World!") })
        XCTAssertTrue(chatViewModel.messages.contains { $0.content.contains("\"version\": \"1.0.0\"") })
    }
}

// UI関連のテスト（View構造の検証）
final class WorkingDirectoryUITests: XCTestCase {
    
    func testWorkingDirectoryViewInitialization() {
        // Given
        let viewModel = ChatViewModel()
        
        // When
        let view = WorkingDirectoryView(viewModel: viewModel)
        
        // Then
        XCTAssertNotNil(view)
    }
    
    func testFileBrowserViewInitialization() {
        // Given
        let viewModel = ChatViewModel()
        
        // When
        let view = FileBrowserView(viewModel: viewModel)
        
        // Then
        XCTAssertNotNil(view)
    }
}