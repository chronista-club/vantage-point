import AppKit
import SwiftUI

@main
struct VantagePointApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        // Menu bar only app - no window
        Settings {
            EmptyView()
        }
    }
}
