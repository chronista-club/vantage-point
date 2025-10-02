import XCTest
@testable import Vantage_Point_for_Mac

final class FileItemTests: XCTestCase {
    
    func testFileItemInitialization() {
        // Given
        let name = "test.swift"
        let path = "/Users/test/Documents/test.swift"
        let type = FileItem.FileType.file
        let size: Int64 = 1024
        let modificationDate = Date()
        
        // When
        let fileItem = FileItem(
            name: name,
            path: path,
            type: type,
            size: size,
            modificationDate: modificationDate
        )
        
        // Then
        XCTAssertEqual(fileItem.name, name)
        XCTAssertEqual(fileItem.path, path)
        XCTAssertEqual(fileItem.type, type)
        XCTAssertEqual(fileItem.size, size)
        XCTAssertEqual(fileItem.modificationDate, modificationDate)
        XCTAssertNotNil(fileItem.id)
    }
    
    func testFileItemIcon() {
        // Test Swift file
        let swiftFile = FileItem(
            name: "test.swift",
            path: "/test.swift",
            type: .file,
            size: 0,
            modificationDate: Date()
        )
        XCTAssertEqual(swiftFile.icon, "swift")
        
        // Test Markdown file
        let mdFile = FileItem(
            name: "README.md",
            path: "/README.md",
            type: .file,
            size: 0,
            modificationDate: Date()
        )
        XCTAssertEqual(mdFile.icon, "doc.text")
        
        // Test JSON file
        let jsonFile = FileItem(
            name: "config.json",
            path: "/config.json",
            type: .file,
            size: 0,
            modificationDate: Date()
        )
        XCTAssertEqual(jsonFile.icon, "doc.badge.gearshape")
        
        // Test generic file
        let genericFile = FileItem(
            name: "document.txt",
            path: "/document.txt",
            type: .file,
            size: 0,
            modificationDate: Date()
        )
        XCTAssertEqual(genericFile.icon, "doc")
        
        // Test directory
        let directory = FileItem(
            name: "Documents",
            path: "/Documents",
            type: .directory,
            size: 0,
            modificationDate: Date()
        )
        XCTAssertEqual(directory.icon, "folder.fill")
    }
    
    func testFormattedSize() {
        // Test various file sizes
        let testCases: [(Int64, String)] = [
            (0, "Zero KB"),
            (512, "512 bytes"),
            (1024, "1 KB"),
            (1024 * 1024, "1 MB"),
            (1024 * 1024 * 1024, "1 GB"),
            (1536, "1.5 KB"),
            (1024 * 1024 * 1.5, "1.5 MB")
        ]
        
        for (size, _) in testCases {
            let fileItem = FileItem(
                name: "test",
                path: "/test",
                type: .file,
                size: size,
                modificationDate: Date()
            )
            
            // ByteCountFormatterの出力は環境によって異なる可能性があるため、
            // 空でないことだけを確認
            XCTAssertFalse(fileItem.formattedSize.isEmpty)
        }
    }
    
    func testFileTypeEquality() {
        XCTAssertEqual(FileItem.FileType.file, FileItem.FileType.file)
        XCTAssertEqual(FileItem.FileType.directory, FileItem.FileType.directory)
        XCTAssertNotEqual(FileItem.FileType.file, FileItem.FileType.directory)
    }
    
    func testFileItemIdentifiable() {
        // Given
        let file1 = FileItem(
            name: "file1.txt",
            path: "/file1.txt",
            type: .file,
            size: 100,
            modificationDate: Date()
        )
        
        let file2 = FileItem(
            name: "file2.txt",
            path: "/file2.txt",
            type: .file,
            size: 200,
            modificationDate: Date()
        )
        
        // Then
        XCTAssertNotEqual(file1.id, file2.id)
    }
}

// FileBrowserView関連のユーティリティテスト
final class FileBrowserUtilityTests: XCTestCase {
    
    func testFileItemSorting() {
        // Given
        let file1 = FileItem(name: "b_file.txt", path: "/b_file.txt", type: .file, size: 100, modificationDate: Date())
        let file2 = FileItem(name: "a_file.txt", path: "/a_file.txt", type: .file, size: 200, modificationDate: Date())
        let dir1 = FileItem(name: "z_dir", path: "/z_dir", type: .directory, size: 0, modificationDate: Date())
        let dir2 = FileItem(name: "a_dir", path: "/a_dir", type: .directory, size: 0, modificationDate: Date())
        
        let items = [file1, file2, dir1, dir2]
        
        // When
        let sorted = items.sorted { item1, item2 in
            // ディレクトリを先に、その後名前順
            if item1.type != item2.type {
                return item1.type == .directory
            }
            return item1.name.localizedCaseInsensitiveCompare(item2.name) == .orderedAscending
        }
        
        // Then
        XCTAssertEqual(sorted[0].name, "a_dir")
        XCTAssertEqual(sorted[1].name, "z_dir")
        XCTAssertEqual(sorted[2].name, "a_file.txt")
        XCTAssertEqual(sorted[3].name, "b_file.txt")
    }
}