import XCTest

// すべてのテストクラスをまとめて実行するためのテストスイート
final class AllTests: XCTestCase {
    
    func testRunAllTests() {
        // テストクラスの一覧
        let testClasses: [XCTestCase.Type] = [
            WorkingDirectoryManagerTests.self,
            BookmarkedDirectoryTests.self,
            ChatViewModelWorkingDirectoryTests.self,
            FileItemTests.self,
            FileBrowserUtilityTests.self,
            WorkingDirectoryIntegrationTests.self,
            WorkingDirectoryUITests.self
        ]
        
        print("=== Running Working Directory Feature Tests ===")
        print("Total test classes: \(testClasses.count)")
        
        var totalTests = 0
        var passedTests = 0
        var failedTests = 0
        
        for testClass in testClasses {
            print("\nRunning tests in \(String(describing: testClass))...")
            
            // 各テストクラスのテストメソッドを取得
            let methodCount = testClass.defaultTestSuite.testCaseCount
            totalTests += methodCount
            
            print("Found \(methodCount) test methods")
        }
        
        print("\n=== Test Summary ===")
        print("Total test methods found: \(totalTests)")
        print("Note: Run tests through Xcode for actual execution")
    }
    
    func testCompilation() {
        // すべてのテストファイルがコンパイルされることを確認
        XCTAssertNotNil(WorkingDirectoryManagerTests.self)
        XCTAssertNotNil(BookmarkedDirectoryTests.self)
        XCTAssertNotNil(ChatViewModelWorkingDirectoryTests.self)
        XCTAssertNotNil(FileItemTests.self)
        XCTAssertNotNil(FileBrowserUtilityTests.self)
        XCTAssertNotNil(WorkingDirectoryIntegrationTests.self)
        XCTAssertNotNil(WorkingDirectoryUITests.self)
    }
}

// テストカバレッジの概要
extension AllTests {
    
    static func printTestCoverage() {
        print("""
        
        === Test Coverage Summary ===
        
        1. WorkingDirectoryManager Tests:
           - ディレクトリの設定と保存
           - ブックマークの作成と復元
           - ブックマーク数の制限
           - 永続化機能
        
        2. BookmarkedDirectory Tests:
           - プロパティの初期化
           - Codableプロトコルの実装
        
        3. ChatViewModel Working Directory Tests:
           - 作業ディレクトリの設定
           - ブックマークからの復元
           - ファイル内容の送信
           - エラーハンドリング
        
        4. FileItem Tests:
           - ファイルアイテムの初期化
           - アイコンの判定ロジック
           - ファイルサイズのフォーマット
           - ソート機能
        
        5. Integration Tests:
           - 完全なワークフローのテスト
           - 複数ファイルの処理
           - インスタンス間での永続化
        
        6. UI Tests:
           - ビューの初期化
           - 基本的な構造の検証
        
        """)
    }
}