import SwiftUI

@main
struct VantageMac: App {
    var body: some Scene {
        WindowGroup {
            ContentViewWithConsole()
        }
        .windowResizability(.contentSize)
    }
}
