import ArgumentParser
import Foundation

struct VersionCommand: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "version",
        abstract: "Show the version information"
    )
    
    func run() throws {
        let version = "0.1.0"
        let build = "dev"
        let swiftVersion = "6.0"
        
        print("""
        Vantage Point CLI
        Version: \(version) (Build: \(build))
        Swift: \(swiftVersion)
        Platform: \(getPlatformInfo())
        """)
    }
    
    private func getPlatformInfo() -> String {
        #if os(macOS)
        let osVersion = ProcessInfo.processInfo.operatingSystemVersion
        return "macOS \(osVersion.majorVersion).\(osVersion.minorVersion).\(osVersion.patchVersion)"
        #else
        return "Unknown"
        #endif
    }
}