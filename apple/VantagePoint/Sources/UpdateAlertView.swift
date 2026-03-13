import AppKit
import SwiftUI

/// 更新確認ダイアログビュー - Liquid Glass UIスタイル
struct UpdateAlertView: View {
    @ObservedObject var updateService: UpdateService

    /// 適用中の状態
    @State private var isApplying = false

    /// エラーメッセージ
    @State private var applyError: String?

    /// 更新成功
    @State private var updateSuccess = false

    var body: some View {
        VStack(spacing: 0) {
            // Liquid Glass Header
            headerSection
                .padding(.horizontal, 24)
                .padding(.top, 24)
                .padding(.bottom, 16)

            // Version Info Card
            versionCard
                .padding(.horizontal, 24)

            // Release Notes (if available)
            if let release = updateService.checkResult?.release,
               let body = release.body, !body.isEmpty {
                releaseNotesSection(body: body)
                    .padding(.horizontal, 24)
                    .padding(.top, 12)
            }

            // Status Messages
            statusSection
                .padding(.horizontal, 24)
                .padding(.top, 12)

            Spacer(minLength: 16)

            // Action Buttons
            buttonSection
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
        }
        .frame(width: 420, height: 380)
        .background(.ultraThinMaterial)
    }

    // MARK: - Header Section

    private var headerSection: some View {
        HStack(spacing: 16) {
            // Animated Icon
            ZStack {
                Circle()
                    .fill(.blue.gradient)
                    .frame(width: 56, height: 56)

                Image(systemName: "arrow.down.circle.fill")
                    .font(.system(size: 32, weight: .medium))
                    .foregroundStyle(.white)
            }
            .shadow(color: .blue.opacity(0.3), radius: 8, x: 0, y: 4)

            VStack(alignment: .leading, spacing: 4) {
                Text("Update Available")
                    .font(.system(size: 20, weight: .semibold))

                if let result = updateService.checkResult {
                    Text("Version \(result.latestVersion)")
                        .font(.system(size: 14))
                        .foregroundStyle(.secondary)
                }
            }

            Spacer()
        }
    }

    // MARK: - Version Card

    private var versionCard: some View {
        VStack(spacing: 0) {
            if let result = updateService.checkResult {
                // Current Version Row
                HStack {
                    Label {
                        Text("Current")
                            .foregroundStyle(.secondary)
                    } icon: {
                        Image(systemName: "checkmark.circle")
                            .foregroundStyle(.green)
                    }
                    .font(.system(size: 13))

                    Spacer()

                    Text(result.currentVersion)
                        .font(.system(size: 13, weight: .medium, design: .monospaced))
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)

                Divider()
                    .padding(.horizontal, 16)

                // New Version Row
                HStack {
                    Label {
                        Text("New")
                            .foregroundStyle(.secondary)
                    } icon: {
                        Image(systemName: "sparkles")
                            .foregroundStyle(.blue)
                    }
                    .font(.system(size: 13))

                    Spacer()

                    Text(result.latestVersion)
                        .font(.system(size: 13, weight: .semibold, design: .monospaced))
                        .foregroundStyle(.blue)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(.quaternary, lineWidth: 0.5)
        )
    }

    // MARK: - Release Notes

    private func releaseNotesSection(body: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("What's New", systemImage: "doc.text")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)

            ScrollView {
                Text(body)
                    .font(.system(size: 12))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
            .frame(maxHeight: 120)
            .padding(12)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
            .overlay(
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(.quaternary, lineWidth: 0.5)
            )
        }
    }

    // MARK: - Status Section

    private var statusSection: some View {
        VStack(spacing: 8) {
            // Error message
            if let error = applyError {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Text(error)
                        .font(.system(size: 12))
                        .foregroundStyle(.red)
                }
                .padding(12)
                .frame(maxWidth: .infinity)
                .background(.red.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
            }

            // Success message
            if updateSuccess {
                HStack(spacing: 8) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text("Update applied! Please restart to complete.")
                        .font(.system(size: 12))
                }
                .padding(12)
                .frame(maxWidth: .infinity)
                .background(.green.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }

    // MARK: - Button Section

    private var buttonSection: some View {
        HStack(spacing: 12) {
            // Skip This Version
            Button(
                action: { updateService.skipThisVersion() },
                label: {
                    Text("Skip This Version")
                        .font(.system(size: 13))
                }
            )
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .disabled(isApplying || updateSuccess)

            Spacer()

            // Later
            Button(
                action: { updateService.remindLater() },
                label: {
                    Text("Later")
                        .font(.system(size: 13))
                        .padding(.horizontal, 16)
                        .padding(.vertical, 8)
                }
            )
            .buttonStyle(.plain)
            .background(.regularMaterial, in: Capsule())
            .disabled(isApplying || updateSuccess)

            // Update Now / Restart
            Button(
                action: { applyUpdate() },
                label: {
                    HStack(spacing: 6) {
                        if isApplying {
                            ProgressView()
                                .scaleEffect(0.6)
                                .frame(width: 14, height: 14)
                        }
                        Text(updateSuccess ? "Restart Now" : (isApplying ? "Updating..." : "Update Now"))
                            .font(.system(size: 13, weight: .medium))
                    }
                    .padding(.horizontal, 20)
                    .padding(.vertical, 8)
                }
            )
            .buttonStyle(.plain)
            .foregroundStyle(.white)
            .background(
                Capsule()
                    .fill(.blue.gradient)
                    .shadow(color: .blue.opacity(0.3), radius: 4, x: 0, y: 2)
            )
            .disabled(isApplying)
        }
    }

    // MARK: - Actions

    private func applyUpdate() {
        if updateSuccess {
            performRestart()
            return
        }

        isApplying = true
        applyError = nil

        Task {
            let success = await updateService.applyUpdate()

            await MainActor.run {
                isApplying = false
                if success {
                    updateSuccess = true
                } else {
                    applyError = updateService.errorMessage ?? "Update failed"
                }
            }
        }
    }

    private func performRestart() {
        isApplying = true

        Task {
            // TheWorldを再起動（vpバイナリが更新されている場合）
            _ = await updateService.restartTheWorld(delay: 2)

            await MainActor.run {
                // VantagePoint.appを再起動
                updateService.restartSelf()
            }
        }
    }
}

// MARK: - Window Controller

/// 更新ダイアログウィンドウコントローラー - Liquid Glass スタイル
@MainActor
class UpdateWindowController {
    private var window: NSWindow?
    private var hostingController: NSHostingController<UpdateAlertView>?

    func show(updateService: UpdateService) {
        // Close existing window if any
        window?.close()

        let contentView = UpdateAlertView(updateService: updateService)
        hostingController = NSHostingController(rootView: contentView)

        // Liquid Glass スタイルのウィンドウ
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 380),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )

        window.title = ""
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.contentViewController = hostingController
        window.center()
        window.isReleasedWhenClosed = false
        window.level = .floating
        window.isMovableByWindowBackground = true

        // 透明な背景
        window.backgroundColor = .clear
        window.isOpaque = false

        self.window = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func close() {
        window?.close()
        window = nil
        hostingController = nil
    }
}

// MARK: - Preview

#Preview {
    let client = TheWorldClient()
    let service = UpdateService(client: client)

    return UpdateAlertView(updateService: service)
        .frame(width: 420, height: 380)
}
