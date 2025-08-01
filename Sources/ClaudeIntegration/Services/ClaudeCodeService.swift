#if os(macOS)
import Foundation
import AppKit

/// Claude Codeアプリケーションとの連携サービス
/// macOSでのみ利用可能
public actor ClaudeCodeService: ClaudeServiceProtocol {
    
    // MARK: - Properties
    
    private let serviceConfiguration: ClaudeServiceConfiguration
    private var workingDirectory: String?
    private var fileContexts: [String] = []
    private let processManager = ClaudeCodeProcessManager()
    
    // MARK: - ClaudeServiceProtocol
    
    public var isAvailable: Bool {
        get async {
            await processManager.isClaudeCodeRunning()
        }
    }
    
    public var configuration: ClaudeServiceConfiguration {
        serviceConfiguration
    }
    
    // MARK: - Initialization
    
    public init(configuration: ClaudeServiceConfiguration) {
        self.serviceConfiguration = configuration
    }
    
    // MARK: - Message Sending
    
    public func sendMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> ClaudeResponse {
        // Claude Codeが起動していることを確認
        guard await isAvailable else {
            throw ClaudeIntegrationError.serviceUnavailable("Claude Code is not running")
        }
        
        // IPC通信でメッセージを送信
        let request = ClaudeCodeRequest(
            messages: messages,
            options: options,
            workingDirectory: options?.workingDirectory ?? workingDirectory,
            fileContexts: (options?.fileContexts ?? []) + fileContexts
        )
        
        return try await processManager.sendRequest(request, defaultModel: serviceConfiguration.defaultModel)
    }
    
    public func streamMessage(
        _ messages: [Message],
        options: MessageOptions?
    ) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        // Claude Codeが起動していることを確認
        guard await isAvailable else {
            throw ClaudeIntegrationError.serviceUnavailable("Claude Code is not running")
        }
        
        // IPC通信でストリーミングリクエストを送信
        let request = ClaudeCodeRequest(
            messages: messages,
            options: options,
            workingDirectory: options?.workingDirectory ?? workingDirectory,
            fileContexts: (options?.fileContexts ?? []) + fileContexts,
            streaming: true
        )
        
        return try await processManager.sendStreamingRequest(request, defaultModel: serviceConfiguration.defaultModel)
    }
    
    // MARK: - Context Management
    
    public func setWorkingDirectory(_ path: String?) async {
        self.workingDirectory = path
        // Claude Codeに作業ディレクトリの変更を通知
        if let path = path {
            await processManager.notifyWorkingDirectoryChange(path)
        }
    }
    
    public func addFileContext(_ paths: [String]) async {
        self.fileContexts.append(contentsOf: paths)
    }
    
    public func checkConnection() async throws {
        guard await isAvailable else {
            throw ClaudeIntegrationError.serviceUnavailable("Claude Code is not running")
        }
        
        // Claude Codeとの接続をテスト
        try await processManager.ping()
    }
}

// MARK: - Claude Code Process Manager

/// Claude Codeプロセスとの通信を管理
private actor ClaudeCodeProcessManager {
    
    private let claudeCodeBundleIdentifier = "com.anthropic.claudecode"
    private let claudeCLIPath = "/Users/mito/.local/share/mise/installs/node/20.19.4/bin/claude"
    private var currentProcess: Process?
    
    /// Claude Codeが実行中かチェック
    func isClaudeCodeRunning() -> Bool {
        // CLIコマンドの存在をチェック
        return FileManager.default.fileExists(atPath: claudeCLIPath)
    }
    
    /// リクエストを送信
    func sendRequest(_ request: ClaudeCodeRequest, defaultModel: ClaudeModel) async throws -> ClaudeResponse {
        // メッセージを結合して単一のプロンプトを作成
        let prompt = formatMessagesAsPrompt(request.messages)
        
        // Claude CLIコマンドを構築
        var arguments = ["-p", prompt]
        
        // 出力フォーマットをJSON
        arguments.append(contentsOf: ["--output-format", "json"])
        
        // 作業ディレクトリを設定
        if let workingDir = request.workingDirectory {
            arguments.append(contentsOf: ["--cwd", workingDir])
        }
        
        // ファイルコンテキストを追加
        for filePath in request.fileContexts {
            arguments.append(contentsOf: ["--file", filePath])
        }
        
        // プロセスを実行
        let result = try await executeClaudeCLI(arguments: arguments)
        
        // JSONレスポンスをパース
        return try parseJSONResponse(result, defaultModel: defaultModel)
    }
    
    /// ストリーミングリクエストを送信
    func sendStreamingRequest(_ request: ClaudeCodeRequest, defaultModel: ClaudeModel) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        AsyncThrowingStream { continuation in
            Task {
                do {
                    // メッセージを結合して単一のプロンプトを作成
                    let prompt = formatMessagesAsPrompt(request.messages)
                    
                    // Claude CLIコマンドを構築
                    var arguments = ["-p", prompt]
                    
                    // 出力フォーマットをストリーミングJSON
                    arguments.append(contentsOf: ["--output-format", "stream-json"])
                    
                    // 作業ディレクトリを設定
                    if let workingDir = request.workingDirectory {
                        arguments.append(contentsOf: ["--cwd", workingDir])
                    }
                    
                    // ファイルコンテキストを追加
                    for filePath in request.fileContexts {
                        arguments.append(contentsOf: ["--file", filePath])
                    }
                    
                    // ストリーミングプロセスを実行
                    try await executeClaudeCLIStreaming(arguments: arguments, defaultModel: defaultModel) { event in
                        continuation.yield(event)
                    }
                    
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
        }
    }
    
    /// 作業ディレクトリの変更を通知
    func notifyWorkingDirectoryChange(_ path: String) async {
        // CLIは各実行時に--cwdで指定するため、特別な処理は不要
    }
    
    /// 接続テスト
    func ping() async throws {
        // claude --versionを実行してCLIが利用可能か確認
        let arguments = ["--version"]
        _ = try await executeClaudeCLI(arguments: arguments)
    }
    
    // MARK: - Private Methods
    
    /// メッセージをCLIプロンプトにフォーマット
    private func formatMessagesAsPrompt(_ messages: [Message]) -> String {
        messages.map { message in
            switch message.role {
            case .system:
                return "System: \(message.content)"
            case .user:
                return "User: \(message.content)"
            case .assistant:
                return "Assistant: \(message.content)"
            }
        }.joined(separator: "\n\n")
    }
    
    /// Claude CLIを実行
    private func executeClaudeCLI(arguments: [String]) async throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: claudeCLIPath)
        process.arguments = arguments
        
        let outputPipe = Pipe()
        let errorPipe = Pipe()
        process.standardOutput = outputPipe
        process.standardError = errorPipe
        
        return try await withCheckedThrowingContinuation { continuation in
            do {
                try process.run()
                process.waitUntilExit()
                
                let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
                let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
                
                if process.terminationStatus != 0 {
                    let errorMessage = String(data: errorData, encoding: .utf8) ?? "Unknown error"
                    continuation.resume(throwing: ClaudeIntegrationError.customError(errorMessage))
                } else {
                    let output = String(data: outputData, encoding: .utf8) ?? ""
                    continuation.resume(returning: output)
                }
            } catch {
                continuation.resume(throwing: error)
            }
        }
    }
    
    /// Claude CLIをストリーミングモードで実行
    private func executeClaudeCLIStreaming(
        arguments: [String],
        defaultModel: ClaudeModel,
        onEvent: @Sendable @escaping (StreamEvent) -> Void
    ) async throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: claudeCLIPath)
        process.arguments = arguments
        
        let outputPipe = Pipe()
        let errorPipe = Pipe()
        process.standardOutput = outputPipe
        process.standardError = errorPipe
        
        // 出力を非同期で読み取る
        let outputHandle = outputPipe.fileHandleForReading
        outputHandle.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            
            if let line = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
               !line.isEmpty {
                // JSONイベントをパース
                Task { [weak self] in
                    if let self = self,
                       let event = await self.parseStreamEvent(from: line, defaultModel: defaultModel) {
                        onEvent(event)
                    }
                }
            }
        }
        
        try process.run()
        process.waitUntilExit()
        
        // ハンドラーをクリーンアップ
        outputHandle.readabilityHandler = nil
        
        if process.terminationStatus != 0 {
            let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
            let errorMessage = String(data: errorData, encoding: .utf8) ?? "Unknown error"
            throw ClaudeIntegrationError.customError(errorMessage)
        }
    }
    
    /// JSONレスポンスをパース
    private func parseJSONResponse(_ jsonString: String, defaultModel: ClaudeModel) throws -> ClaudeResponse {
        guard let data = jsonString.data(using: .utf8) else {
            throw ClaudeIntegrationError.invalidResponse
        }
        
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        
        do {
            let cliResponse = try decoder.decode(CLIResponse.self, from: data)
            
            // CLIレスポンスをClaudeResponseに変換
            let responseContent = ResponseContent(
                type: "text",
                text: cliResponse.content
            )
            
            let usage = Usage(
                inputTokens: cliResponse.usage?.inputTokens ?? 0,
                outputTokens: cliResponse.usage?.outputTokens ?? 0
            )
            
            return ClaudeResponse(
                id: cliResponse.id ?? UUID().uuidString,
                type: "message",
                model: defaultModel.rawValue,
                role: "assistant",
                content: [responseContent],
                stopReason: "end_turn",
                stopSequence: nil,
                usage: usage
            )
        } catch {
            throw ClaudeIntegrationError.decodingError(error)
        }
    }
    
    /// ストリームイベントをパース
    private func parseStreamEvent(from line: String, defaultModel: ClaudeModel) -> StreamEvent? {
        guard let data = line.data(using: .utf8) else { return nil }
        
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        
        do {
            let event = try decoder.decode(CLIStreamEvent.self, from: data)
            
            switch event.type {
            case "content_block_delta":
                if let text = event.delta?.text {
                    let delta = Delta(type: "text_delta", text: text)
                    let blockDelta = ContentBlockDelta(
                        type: "content_block_delta",
                        index: 0,
                        delta: delta
                    )
                    return .contentBlockDelta(blockDelta)
                }
            case "message_start":
                // 簡易的なメッセージ開始イベント
                let partialMessage = PartialMessage(
                    id: UUID().uuidString,
                    type: "message",
                    role: "assistant", 
                    model: defaultModel.rawValue,
                    usage: nil
                )
                let messageStart = MessageStart(
                    type: "message_start",
                    message: partialMessage
                )
                return .messageStart(messageStart)
            case "message_stop":
                let messageStop = MessageStop(type: "message_stop")
                return .messageStop(messageStop)
            default:
                break
            }
        } catch {
            // パースエラーは無視（デバッグログを出力可能）
        }
        
        return nil
    }
}

// MARK: - Claude Code Request

/// Claude Codeへのリクエスト
private struct ClaudeCodeRequest {
    let messages: [Message]
    let options: MessageOptions?
    let workingDirectory: String?
    let fileContexts: [String]
    let streaming: Bool
    
    init(
        messages: [Message],
        options: MessageOptions?,
        workingDirectory: String?,
        fileContexts: [String],
        streaming: Bool = false
    ) {
        self.messages = messages
        self.options = options
        self.workingDirectory = workingDirectory
        self.fileContexts = fileContexts
        self.streaming = streaming
    }
}

// MARK: - CLI Response Types

/// Claude CLIのJSONレスポンス
private struct CLIResponse: Codable {
    let id: String?
    let content: String
    let usage: CLIUsage?
}

/// CLI使用統計
private struct CLIUsage: Codable {
    let inputTokens: Int
    let outputTokens: Int
}

/// CLIストリームイベント
private struct CLIStreamEvent: Codable {
    let type: String
    let delta: CLIDelta?
}

/// CLIストリームデルタ
private struct CLIDelta: Codable {
    let text: String?
}

// MARK: - Custom Errors


#endif // os(macOS)