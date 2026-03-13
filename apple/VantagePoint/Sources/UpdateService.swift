import AppKit
import Combine
import Foundation

/// 更新サービス
/// TheWorld Processと連携してアプリケーションの更新を管理
@MainActor
class UpdateService: ObservableObject {
    /// TheWorldClient
    private let client: TheWorldClient

    /// vpバイナリ更新チェック結果
    @Published private(set) var checkResult: UpdateCheckResult?

    /// Macアプリ更新チェック結果
    @Published private(set) var macAppCheckResult: MacAppUpdateCheckResult?

    /// 更新中フラグ
    @Published private(set) var isUpdating: Bool = false

    /// エラーメッセージ
    @Published var errorMessage: String?

    /// 更新ダイアログを表示するか（vpバイナリ）
    @Published var showUpdateDialog: Bool = false

    /// Macアプリ更新ダイアログを表示するか
    @Published var showMacAppUpdateDialog: Bool = false

    /// スキップしたバージョン（vpバイナリ用、UserDefaultsに保存）
    private var skippedVersion: String? {
        get { UserDefaults.standard.string(forKey: "SkippedUpdateVersion") }
        set { UserDefaults.standard.set(newValue, forKey: "SkippedUpdateVersion") }
    }

    /// スキップしたバージョン（Macアプリ用、UserDefaultsに保存）
    private var skippedMacAppVersion: String? {
        get { UserDefaults.standard.string(forKey: "SkippedMacAppUpdateVersion") }
        set { UserDefaults.standard.set(newValue, forKey: "SkippedMacAppUpdateVersion") }
    }

    /// 最後のチェック日時
    private var lastCheckDate: Date? {
        get { UserDefaults.standard.object(forKey: "LastUpdateCheckDate") as? Date }
        set { UserDefaults.standard.set(newValue, forKey: "LastUpdateCheckDate") }
    }

    /// Macアプリバックアップパス（ロールバック用）
    private var macAppBackupPath: String?

    init(client: TheWorldClient) {
        self.client = client
    }

    /// 更新をチェック
    /// - Parameter force: スキップしたバージョンも含めてチェックするか
    func checkForUpdates(force: Bool = false) async {
        do {
            let result = try await client.checkUpdate()
            checkResult = result

            if result.updateAvailable {
                // スキップしたバージョンの場合は表示しない（強制チェック時を除く）
                if !force, let skipped = skippedVersion, skipped == result.latestVersion {
                    print("[UpdateService] Skipped version \(skipped), not showing dialog")
                    return
                }

                showUpdateDialog = true
            }

            lastCheckDate = Date()
        } catch {
            print("[UpdateService] Check failed: \(error)")
            errorMessage = error.localizedDescription
        }
    }

    /// 起動時の自動チェック（1日1回）
    func checkOnLaunchIfNeeded() async {
        // 最後のチェックから24時間以内ならスキップ
        if let lastCheck = lastCheckDate {
            let hoursSinceLastCheck = Date().timeIntervalSince(lastCheck) / 3600
            if hoursSinceLastCheck < 24 {
                print("[UpdateService] Last check was \(Int(hoursSinceLastCheck)) hours ago, skipping")
                return
            }
        }

        // vpバイナリとMacアプリの両方をチェック
        await checkForUpdates()
        await checkForMacAppUpdates()
    }

    // MARK: - Mac App Update

    /// VantagePoint.appの更新をチェック
    /// - Parameter force: スキップしたバージョンも含めてチェックするか
    func checkForMacAppUpdates(force: Bool = false) async {
        do {
            let result = try await client.checkMacUpdate()
            macAppCheckResult = result

            if result.updateAvailable {
                // スキップしたバージョンの場合は表示しない（強制チェック時を除く）
                if !force, let skipped = skippedMacAppVersion, skipped == result.latestVersion {
                    print("[UpdateService] Skipped Mac app version \(skipped), not showing dialog")
                    return
                }

                showMacAppUpdateDialog = true
            }

            lastCheckDate = Date()
        } catch {
            print("[UpdateService] Mac app check failed: \(error)")
            errorMessage = error.localizedDescription
        }
    }

    /// VantagePoint.appの更新を適用
    func applyMacAppUpdate() async -> Bool {
        guard macAppCheckResult?.updateAvailable == true else {
            return false
        }

        isUpdating = true
        defer { isUpdating = false }

        do {
            let result = try await client.applyMacUpdate()

            if result.success {
                print("[UpdateService] Mac app update applied successfully: \(result.message)")
                macAppBackupPath = result.backupPath
                return true
            } else {
                errorMessage = result.message
                return false
            }
        } catch {
            print("[UpdateService] Mac app apply failed: \(error)")
            errorMessage = error.localizedDescription
            return false
        }
    }

    /// VantagePoint.appのこのバージョンをスキップ
    func skipMacAppVersion() {
        if let version = macAppCheckResult?.latestVersion {
            skippedMacAppVersion = version
            print("[UpdateService] Skipped Mac app version \(version)")
        }
        showMacAppUpdateDialog = false
    }

    /// Macアプリ更新ダイアログを後で
    func remindMacAppLater() {
        showMacAppUpdateDialog = false
    }

    /// Macアプリ更新ダイアログを閉じる
    func dismissMacAppDialog() {
        showMacAppUpdateDialog = false
    }

    /// Macアプリのスキップ状態をリセット
    func resetSkippedMacAppVersion() {
        skippedMacAppVersion = nil
    }

    /// VantagePoint.appをロールバック
    func rollbackMacApp() async -> Bool {
        guard let backupPath = macAppBackupPath else {
            errorMessage = "No backup available for rollback"
            return false
        }

        do {
            try await client.rollbackMacApp(backupPath: backupPath)
            print("[UpdateService] Mac app rollback completed")
            macAppBackupPath = nil
            return true
        } catch {
            print("[UpdateService] Mac app rollback failed: \(error)")
            errorMessage = error.localizedDescription
            return false
        }
    }

    /// 更新を適用
    func applyUpdate() async -> Bool {
        guard checkResult?.updateAvailable == true else {
            return false
        }

        isUpdating = true
        defer { isUpdating = false }

        do {
            let result = try await client.applyUpdate()

            if result.success {
                print("[UpdateService] Update applied successfully: \(result.message)")
                return true
            } else {
                errorMessage = result.message
                return false
            }
        } catch {
            print("[UpdateService] Apply failed: \(error)")
            errorMessage = error.localizedDescription
            return false
        }
    }

    /// このバージョンをスキップ
    func skipThisVersion() {
        if let version = checkResult?.latestVersion {
            skippedVersion = version
            print("[UpdateService] Skipped version \(version)")
        }
        showUpdateDialog = false
    }

    /// 後で通知
    func remindLater() {
        showUpdateDialog = false
    }

    /// ダイアログを閉じる
    func dismissDialog() {
        showUpdateDialog = false
    }

    /// スキップ状態をリセット
    func resetSkippedVersion() {
        skippedVersion = nil
    }

    // MARK: - Restart

    /// TheWorld（vpバイナリ）を再起動
    /// - Parameter delay: 遅延秒数（デフォルト: 1秒）
    func restartTheWorld(delay: UInt32 = 1) async -> Bool {
        do {
            let result = try await client.restart(delay: delay)
            print("[UpdateService] TheWorld restart scheduled: \(result.message)")
            return result.success
        } catch {
            print("[UpdateService] TheWorld restart failed: \(error)")
            errorMessage = error.localizedDescription
            return false
        }
    }

    /// VantagePoint.appを再起動
    /// この関数を呼び出すとアプリが終了します
    func restartSelf() {
        let bundleURL = Bundle.main.bundleURL

        // 遅延して再起動するスクリプトをバックグラウンドで実行
        let script = """
        sleep 1
        open "\(bundleURL.path)"
        """

        let task = Process()
        task.launchPath = "/bin/sh"
        task.arguments = ["-c", script]

        // プロセスを切り離して実行
        task.standardInput = FileHandle.nullDevice
        task.standardOutput = FileHandle.nullDevice
        task.standardError = FileHandle.nullDevice

        do {
            try task.run()
            print("[UpdateService] Restart script spawned, terminating app...")

            // 少し待ってからアプリを終了
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
                NSApp.terminate(nil)
            }
        } catch {
            print("[UpdateService] Failed to spawn restart script: \(error)")
            errorMessage = "Failed to restart: \(error.localizedDescription)"
        }
    }

    /// 更新後の完全な再起動フロー
    /// VantagePoint.appとTheWorldの両方を再起動
    func performFullRestart() async {
        // まずTheWorldを再起動（2秒後に起動）
        _ = await restartTheWorld(delay: 2)

        // 次にVantagePoint.appを再起動
        restartSelf()
    }
}

// MARK: - Update State

extension UpdateService {
    /// 更新状態
    enum UpdateState {
        case idle
        case checking
        case available(version: String, releaseNotes: String?)
        case downloading
        case applying
        case completed
        case error(String)
    }
}
