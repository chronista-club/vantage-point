import ArgumentParser
import Foundation

@main
struct VantageCLI: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "vantage",
        abstract: "Vantage Point CLI - Command-line interface for spatial computing project management",
        version: "0.1.0",
        subcommands: [
            VersionCommand.self,
            ConfigCommand.self,
        ],
        defaultSubcommand: nil,
        helpNames: [.long, .short]
    )
    
    struct Options: ParsableArguments {
        @Flag(name: .shortAndLong, help: "Enable verbose output")
        var verbose = false
        
        @Flag(name: .shortAndLong, help: "Suppress all output except errors")
        var quiet = false
        
        @Option(name: .shortAndLong, help: "Path to custom configuration file")
        var config: String?
        
        @Flag(name: .long, help: "Disable colored output")
        var noColor = false
        
        @Flag(name: .long, help: "Output in JSON format")
        var json = false
    }
}