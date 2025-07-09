import Foundation

/// Claude API クライアント
public actor ClaudeClient {
    /// API設定
    private let configuration: APIConfiguration
    
    /// URLセッション
    private let session: URLSession
    
    /// JSON エンコーダー
    private let encoder = JSONEncoder()
    
    /// JSON デコーダー
    private let decoder = JSONDecoder()
    
    /// クライアントを初期化
    public init(configuration: APIConfiguration) {
        self.configuration = configuration
        
        let sessionConfig = URLSessionConfiguration.default
        sessionConfig.timeoutIntervalForRequest = configuration.timeoutInterval
        self.session = URLSession(configuration: sessionConfig)
    }
    
    /// 便利なイニシャライザ（APIキーのみ）
    public init(apiKey: String) {
        self.init(configuration: APIConfiguration(apiKey: apiKey))
    }
    
    /// メッセージを送信
    public func sendMessage(
        _ messages: [Message],
        model: ClaudeModel? = nil,
        system: String? = nil,
        maxTokens: Int? = nil,
        temperature: Double? = nil
    ) async throws -> ClaudeResponse {
        let request = ClaudeRequest(
            model: (model ?? configuration.defaultModel).rawValue,
            messages: messages,
            system: system,
            maxTokens: maxTokens ?? (model ?? configuration.defaultModel).defaultMaxTokens,
            temperature: temperature,
            stream: false
        )
        
        return try await performRequest(request)
    }
    
    /// ストリーミングメッセージを送信
    public func streamMessage(
        _ messages: [Message],
        model: ClaudeModel? = nil,
        system: String? = nil,
        maxTokens: Int? = nil,
        temperature: Double? = nil
    ) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        let request = ClaudeRequest(
            model: (model ?? configuration.defaultModel).rawValue,
            messages: messages,
            system: system,
            maxTokens: maxTokens ?? (model ?? configuration.defaultModel).defaultMaxTokens,
            temperature: temperature,
            stream: true
        )
        
        return try await performStreamingRequest(request)
    }
    
    /// 単純なテキスト補完
    public func complete(
        prompt: String,
        model: ClaudeModel? = nil,
        system: String? = nil,
        maxTokens: Int? = nil,
        temperature: Double? = nil
    ) async throws -> String {
        let messages = [Message(role: .user, content: prompt)]
        let response = try await sendMessage(
            messages,
            model: model,
            system: system,
            maxTokens: maxTokens,
            temperature: temperature
        )
        return response.text
    }
    
    // MARK: - Private Methods
    
    /// リクエストを実行
    private func performRequest(_ claudeRequest: ClaudeRequest) async throws -> ClaudeResponse {
        guard !configuration.apiKey.isEmpty else {
            throw ClaudeAPIError.missingAPIKey
        }
        
        var urlRequest = URLRequest(url: configuration.messagesEndpoint)
        urlRequest.httpMethod = "POST"
        urlRequest.allHTTPHeaderFields = APIHeaders.headers(for: configuration)
        
        do {
            urlRequest.httpBody = try encoder.encode(claudeRequest)
        } catch {
            throw ClaudeAPIError.decodingError(error)
        }
        
        let (data, response) = try await session.data(for: urlRequest)
        
        guard let httpResponse = response as? HTTPURLResponse else {
            throw ClaudeAPIError.invalidResponse
        }
        
        if httpResponse.statusCode.isSuccessHTTPCode {
            do {
                return try decoder.decode(ClaudeResponse.self, from: data)
            } catch {
                throw ClaudeAPIError.decodingError(error)
            }
        } else {
            // エラーレスポンスの処理
            if let error = try? decoder.decode(ClaudeError.self, from: data) {
                switch httpResponse.statusCode {
                case 429:
                    let retryAfter = httpResponse.value(forHTTPHeaderField: "Retry-After")
                        .flatMap { Double($0) }
                    throw ClaudeAPIError.rateLimited(retryAfter: retryAfter)
                case 400:
                    throw ClaudeAPIError.invalidRequest(error.message)
                case 500...599:
                    throw ClaudeAPIError.serverError(error.message)
                default:
                    throw ClaudeAPIError.httpError(
                        statusCode: httpResponse.statusCode,
                        message: error.message
                    )
                }
            } else {
                throw ClaudeAPIError.httpError(
                    statusCode: httpResponse.statusCode,
                    message: String(data: data, encoding: .utf8)
                )
            }
        }
    }
    
    /// ストリーミングリクエストを実行
    private func performStreamingRequest(_ claudeRequest: ClaudeRequest) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        guard !configuration.apiKey.isEmpty else {
            throw ClaudeAPIError.missingAPIKey
        }
        
        var urlRequest = URLRequest(url: configuration.messagesEndpoint)
        urlRequest.httpMethod = "POST"
        urlRequest.allHTTPHeaderFields = APIHeaders.streamingHeaders(for: configuration)
        
        do {
            urlRequest.httpBody = try encoder.encode(claudeRequest)
        } catch {
            throw ClaudeAPIError.decodingError(error)
        }
        
        let (bytes, response) = try await session.bytes(for: urlRequest)
        
        guard let httpResponse = response as? HTTPURLResponse else {
            throw ClaudeAPIError.invalidResponse
        }
        
        guard httpResponse.statusCode.isSuccessHTTPCode else {
            throw ClaudeAPIError.httpError(
                statusCode: httpResponse.statusCode,
                message: httpResponse.description
            )
        }
        
        return AsyncThrowingStream { continuation in
            Task {
                do {
                    for try await line in bytes.lines {
                        if line.hasPrefix("data: ") {
                            let jsonString = String(line.dropFirst(6))
                            if jsonString == "[DONE]" {
                                continuation.finish()
                                return
                            }
                            
                            guard let data = jsonString.data(using: .utf8) else {
                                continue
                            }
                            
                            // イベントタイプを判定してデコード
                            if let event = try? self.parseStreamEvent(from: data) {
                                continuation.yield(event)
                            }
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
        }
    }
    
    /// ストリームイベントをパース
    private func parseStreamEvent(from data: Data) throws -> StreamEvent? {
        // まず、イベントタイプを判定
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = json["type"] as? String else {
            return nil
        }
        
        switch type {
        case "message_start":
            let event = try decoder.decode(MessageStart.self, from: data)
            return .messageStart(event)
        case "content_block_start":
            let event = try decoder.decode(ContentBlockStart.self, from: data)
            return .contentBlockStart(event)
        case "content_block_delta":
            let event = try decoder.decode(ContentBlockDelta.self, from: data)
            return .contentBlockDelta(event)
        case "content_block_stop":
            let event = try decoder.decode(ContentBlockStop.self, from: data)
            return .contentBlockStop(event)
        case "message_stop":
            let event = try decoder.decode(MessageStop.self, from: data)
            return .messageStop(event)
        case "error":
            let error = try decoder.decode(ClaudeError.self, from: data)
            return .error(error)
        default:
            return nil
        }
    }
}