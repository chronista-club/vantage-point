# Claude APIの完全ガイド：Swift開発者向け詳細調査

Claude APIは、Anthropic社が提供する最先端のAI言語モデルAPIです。本報告書では、Swift開発者がClaude APIを効果的に実装するために必要な全ての情報を網羅的に調査し、2024-2025年の最新情報に基づいて解説します。特にiOS/macOSアプリケーション開発における実装方法、ベストプラクティス、そして実践的なサンプルコードを含めて詳しく説明します。

## Claude APIの基本的な仕組みと機能の詳細

Claude APIは、RESTful APIアーキテクチャを採用し、HTTPSプロトコルを通じて安全な通信を実現しています。基本URLは`https://api.anthropic.com/v1/`で、JSONフォーマットでデータをやり取りします。認証には`x-api-key`ヘッダーにAPIキーを設定する方式を採用しており、シンプルかつセキュアな実装が可能です。

### 主要機能と特徴

Claude APIは**マルチモーダル対応**を実現しており、テキストだけでなく画像（最大20枚、各3.75MB以下）やPDF（最大5ファイル、各4.5MB以下）の処理が可能です。2025年に入り、**拡張思考モード（Extended Thinking）**が導入され、複雑な推論タスクに対してより深い分析が可能になりました。また、**コード実行機能**によりサンドボックス環境でPythonコードを実行でき、**ウェブ検索機能**で最新情報へのアクセスも可能です。

特筆すべき機能として、**プロンプトキャッシュ**があります。これにより、頻繁に使用されるコンテキストをキャッシュして最大90%のコスト削減と85%のレイテンシー削減を実現できます。さらに、**ツール使用機能**により外部APIとの連携や関数呼び出しが可能で、より高度なアプリケーション開発が実現できます。

### 利用可能なエンドポイント

最も重要なエンドポイントは**Messages API**（`POST /v1/messages`）です。これは会話型アプリケーションの中核となるAPIで、ユーザーとアシスタントの会話ターンを管理し、システムプロンプトの設定、ストリーミングレスポンス、ツール使用などをサポートしています。大量のクエリを効率的に処理する場合は、**Message Batches API**（`POST /v1/batches`）を使用することで、50%のコスト削減が可能です。最大100,000リクエストを1バッチで処理できます。

## SwiftからClaude APIを使用する方法

SwiftでClaude APIを実装する際は、標準ライブラリの`URLSession`を使用してHTTPリクエストを構築します。必要なHTTPヘッダーは3つです：`Content-Type`を`application/json`に、`x-api-key`にAPIキーを、`anthropic-version`にAPIバージョン（例：`2023-06-01`）を設定します。

### 基本的な実装パターン

```swift
import Foundation

struct ClaudeAPIClient {
    private let apiKey: String
    private let baseURL = "https://api.anthropic.com/v1"

    func sendMessage(_ message: String) async throws -> ClaudeResponse {
        let url = URL(string: "\(baseURL)/messages")!
        var request = URLRequest(url: url)

        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue(apiKey, forHTTPHeaderField: "x-api-key")
        request.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")

        let requestBody = [
            "model": "claude-3-5-sonnet-20241022",
            "max_tokens": 1024,
            "messages": [["role": "user", "content": message]]
        ] as [String : Any]

        request.httpBody = try JSONSerialization.data(withJSONObject: requestBody)

        let (data, _) = try await URLSession.shared.data(for: request)
        return try JSONDecoder().decode(ClaudeResponse.self, from: data)
    }
}
```

### APIキーの安全な管理

セキュリティの観点から、APIキーは**Keychainに保存**することを強く推奨します。環境変数での管理も一般的ですが、iOSアプリケーションではKeychainが最も安全な選択肢です。定期的なキーローテーション（90日間隔推奨）を実施し、本番環境と開発環境で異なるキーを使用することが重要です。

## 会話の文脈を保持する方法

Claude APIは**stateless設計**を採用しており、各APIコールで完全な会話履歴を送信する必要があります。これは`messages`パラメータを使用して実現します。各メッセージは`role`（user/assistant）と`content`を持つオブジェクトとして構造化されます。

### 効率的な文脈管理

会話履歴は配列形式で保存し、新しいメッセージを追加してAPIコールを行います。Claude APIは**200,000トークン**という大きなコンテキストウィンドウを持ち、最大100,000メッセージまで単一リクエスト内で処理可能です。トークン数の管理には**Token Counting API**を使用し、送信前にトークン数を計算できます。

プロンプトキャッシュを活用することで、頻繁に使用されるコンテキスト（長い指示やドキュメント）をキャッシュし、コストとレイテンシーを大幅に削減できます。最大4つのキャッシュブレークポイントを設定でき、5分間のキャッシュ生存時間があります。

## 開発に役立つベストプラクティス

### プロンプトエンジニアリング

Claude 4向けの最適化では、明確で具体的な指示を提供することが重要です。**XMLタグを使用したコンテンツ区分**により、構造化されたプロンプトを作成できます。複雑な推論タスクには「think」「think hard」「think harder」「ultrathink」といったキーワードで拡張思考モードを活用できます。

### APIコールの最適化

長いレスポンスには**ストリーミング（Server-Sent Events）**を使用し、ユーザー体験を向上させます。大量のリクエストがある場合は**Batch API**を使用することで50%のコスト削減が可能です。また、分析的タスクではtemperatureを0.0に近く、創造的タスクでは1.0に近く設定することで、適切な応答を得られます。

## 料金体系、レート制限、利用可能なモデル

### 最新の料金体系（2024-2025年）

最新のClaude 4シリーズでは、**Claude Opus 4**が$15/百万入力トークン、$75/百万出力トークン、**Claude Sonnet 4**が$3/百万入力トークン、$15/百万出力トークンとなっています。コスト効率を重視する場合は、**Claude 3.5 Haiku**（$0.80/百万入力トークン、$4/百万出力トークン）が最適です。

### レート制限の詳細

標準のTier 1では、各モデルで1分間に50リクエスト（RPM）まで可能です。トークン制限は、例えばClaude 3.5 Sonnetでは1分間に40,000入力トークン（ITPM）、8,000出力トークン（OTPM）となっています。使用量が増加するとより高いTierに移行でき、制限が緩和されます。

### 利用可能なモデルの特徴

**Claude 4シリーズ**は2025年5月にリリースされた最新モデルで、最高性能と持続的な長時間タスク対応が特徴です。**Claude 3.7 Sonnet**は思考モード対応で、瞬時回答と段階的推論の両方が可能です。各モデルは200,000トークンのコンテキストウィンドウを持ち、将来的には100万トークンまで拡張予定です。

## SwiftでのサンプルコードとSDK

### 利用可能なサードパーティSDK

Anthropicは現在Swift用の公式SDKを提供していませんが、優れたサードパーティライブラリが3つ存在します。

**SwiftClaude**（George Lyon作）は、Swift 6対応で優れたSwiftUI統合を提供します。Observableプロトコル対応と@Toolマクロによる関数呼び出し機能が特徴です。**SwiftAnthropic**（James Rochabrun作）は、包括的なAPI対応とストリーミング機能、画像・PDF対応を実現しています。**AnthropicSwiftSDK**（fumito-ito作）は、シンプルなAPI設計でバッチ処理やAWS Bedrock/Vertex AI対応も含まれています。

### 実践的なSwift実装例

```swift
// Codableモデルの定義
struct ClaudeMessage: Codable {
    let role: String
    let content: String
}

struct ClaudeRequest: Codable {
    let model: String
    let maxTokens: Int
    let messages: [ClaudeMessage]
}

struct ClaudeResponse: Codable {
    let id: String
    let content: [ClaudeContent]
    let usage: ClaudeUsage
}

// ストリーミング対応の実装
func streamMessage(_ message: String) async throws -> AsyncThrowingStream<String, Error> {
    AsyncThrowingStream<String, Error> { continuation in
        Task {
            // ストリーミングリクエストの実装
            let (bytes, _) = try await URLSession.shared.bytes(for: request)

            for try await line in bytes.lines {
                if line.hasPrefix("data: ") {
                    let jsonString = String(line.dropFirst(6))
                    // JSONパースとテキスト抽出
                    continuation.yield(extractedText)
                }
            }
            continuation.finish()
        }
    }
}
```

## エラーハンドリングとストリーミングレスポンスの扱い方

### 一般的なエラーコードと対処法

Claude APIは標準的なHTTPステータスコードを使用します。**429エラー（レート制限超過）**には指数バックオフとジッターを使用した再試行ロジックを実装します。**500エラー（内部エラー）**や**529エラー（API過負荷）**の場合は、段階的な使用量増加を推奨します。

### ストリーミングレスポンスの実装

Server-Sent Events（SSE）を使用したストリーミングは、`"stream": true`パラメータで有効化します。イベントタイプには`message_start`、`content_block_delta`、`message_stop`などがあり、リアルタイムでコンテンツを受信できます。長時間のリクエスト（10分超）にはストリーミングまたはBatch APIの使用が推奨されます。

### Swift特有のエラーハンドリング

SwiftではResult型を活用して型安全なエラーハンドリングを実装します。async/awaitパターンを使用することで、非同期処理を簡潔に記述でき、適切なキャンセレーション処理も実装できます。

## 結論

Claude APIは、強力な機能と柔軟性を持つAI言語モデルAPIです。Swift開発者にとって、適切なSDKの選択、セキュアなAPIキー管理、効率的な文脈管理、そして適切なエラーハンドリングの実装が成功の鍵となります。プロンプトキャッシュやBatch APIなどの機能を活用することで、コスト効率的かつ高性能なアプリケーションを開発できます。本報告書の情報を活用し、Claude APIの持つ可能性を最大限に引き出してください。
