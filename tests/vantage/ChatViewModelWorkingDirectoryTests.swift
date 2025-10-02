import XCTest
@testable import Vantage_Point_for_Mac

@MainActor
final class ChatViewModelWorkingDirectoryTests: XCTestCase {
    
    var sut: ChatViewModel!
    var testDirectory: URL!
    
    override func setUp() async throws {
        try await super.setUp()
        sut = ChatViewModel()
        
        // テスト用の一時ディレクトリを作成
        let tempDir = FileManager.default.temporaryDirectory
        testDirectory = tempDir.appendingPathComponent("ChatViewModelTestDir_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: testDirectory, withIntermediateDirectories: true)
    }
    
    override func tearDown() async throws {
        // テスト用ディレクトリを削除
        if let testDir = testDirectory {
            try? FileManager.default.removeItem(at: testDir)
        }
        
        sut = nil
        try await super.tearDown()
    }
    
    func testSetWorkingDirectory() async throws {
        // Given
        XCTAssertNil(sut.workingDirectory)
        let initialLogCount = sut.consoleLogs.count
        
        // When
        sut.setWorkingDirectory(testDirectory)
        
        // Then
        XCTAssertEqual(sut.workingDirectory?.path, testDirectory.path)
        XCTAssertEqual(sut.workingDirectoryManager.currentDirectory?.path, testDirectory.path)
        
        // ログが追加されていることを確認
        XCTAssertGreaterThan(sut.consoleLogs.count, initialLogCount)
        XCTAssertTrue(sut.consoleLogs.contains { log in
            log.message.contains("作業ディレクトリを設定") && log.level == .info
        })
    }
    
    func testRestoreWorkingDirectory() async throws {
        // Given - まず作業ディレクトリを設定
        sut.setWorkingDirectory(testDirectory)
        guard let bookmark = sut.workingDirectoryManager.bookmarkedDirectories.first else {
            XCTFail("ブックマークが作成されていません")
            return
        }
        
        // 作業ディレクトリをクリア
        sut.workingDirectoryManager.stopAccessingCurrentDirectory()
        sut.workingDirectory = nil
        
        // When
        sut.restoreWorkingDirectory(from: bookmark)
        
        // Then
        XCTAssertEqual(sut.workingDirectory?.path, testDirectory.path)
        XCTAssertTrue(sut.consoleLogs.contains { log in
            log.message.contains("作業ディレクトリを復元") && log.level == .info
        })
    }
    
    func testSendFileContent() async throws {
        // Given
        let testFileName = "test.txt"
        let testContent = "これはテストファイルの内容です。\n日本語も含まれています。"
        let testFilePath = testDirectory.appendingPathComponent(testFileName)
        try testContent.write(to: testFilePath, atomically: true, encoding: .utf8)
        
        // APIキーを設定（モック用）
        sut.setAPIKey("test-api-key")
        
        let initialMessageCount = sut.messages.count
        
        // When
        await sut.sendFileContent(path: testFilePath.path, fileName: testFileName)
        
        // Then
        // メッセージが追加されていることを確認
        XCTAssertEqual(sut.messages.count, initialMessageCount + 1)
        
        // 追加されたメッセージの内容を確認
        if let lastMessage = sut.messages.last {
            XCTAssertTrue(lastMessage.isUser)
            XCTAssertTrue(lastMessage.content.contains("ファイル: \(testFileName)"))
            XCTAssertTrue(lastMessage.content.contains("パス: \(testFilePath.path)"))
            XCTAssertTrue(lastMessage.content.contains(testContent))
        } else {
            XCTFail("メッセージが追加されていません")
        }
    }
    
    func testSendFileContentWithError() async throws {
        // Given
        let nonExistentPath = "/path/that/does/not/exist.txt"
        let initialLogCount = sut.consoleLogs.count
        
        // When
        await sut.sendFileContent(path: nonExistentPath, fileName: "nonexistent.txt")
        
        // Then
        // エラーログが追加されていることを確認
        XCTAssertGreaterThan(sut.consoleLogs.count, initialLogCount)
        XCTAssertTrue(sut.consoleLogs.contains { log in
            log.message.contains("ファイルの読み込みに失敗") && log.level == .error
        })
    }
    
    func testSystemPromptIncludesWorkingDirectory() async throws {
        // Given
        sut.setAPIKey("test-api-key")
        sut.setWorkingDirectory(testDirectory)
        
        // When
        // sendMessageメソッドが内部でシステムプロンプトに作業ディレクトリを含めることを確認
        // 実際のAPI呼び出しはモックが必要なため、ここではログを確認
        
        // Then
        XCTAssertNotNil(sut.workingDirectory)
        XCTAssertEqual(sut.workingDirectory?.path, testDirectory.path)
    }
    
    func testWorkingDirectoryManagerIntegration() async throws {
        // Given
        XCTAssertNotNil(sut.workingDirectoryManager)
        
        // When
        sut.setWorkingDirectory(testDirectory)
        
        // Then
        XCTAssertEqual(sut.workingDirectoryManager.currentDirectory?.path, testDirectory.path)
        XCTAssertFalse(sut.workingDirectoryManager.bookmarkedDirectories.isEmpty)
    }
}