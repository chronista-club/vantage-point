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
            fileCommands
            navigateCommands
            viewCommands
        }

        // Design Inspector — VP-83 refinement 27 の creo-ui Editor 連携 MVP
        Window("Design Inspector", id: "design-inspector") {
            DesignInspectorView()
        }
        .defaultSize(width: 560, height: 520)
        .windowResizability(.contentSize)
    }

    /// VP メニュー — Command Palette + Design Inspector (macOS 既定 View menu と衝突避け)
    @CommandsBuilder
    private var viewCommands: some Commands {
        CommandMenu("VP") {
            Button("Command Palette…") {
                NotificationCenter.default.post(name: .openCommandPalette, object: nil)
            }
            .keyboardShortcut("k", modifiers: .command)
            Divider()
            DesignInspectorMenuButton()
        }
    }

    /// File メニュー（New Window / Close Window）
    ///
    /// SwiftUI WindowGroup が AppDelegate.setupMainMenu の File menu を上書き
    /// するため、`CommandGroup(replacing: .newItem)` で SwiftUI 側に再宣言する。
    @CommandsBuilder
    private var fileCommands: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Window") {
                // SwiftUI WindowGroup に新ウィンドウ作成を依頼
                // openWindow を直接使えない（@Environment 取得タイミング問題）ため
                // newWindowForTab セレクタを呼ぶ（VP-54 マルチウィンドウ対応経由）
                NSApp.sendAction(#selector(NSResponder.newWindowForTab(_:)), to: nil, from: nil)
                NSApp.activate(ignoringOtherApps: true)
            }
            .keyboardShortcut("n", modifiers: .command)

            Button("Close Window") {
                NSApp.keyWindow?.performClose(nil)
            }
            .keyboardShortcut("w", modifiers: .command)
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

/// @Environment(\.openWindow) を read するための中間 View
/// (`.commands` Scene context では直接 openWindow を使えないため)
private struct DesignInspectorMenuButton: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Design Inspector…") {
            openWindow(id: "design-inspector")
        }
        .keyboardShortcut("i", modifiers: [.command, .shift])
    }
}
