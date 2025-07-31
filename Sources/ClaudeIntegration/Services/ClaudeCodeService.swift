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
        
        return try await processManager.sendRequest(request)
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
        
        return try await processManager.sendStreamingRequest(request)
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
    private var connection: NSXPCConnection?
    
    /// Claude Codeが実行中かチェック
    func isClaudeCodeRunning() -> Bool {
        let runningApps = NSWorkspace.shared.runningApplications
        return runningApps.contains { app in
            app.bundleIdentifier == claudeCodeBundleIdentifier
        }
    }
    
    /// リクエストを送信
    func sendRequest(_ request: ClaudeCodeRequest) async throws -> ClaudeResponse {
        // TODO: 実際のIPC実装
        // 現在は仮実装
        throw ClaudeIntegrationError.notImplemented("Claude Code IPC is not yet implemented")
    }
    
    /// ストリーミングリクエストを送信
    func sendStreamingRequest(_ request: ClaudeCodeRequest) async throws -> AsyncThrowingStream<StreamEvent, Error> {
        // TODO: 実際のIPC実装
        throw ClaudeIntegrationError.notImplemented("Claude Code streaming IPC is not yet implemented")
    }
    
    /// 作業ディレクトリの変更を通知
    func notifyWorkingDirectoryChange(_ path: String) async {
        // TODO: 実装
    }
    
    /// 接続テスト
    func ping() async throws {
        // TODO: 実装
    }
}

// MARK: - Claude Code Request

/// Claude Codeへのリクエスト
private struct ClaudeCodeRequest: Codable {
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

// MARK: - Custom Errors

extension ClaudeIntegrationError {
    static let notImplemented: (String) -> ClaudeIntegrationError = { message in
        .customError("Not implemented: \(message)")
    }
}

#endif // os(macOS)