import Foundation
import ClaudeIntegration

/// リトライ管理クラス
@MainActor
class RetryManager: ObservableObject {
    /// リトライ設定
    struct RetryConfiguration {
        let maxRetries: Int
        let initialDelay: TimeInterval
        let maxDelay: TimeInterval
        let backoffMultiplier: Double
        
        static let `default` = RetryConfiguration(
            maxRetries: 3,
            initialDelay: 1.0,
            maxDelay: 30.0,
            backoffMultiplier: 2.0
        )
    }
    
    /// リトライ状態
    @Published var isRetrying = false
    @Published var currentRetry = 0
    @Published var nextRetryIn: TimeInterval?
    
    private let configuration: RetryConfiguration
    private var retryTask: Task<Void, Never>?
    
    init(configuration: RetryConfiguration = .default) {
        self.configuration = configuration
    }
    
    /// エラーを考慮してリトライを実行
    func performWithRetry<T: Sendable>(
        operation: @escaping @Sendable () async throws -> T,
        onError: (@Sendable (Error, Int) -> Void)? = nil
    ) async throws -> T {
        currentRetry = 0
        
        while currentRetry <= configuration.maxRetries {
            do {
                isRetrying = currentRetry > 0
                let result = try await operation()
                isRetrying = false
                currentRetry = 0
                return result
            } catch {
                // ClaudeIntegrationErrorの場合、リトライ可能かチェック
                if let apiError = error as? ClaudeIntegrationError {
                    guard apiError.isRetryable else {
                        isRetrying = false
                        throw error
                    }
                    
                    // レート制限の場合は指定された時間待つ
                    if case .rateLimited(let retryAfter) = apiError,
                       let retryAfter = retryAfter {
                        await waitForRetry(delay: retryAfter)
                        currentRetry += 1
                        onError?(error, currentRetry)
                        continue
                    }
                }
                
                // 最大リトライ回数に達した場合
                if currentRetry >= configuration.maxRetries {
                    isRetrying = false
                    throw error
                }
                
                // エクスポネンシャルバックオフで待機
                let delay = calculateDelay(for: currentRetry)
                await waitForRetry(delay: delay)
                
                currentRetry += 1
                onError?(error, currentRetry)
            }
        }
        
        // ここには到達しないはず
        throw ClaudeIntegrationError.invalidResponse
    }
    
    /// キャンセル
    func cancel() {
        retryTask?.cancel()
        retryTask = nil
        isRetrying = false
        currentRetry = 0
        nextRetryIn = nil
    }
    
    // MARK: - Private Methods
    
    /// リトライまでの遅延を計算
    private func calculateDelay(for retryCount: Int) -> TimeInterval {
        let delay = configuration.initialDelay * pow(configuration.backoffMultiplier, Double(retryCount))
        return min(delay, configuration.maxDelay)
    }
    
    /// 指定時間待機
    private func waitForRetry(delay: TimeInterval) async {
        nextRetryIn = delay
        
        // カウントダウンタスク
        retryTask = Task {
            var remaining = delay
            while remaining > 0 && !Task.isCancelled {
                nextRetryIn = remaining
                try? await Task.sleep(nanoseconds: UInt64(100_000_000)) // 0.1秒
                remaining -= 0.1
            }
            nextRetryIn = nil
        }
        
        // 実際の待機
        try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
        
        retryTask?.cancel()
        retryTask = nil
    }
}

/// リトライ可能な操作のラッパー
struct RetryableOperation<T: Sendable> {
    let operation: @Sendable () async throws -> T
    let retryManager: RetryManager
    
    func execute() async throws -> T {
        try await retryManager.performWithRetry(operation: operation)
    }
}