import AppKit
import SwiftUI

/// VP macOS アプリ — NavigationSplitView + Liquid Glass
///
/// SwiftUI WindowGroup がメインウィンドウを管理し、
/// AppDelegate はステータスバーアイコン + ポップオーバーに専念する。
@main
struct VantagePointApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        WindowGroup {
            MainWindowView()
                .toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        }
        .defaultSize(width: 1200, height: 800)
    }
}
