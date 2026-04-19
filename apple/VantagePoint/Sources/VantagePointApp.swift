import AppKit
import SwiftUI

/// VP macOS アプリ — NavigationSplitView + Liquid Glass
///
/// SwiftUI WindowGroup がメインウィンドウを管理し、
/// AppDelegate はステータスバーアイコン + ポップオーバーに専念する。
///
/// メニューは SwiftUI `.commands{}` で宣言（AppDelegate 経由の `NSApp.mainMenu` は
/// SwiftUI WindowGroup に上書きされて表示されないため）。
@main
struct VantagePointApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        WindowGroup {
            MainWindowView()
                .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        }
        .defaultSize(width: 1200, height: 800)
        .commands {
            navigateCommands
        }
    }

    /// Navigate メニュー（プロジェクト切替 / Lane 切替 / Pane 分割）
    ///
    /// AppDelegate.setupMainMenu の Navigate menu を SwiftUI commands に移行。
    /// 各 button は AppDelegate と同じ NotificationCenter 通知を post する。
    @CommandsBuilder
    private var navigateCommands: some Commands {
        CommandMenu("Navigate") {
            Button("前のプロジェクト") {
                NotificationCenter.default.post(name: .selectPreviousProject, object: nil)
            }
            .keyboardShortcut(.upArrow, modifiers: .command)

            Button("次のプロジェクト") {
                NotificationCenter.default.post(name: .selectNextProject, object: nil)
            }
            .keyboardShortcut(.downArrow, modifiers: .command)

            Divider()

            ForEach(1...9, id: \.self) { i in
                Button("Project \(i)") {
                    NotificationCenter.default.post(
                        name: .selectProjectByNumber,
                        object: nil,
                        userInfo: ["number": i]
                    )
                }
                .keyboardShortcut(KeyEquivalent(Character("\(i)")), modifiers: .command)
            }

            Divider()

            ForEach(1...9, id: \.self) { i in
                Button("Lane \(i)") {
                    NotificationCenter.default.post(
                        name: .selectLaneByNumber,
                        object: nil,
                        userInfo: ["number": i]
                    )
                }
                .keyboardShortcut(KeyEquivalent(Character("\(i)")), modifiers: .control)
            }

            Divider()

            Button("Split Pane") {
                NotificationCenter.default.post(name: .splitTerminalPane, object: nil)
            }
            .keyboardShortcut("d", modifiers: .command)
        }
    }
}
