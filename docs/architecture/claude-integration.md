# Claude AI統合アーキテクチャ

## 概要

VantageのClaude AI統合は、プラットフォーム適応型の設計により、各環境で最適な連携方法を提供します。macOSではClaude Codeアプリケーションとの高度な連携を、visionOSではAPI直接呼び出しによる軽量な実装を実現しています。

## アーキテクチャ設計

### レイヤー構造

```
┌─────────────────────────────────────┐
│        Application Layer            │
│    (VantageApp / ContentView)       │
├─────────────────────────────────────┤
│      Service Abstraction Layer      │
│     (ClaudeServiceProtocol)         │
├─────────────────────────────────────┤
│    Platform Implementation Layer    │
│ ┌─────────────┐ ┌─────────────────┐ │
│ │ClaudeCode   │ │  ClaudeAPI      │ │
│ │Service(Mac) │ │Service(Vision)  │ │
│ └─────────────┘ └─────────────────┘ │
├─────────────────────────────────────┤
│        Infrastructure Layer         │
│   (Keychain, Network, IPC, etc.)   │
└─────────────────────────────────────┘
```

### コアコンポーネント

#### 1. ClaudeServiceProtocol
```swift
public protocol ClaudeServiceProtocol: Actor {
    var isAvailable: Bool { get async }
    var configuration: ClaudeServiceConfiguration { get async }
    
    func sendMessage(_ messages: [Message], options: MessageOptions?) async throws -> ClaudeResponse
    func streamMessage(_ messages: [Message], options: MessageOptions?) async throws -> AsyncThrowingStream<StreamEvent, Error>
    func setWorkingDirectory(_ path: String?) async
    func addFileContext(_ paths: [String]) async
    func checkConnection() async throws
}
```

#### 2. ClaudeServiceFactory
プラットフォームと設定に基づいて適切なサービス実装を選択

```swift
public static func createDefault() async throws -> any ClaudeServiceProtocol {
    #if os(macOS)
    // Claude Code連携を試行、失敗時はAPIにフォールバック
    #else
    // visionOSでは常にAPI実装を使用
    #endif
}
```

## プラットフォーム別実装

### macOS: Claude Code連携

#### 特徴
- プロセス間通信（IPC）による高度な連携
- ファイルシステムの直接操作
- プロジェクトコンテキストの共有
- リアルタイムのコード同期

#### 実装方式
1. **XPC Services** - セキュアなプロセス間通信
2. **Distributed Notifications** - 状態変更の通知
3. **Shared Container** - データ共有（App Groups）

#### 利点
- Claude Codeの全機能を活用可能
- ローカルファイル操作の統合
- プロジェクト全体のコンテキスト理解

### visionOS: API直接呼び出し

#### 特徴
- HTTPSによるRESTful API通信
- 軽量で独立した実装
- ネットワーク依存型
- ストリーミングレスポンス対応

#### 実装方式
1. **URLSession** - ネットワーク通信
2. **Keychain** - APIキーの安全な保存
3. **AsyncStream** - ストリーミング処理

#### 利点
- 最小限の依存関係
- 予測可能なレスポンス
- スケーラブルな設計

## セキュリティ考慮事項

### APIキー管理
```swift
// Keychainによる安全な保存
let keychain = KeychainManager()
try await keychain.saveAPIKey(apiKey)

// 環境変数からの読み込み（開発時）
if let apiKey = ProcessInfo.processInfo.environment["CLAUDE_API_KEY"] {
    // 使用
}
```

### 通信セキュリティ
- HTTPS通信の強制
- 証明書ピンニング（オプション）
- リクエスト署名の検証

## エラーハンドリング

### 階層的エラー処理
```swift
enum ClaudeIntegrationError: Error {
    case networkError(Error)
    case serviceUnavailable(String)
    case rateLimited(retryAfter: TimeInterval?)
    case invalidRequest(String)
    case customError(String)
}
```

### リトライ戦略
- 指数バックオフによる自動リトライ
- レート制限の尊重
- フォールバック機構（Claude Code → API）

## パフォーマンス最適化

### キャッシング戦略
1. **プロンプトキャッシュ** - 頻繁に使用されるコンテキストの再利用
2. **レスポンスキャッシュ** - 同一リクエストの結果保存
3. **トークン最適化** - 不要なコンテキストの削除

### 並行処理
```swift
// Actor による安全な並行処理
actor ClaudeAPIService: ClaudeServiceProtocol {
    // スレッドセーフな実装
}
```

## 監視とロギング

### ロギングシステム
```swift
protocol LoggingDelegate: AnyObject {
    func log(level: LogLevel, message: String, context: [String: Any]?)
}
```

### メトリクス収集
- レスポンスタイム
- エラー率
- トークン使用量
- API呼び出し頻度

## 将来の拡張

### 計画中の機能
1. **マルチモーダル対応** - 画像・音声入力のサポート
2. **カスタムツール** - 外部ツールとの連携
3. **ローカルモデル** - オフライン動作のサポート
4. **プラグインシステム** - サードパーティ拡張