import AppKit
import ApplicationServices
import Foundation

guard CommandLine.arguments.count == 2,
      let pid = Int32(CommandLine.arguments[1]),
      let app = NSRunningApplication(processIdentifier: pid),
      app.bundleIdentifier == "tech.anycreative.vp" else { exit(2) }
guard AXIsProcessTrusted() else { print("Accessibility permission unavailable"); exit(3) }
let element = AXUIElementCreateApplication(pid)
var raw: CFTypeRef?
guard AXUIElementCopyAttributeValue(element, kAXWindowsAttribute as CFString, &raw) == .success,
      let windows = raw as? [AXUIElement], let window = windows.first else { exit(4) }
guard AXUIElementCopyAttributeValue(window, kAXCloseButtonAttribute as CFString, &raw) == .success,
      let raw = raw else { exit(5) }
let button = unsafeBitCast(raw, to: AXUIElement.self)
guard AXUIElementPerformAction(button, kAXPressAction as CFString) == .success else { exit(6) }
let deadline = Date().addingTimeInterval(5)
while Date() < deadline {
    RunLoop.current.run(until: Date().addingTimeInterval(0.1))
    if app.isTerminated { print("PASS: GUI terminated after window close"); exit(0) }
}
print("FAIL: GUI remained alive after window close")
exit(1)
