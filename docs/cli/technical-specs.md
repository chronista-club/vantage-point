# Vantage Point CLI 技術仕様書

## 概要

このドキュメントは、Vantage Point CLIの技術的な実装詳細、依存関係、ビルドプロセス、およびデプロイメント手順を定義します。

## システム要件

### 最小要件
- **macOS**: 14.0 (Sonoma) 以上
- **Swift**: 6.0 以上
- **Xcode**: 15.0 以上（開発時）
- **メモリ**: 4GB RAM
- **ストレージ**: 100MB の空き容量

### 推奨要件
- **macOS**: 15.0 (Sequoia) 以上
- **メモリ**: 8GB RAM 以上
- **ネットワーク**: 安定したインターネット接続（AI機能使用時）

## Swift Package 構成

### Package.swift

```swift
// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "VantagePointCLI",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(
            name: "vantage",
            targets: ["VantagePointCLI"]
        )
    ],
    dependencies: [
        // コマンドライン引数パーサー
        .package(url: "https://github.com/apple/swift-argument-parser", from: "1.3.0"),
        
        // 既存のClaude統合ライブラリ
        .package(path: "../ClaudeIntegration"),
        
        // 非同期HTTPクライアント
        .package(url: "https://github.com/swift-server/async-http-client", from: "1.19.0"),
        
        // ファイルシステムユーティリティ
        .package(url: "https://github.com/apple/swift-system", from: "1.3.0"),
        
        // JSON処理の高速化
        .package(url: "https://github.com/apple/swift-crypto", from: "3.0.0"),
        
        // ロギング
        .package(url: "https://github.com/apple/swift-log", from: "1.5.0"),
        
        // プログレスバー
        .package(url: "https://github.com/jkandzi/Progress.swift", from: "0.4.0")
    ],
    targets: [
        .executableTarget(
            name: "VantagePointCLI",
            dependencies: [
                .product(name: "ArgumentParser", package: "swift-argument-parser"),
                "ClaudeIntegration",
                .product(name: "AsyncHTTPClient", package: "async-http-client"),
                .product(name: "SystemPackage", package: "swift-system"),
                .product(name: "Crypto", package: "swift-crypto"),
                .product(name: "Logging", package: "swift-log"),
                .product(name: "Progress", package: "Progress.swift"),
                "VantageCore"
            ]
        ),
        .target(
            name: "VantageCore",
            dependencies: [
                "ClaudeIntegration",
                .product(name: "SystemPackage", package: "swift-system"),
                .product(name: "Logging", package: "swift-log")
            ]
        ),
        .testTarget(
            name: "VantagePointCLITests",
            dependencies: ["VantagePointCLI", "VantageCore"]
        ),
        .testTarget(
            name: "VantageCoreTests",
            dependencies: ["VantageCore"]
        )
    ]
)
```

## アーキテクチャ詳細

### レイヤード・アーキテクチャ

```
┌─────────────────────────────────────────┐
│         Presentation Layer              │
│    (ArgumentParser Commands)            │
├─────────────────────────────────────────┤
│         Application Layer               │
│     (Command Handlers, Workflows)       │
├─────────────────────────────────────────┤
│           Domain Layer                  │
│    (Models, Business Logic)             │
├─────────────────────────────────────────┤
│        Infrastructure Layer             │
│  (File I/O, Network, External APIs)     │
└─────────────────────────────────────────┘
```

### 主要コンポーネント

#### 1. Command System
```swift
// Root command structure
@main
struct VantageCLI: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "vantage",
        abstract: "Vantage Point CLI - Spatial computing project management",
        version: "1.0.0",
        subcommands: [
            NewCommand.self,
            OpenCommand.self,
            ListCommand.self,
            ImportCommand.self,
            AssetsCommand.self,
            DevicesCommand.self,
            ConnectCommand.self,
            SyncCommand.self,
            AskCommand.self,
            GenerateCommand.self,
            ConfigCommand.self,
            StatsCommand.self
        ],
        defaultSubcommand: nil
    )
}
```

#### 2. Configuration Management
```swift
// Configuration structure
struct VantageConfig: Codable {
    var defaultProjectPath: String
    var claudeAPIKey: String?
    var colorOutput: Bool
    var verboseLogging: Bool
    var autoSync: Bool
    var devicePreferences: DevicePreferences
    
    static let configPath = FileManager.default
        .homeDirectoryForCurrentUser
        .appendingPathComponent(".vantage/config.json")
}
```

#### 3. Project Model
```swift
// Project data model
struct VantageProject: Codable {
    let id: UUID
    var name: String
    var type: ProjectType
    var createdAt: Date
    var modifiedAt: Date
    var assets: [Asset]
    var settings: ProjectSettings
    
    enum ProjectType: String, Codable {
        case arExperience = "ar-experience"
        case vrApplication = "vr-application"
        case mixedReality = "mixed-reality"
        case custom = "custom"
    }
}
```

## ビルドとデプロイメント

### 開発ビルド

```bash
# デバッグビルド
swift build

# 実行
swift run vantage

# テスト実行
swift test
```

### リリースビルド

```bash
# 最適化されたリリースビルド
swift build -c release

# バイナリの場所
.build/release/vantage

# Universal Binary (Intel + Apple Silicon)
swift build -c release --arch arm64 --arch x86_64
```

### CI/CD パイプライン

```yaml
# .github/workflows/release.yml
name: Release

on:
  push:
    tags:
      - 'v*'

jobs:
  build:
    runs-on: macos-14
    steps:
      - uses: actions/checkout@v4
      
      - name: Setup Swift
        uses: swift-actions/setup-swift@v1
        with:
          swift-version: "6.0"
      
      - name: Build Release
        run: |
          swift build -c release --arch arm64 --arch x86_64
          
      - name: Run Tests
        run: swift test
      
      - name: Package
        run: |
          mkdir -p dist
          cp .build/release/vantage dist/
          tar -czf vantage-${{ github.ref_name }}.tar.gz -C dist vantage
      
      - name: Create Release
        uses: softprops/action-gh-release@v1
        with:
          files: vantage-*.tar.gz
```

### インストール方法

#### 1. 直接インストール
```bash
# ダウンロードと展開
curl -L https://github.com/vantage/cli/releases/latest/download/vantage.tar.gz | tar xz

# バイナリを PATH に移動
sudo mv vantage /usr/local/bin/

# 権限設定
sudo chmod +x /usr/local/bin/vantage
```

#### 2. Homebrew Formula
```ruby
class Vantage < Formula
  desc "Command-line interface for Vantage Point spatial computing platform"
  homepage "https://github.com/vantage/cli"
  url "https://github.com/vantage/cli/releases/download/v1.0.0/vantage-v1.0.0.tar.gz"
  sha256 "..."
  license "MIT"

  depends_on :macos => :sonoma

  def install
    bin.install "vantage"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/vantage --version")
  end
end
```

## セキュリティ

### API キー管理

```swift
// Keychain integration
import Security

class KeychainManager {
    static func saveAPIKey(_ key: String, for service: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: "api-key",
            kSecValueData as String: key.data(using: .utf8)!
        ]
        
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw KeychainError.saveFailed(status)
        }
    }
}
```

### ネットワークセキュリティ

- すべてのAPI通信はHTTPS必須
- Vision ProとのローカルvideはTLS 1.3
- 証明書ピンニングの実装

## パフォーマンス最適化

### 起動時間の最適化

1. **遅延初期化**
   ```swift
   class ConfigManager {
       private static var _shared: ConfigManager?
       static var shared: ConfigManager {
           if _shared == nil {
               _shared = ConfigManager()
           }
           return _shared!
       }
   }
   ```

2. **動的ライブラリの削減**
   - 静的リンクの優先使用
   - 不要な依存関係の削除

### メモリ管理

1. **ストリーミング処理**
   ```swift
   func importLargeAsset(from url: URL) async throws {
       let stream = try FileHandle(forReadingFrom: url)
       defer { try? stream.close() }
       
       while let chunk = try stream.read(upToCount: 1024 * 1024) {
           try await processChunk(chunk)
       }
   }
   ```

2. **自動リリースプール**
   ```swift
   func processBatchAssets(_ assets: [Asset]) async {
       for asset in assets {
           await autoreleasepool {
               await processAsset(asset)
           }
       }
   }
   ```

## テスト戦略

### ユニットテスト

```swift
// Tests/VantageCoreTests/ProjectManagerTests.swift
import XCTest
@testable import VantageCore

final class ProjectManagerTests: XCTestCase {
    func testCreateProject() async throws {
        let manager = ProjectManager()
        let project = try await manager.createProject(
            name: "TestProject",
            type: .arExperience
        )
        
        XCTAssertEqual(project.name, "TestProject")
        XCTAssertEqual(project.type, .arExperience)
    }
}
```

### 統合テスト

```swift
// Tests/VantagePointCLITests/CommandTests.swift
final class CommandTests: XCTestCase {
    func testNewCommand() async throws {
        let output = try await runCommand(["new", "TestProject"])
        XCTAssertTrue(output.contains("Project created"))
    }
}
```

### パフォーマンステスト

```swift
final class PerformanceTests: XCTestCase {
    func testImportPerformance() {
        measure {
            let expectation = expectation(description: "Import")
            Task {
                try await importTestAsset()
                expectation.fulfill()
            }
            wait(for: [expectation], timeout: 10.0)
        }
    }
}
```

## モニタリングとロギング

### ログレベル

```swift
enum LogLevel: String {
    case debug = "DEBUG"
    case info = "INFO"
    case warning = "WARNING"
    case error = "ERROR"
    case critical = "CRITICAL"
}
```

### 構造化ログ

```swift
logger.info("Asset imported",
    metadata: [
        "assetId": .string(asset.id.uuidString),
        "size": .int(asset.size),
        "duration": .double(duration)
    ]
)
```

## 今後の拡張計画

### プラグインシステム

```swift
protocol VantagePlugin {
    static var identifier: String { get }
    static var version: String { get }
    
    func register(to app: inout CommandConfiguration)
    func configure(with config: VantageConfig)
}
```

### スクリプティング API

```swift
// Future: JavaScript/Python bindings
@available(macOS 15.0, *)
class VantageScriptingBridge {
    func expose() -> JSExport {
        // API exposure for scripting
    }
}
```