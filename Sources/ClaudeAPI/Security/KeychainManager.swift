import Foundation
import Security

/// Keychainマネージャー
public actor KeychainManager {
    /// サービス名
    private let service: String
    
    /// アカウント名
    private let account: String
    
    /// アクセスグループ（オプション）
    private let accessGroup: String?
    
    /// Keychainエラー
    public enum KeychainError: Error, LocalizedError {
        case itemNotFound
        case duplicateItem
        case invalidData
        case unhandledError(OSStatus)
        
        public var errorDescription: String? {
            switch self {
            case .itemNotFound:
                return "Keychainにアイテムが見つかりません"
            case .duplicateItem:
                return "Keychainに重複するアイテムが存在します"
            case .invalidData:
                return "無効なデータ形式です"
            case .unhandledError(let status):
                return "Keychainエラー: \(status)"
            }
        }
    }
    
    /// イニシャライザ
    public init(service: String = "com.chronista.vantage", account: String = "claude-api-key", accessGroup: String? = nil) {
        self.service = service
        self.account = account
        self.accessGroup = accessGroup
    }
    
    /// APIキーを保存
    public func saveAPIKey(_ apiKey: String) async throws {
        let data = Data(apiKey.utf8)
        
        // 既存のアイテムを削除
        try? await deleteAPIKey()
        
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock
        ]
        
        #if os(macOS)
        // macOSではアクセス制御を設定
        if let access = createAccessControl() {
            query[kSecAttrAccess as String] = access
        }
        #endif
        
        if let accessGroup = accessGroup {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        
        let status = SecItemAdd(query as CFDictionary, nil)
        
        guard status == errSecSuccess else {
            if status == errSecDuplicateItem {
                throw KeychainError.duplicateItem
            } else {
                throw KeychainError.unhandledError(status)
            }
        }
    }
    
    /// APIキーを読み込み
    public func loadAPIKey() async throws -> String {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        
        if let accessGroup = accessGroup {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        
        var dataTypeRef: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &dataTypeRef)
        
        guard status == errSecSuccess else {
            if status == errSecItemNotFound {
                throw KeychainError.itemNotFound
            } else {
                throw KeychainError.unhandledError(status)
            }
        }
        
        guard let data = dataTypeRef as? Data,
              let apiKey = String(data: data, encoding: .utf8) else {
            throw KeychainError.invalidData
        }
        
        return apiKey
    }
    
    /// APIキーを削除
    public func deleteAPIKey() async throws {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        
        if let accessGroup = accessGroup {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        
        let status = SecItemDelete(query as CFDictionary)
        
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unhandledError(status)
        }
    }
    
    /// APIキーを更新
    public func updateAPIKey(_ apiKey: String) async throws {
        // 削除してから保存
        try await deleteAPIKey()
        try await saveAPIKey(apiKey)
    }
    
    /// APIキーが存在するか確認
    public func hasAPIKey() async -> Bool {
        do {
            _ = try await loadAPIKey()
            return true
        } catch {
            return false
        }
    }
    
    #if os(macOS)
    /// アクセス制御を作成（macOS用）
    private func createAccessControl() -> SecAccess? {
        var access: SecAccess?
        let description = "Vantage Claude API Key" as CFString
        
        // 現在のアプリケーションのみアクセス可能
        var trustedApplications: [SecTrustedApplication] = []
        
        // 現在のアプリケーションを信頼されたアプリケーションとして追加
        var currentApp: SecTrustedApplication?
        if SecTrustedApplicationCreateFromPath(nil, &currentApp) == errSecSuccess,
           let app = currentApp {
            trustedApplications.append(app)
        }
        
        // アクセス制御を作成
        let status = SecAccessCreate(
            description,
            trustedApplications as CFArray,
            &access
        )
        
        return status == errSecSuccess ? access : nil
    }
    #endif
}

// MARK: - Convenience Extensions

public extension KeychainManager {
    /// 共有インスタンス
    static let shared = KeychainManager()
    
    /// APIキーを使用してClaudeClientを作成
    func createClient() async throws -> ClaudeClient {
        let apiKey = try await loadAPIKey()
        return ClaudeClient(apiKey: apiKey)
    }
}