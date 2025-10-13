import ArgumentParser
import Foundation
import VantageCore

struct ConfigCommand: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "config",
        abstract: "Manage CLI configuration",
        subcommands: [
            GetCommand.self,
            SetCommand.self,
            ListCommand.self,
            ResetCommand.self
        ],
        defaultSubcommand: ListCommand.self
    )
}

// MARK: - Subcommands

extension ConfigCommand {
    struct GetCommand: AsyncParsableCommand {
        static let configuration = CommandConfiguration(
            commandName: "get",
            abstract: "Get a configuration value"
        )
        
        @Argument(help: "The configuration key to retrieve")
        var key: String
        
        func run() async throws {
            let configManager = ConfigManager.shared
            
            if let value = configManager.getStringValue(for: key) {
                print("\(value)")
            } else {
                throw ValidationError("Unknown configuration key: '\(key)'")
            }
        }
    }
    
    struct SetCommand: AsyncParsableCommand {
        static let configuration = CommandConfiguration(
            commandName: "set",
            abstract: "Set a configuration value"
        )
        
        @Argument(help: "The configuration key")
        var key: String
        
        @Argument(help: "The value to set")
        var value: String
        
        func run() async throws {
            let configManager = ConfigManager.shared
            let logger = ConsoleLogger()
            
            do {
                try configManager.setValue(value, for: key)
                logger.success("Configuration updated: \(key) = \(value)")
            } catch {
                logger.error("Failed to set configuration: \(error.localizedDescription)")
                throw error
            }
        }
    }
    
    struct ListCommand: AsyncParsableCommand {
        static let configuration = CommandConfiguration(
            commandName: "list",
            abstract: "List all configuration values"
        )
        
        func run() async throws {
            let configManager = ConfigManager.shared
            let config = configManager.getConfig()
            let logger = ConsoleLogger()
            
            logger.info("Configuration:")
            logger.info("  defaultProjectPath: \(config.defaultProjectPath)")
            logger.info("  colorOutput: \(config.colorOutput)")
            logger.info("  verboseLogging: \(config.verboseLogging)")
            logger.info("  autoSync: \(config.autoSync)")
            
            if let apiKey = config.claudeAPIKey {
                let maskedKey = String(apiKey.prefix(7)) + "..." + String(apiKey.suffix(4))
                logger.info("  claudeAPIKey: \(maskedKey)")
            } else {
                logger.info("  claudeAPIKey: <not set>")
            }
        }
    }
    
    struct ResetCommand: AsyncParsableCommand {
        static let configuration = CommandConfiguration(
            commandName: "reset",
            abstract: "Reset configuration to defaults"
        )
        
        @Flag(name: .long, help: "Force reset without confirmation")
        var force = false
        
        func run() async throws {
            let logger = ConsoleLogger()
            
            if !force {
                logger.warning("Are you sure you want to reset all configuration? Use --force to confirm.")
                return
            }
            
            let configManager = ConfigManager.shared
            do {
                try configManager.reset()
                logger.success("Configuration reset to defaults.")
            } catch {
                logger.error("Failed to reset configuration: \(error.localizedDescription)")
                throw error
            }
        }
    }
}