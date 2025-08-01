# サービスプロトコル仕様

## ClaudeServiceProtocol

プラットフォームに依存しないClaude AI連携のための統一インターフェース。

### プロトコル定義

```swift
public protocol ClaudeServiceProtocol: Actor {
    /// サービスが利用可能かどうか
    var isAvailable: Bool { get async }
    
    /// 現在の設定
    var configuration: ClaudeServiceConfiguration { get async }
    
    /// メッセージを送信
    func sendMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> ClaudeResponse
    
    /// ストリーミングでメッセージを送信
    func streamMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> AsyncThrowingStream<StreamEvent, Error>
    
    /// 作業ディレクトリを設定
    func setWorkingDirectory(_ path: String?) async
    
    /// ファイルコンテキストを追加
    func addFileContext(_ paths: [String]) async
    
    /// サービスの接続状態を確認
    func checkConnection() async throws
}
```

### データ型定義

#### Message

```swift
public struct Message: Codable, Sendable {
    public enum Role: String, Codable {
        case user
        case assistant
        case system
    }
    
    public let role: Role
    public let content: String
    
    public init(role: Role, content: String) {
        self.role = role
        self.content = content
    }
}
```

#### MessageOptions

```swift
public struct MessageOptions: Sendable {
    public let model: ClaudeModel?
    public let system: String?
    public let maxTokens: Int?
    public let temperature: Double?
    public let workingDirectory: String?
    public let fileContexts: [String]?
}
```

#### ClaudeServiceConfiguration

```swift
public struct ClaudeServiceConfiguration: Sendable {
    public enum ConnectionType: String, Sendable {
        case api = "API"
        case claudeCode = "Claude Code"
    }
    
    public let connectionType: ConnectionType
    public let defaultModel: ClaudeModel
    public let apiKey: String?
    public let timeoutInterval: TimeInterval
}
```

## 実装クラス

### ClaudeAPIService

Claude API直接呼び出しの実装。主にvisionOSで使用。

```swift
public actor ClaudeAPIService: ClaudeServiceProtocol {
    private let client: ClaudeClient
    private let apiConfiguration: APIConfiguration
    private var workingDirectory: String?
    private var fileContexts: [String] = []
    
    public init(apiConfiguration: APIConfiguration) {
        self.apiConfiguration = apiConfiguration
        self.client = ClaudeClient(configuration: apiConfiguration)
    }
}
```

#### 特徴
- URLSessionベースのHTTP通信
- Keychainによる安全なAPIキー管理
- ストリーミングレスポンス対応
- 自動リトライ機能

### ClaudeCodeService

macOS専用のClaude Codeアプリケーション連携実装。

```swift
#if os(macOS)
public actor ClaudeCodeService: ClaudeServiceProtocol {
    private let serviceConfiguration: ClaudeServiceConfiguration
    private var workingDirectory: String?
    private var fileContexts: [String] = []
    private let processManager = ClaudeCodeProcessManager()
    
    public init(configuration: ClaudeServiceConfiguration) {
        self.serviceConfiguration = configuration
    }
}
#endif
```

#### 特徴
- XPC Servicesによるプロセス間通信
- ファイルシステムの直接操作
- プロジェクトコンテキストの共有
- Claude Codeの全機能へのアクセス

## ClaudeServiceFactory

プラットフォームと設定に基づいて適切な実装を選択するファクトリークラス。

### 使用方法

```swift
// デフォルト設定で作成
let service = try await ClaudeServiceFactory.createDefault()

// カスタム設定で作成
let config = ClaudeServiceConfiguration(
    connectionType: .api,
    defaultModel: .claude3_haiku,
    apiKey: "your-api-key"
)
let service = try await ClaudeServiceFactory.create(with: config)
```

### プラットフォーム判定ロジック

```swift
public static func createDefault() async throws -> any ClaudeServiceProtocol {
    let configuration = ClaudeServiceConfiguration.platformDefault
    
    switch configuration.connectionType {
    case .api:
        return try await createAPIService(configuration: configuration)
        
    case .claudeCode:
        #if os(macOS)
        return try await createClaudeCodeService(configuration: configuration)
        #else
        // visionOSではAPIにフォールバック
        var apiConfig = configuration
        apiConfig = ClaudeServiceConfiguration(
            connectionType: .api,
            defaultModel: configuration.defaultModel,
            apiKey: configuration.apiKey,
            timeoutInterval: configuration.timeoutInterval
        )
        return try await createAPIService(configuration: apiConfig)
        #endif
    }
}
```

## エラーハンドリング

### ClaudeIntegrationError

```swift
public enum ClaudeIntegrationError: Error, LocalizedError {
    case networkError(Error)
    case invalidResponse
    case httpError(statusCode: Int, message: String?)
    case decodingError(Error)
    case missingAPIKey
    case rateLimited(retryAfter: TimeInterval?)
    case invalidRequest(String)
    case serverError(String)
    case streamingError(String)
    case customError(String)
}
```

### エラー処理の例

```swift
do {
    let response = try await service.sendMessage(messages, options: nil)
    print("Response: \(response.text)")
} catch ClaudeIntegrationError.rateLimited(let retryAfter) {
    if let retryAfter = retryAfter {
        print("Rate limited. Retry after \(retryAfter) seconds")
    }
} catch ClaudeIntegrationError.missingAPIKey {
    print("Please configure your API key")
} catch {
    print("Error: \(error.localizedDescription)")
}
```

## ベストプラクティス

### 1. プラットフォーム適応設計
```swift
// プラットフォームに応じた最適な実装を自動選択
let service = try await ClaudeServiceFactory.createDefault()
```

### 2. エラーハンドリング
```swift
// 詳細なエラー処理でユーザー体験を向上
switch error {
case ClaudeIntegrationError.serviceUnavailable:
    // フォールバック処理
case ClaudeIntegrationError.rateLimited:
    // リトライ処理
default:
    // 一般的なエラー処理
}
```

### 3. コンテキスト管理
```swift
// 作業ディレクトリとファイルコンテキストを設定
await service.setWorkingDirectory("/path/to/project")
await service.addFileContext(["main.swift", "Package.swift"])
```

### 4. 非同期処理
```swift
// Swift Concurrencyを活用した効率的な非同期処理
async let response1 = service.sendMessage(messages1, options: nil)
async let response2 = service.sendMessage(messages2, options: nil)
let responses = try await [response1, response2]
```

## 関連ドキュメント

- [Claude API実装ガイド](./claude-api-guide.md)
- [アーキテクチャ概要](../architecture/overview.md)
- [Claude統合設計](../architecture/claude-integration.md)