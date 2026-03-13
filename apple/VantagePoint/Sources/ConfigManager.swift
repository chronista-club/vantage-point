import Foundation

/// VP config.toml の読み書き
/// パス: ~/.config/vp/config.toml
class ConfigManager {
    static let shared = ConfigManager()

    /// config.toml のパス
    var configPath: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/vp/config.toml")
    }

    /// 設定全体
    struct VpConfig {
        var defaultProjectDir: String?
        var defaultPort: UInt16
        var claudeCliPath: String?
        var projects: [ProjectEntry]

        init() {
            defaultProjectDir = nil
            defaultPort = 33000
            claudeCliPath = nil
            projects = []
        }
    }

    /// プロジェクトエントリ
    struct ProjectEntry: Identifiable, Equatable {
        let id: UUID
        var name: String
        var path: String
        var port: UInt16?

        init(name: String, path: String, port: UInt16? = nil) {
            id = UUID()
            self.name = name
            self.path = path
            self.port = port
        }
    }

    /// config.toml を読み込む
    func load() -> VpConfig {
        guard let content = try? String(contentsOf: configPath, encoding: .utf8) else {
            return VpConfig()
        }
        return parse(content)
    }

    /// config.toml に書き込む
    func save(_ config: VpConfig) throws {
        let content = serialize(config)

        // ディレクトリを作成
        let dir = configPath.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        try content.write(to: configPath, atomically: true, encoding: .utf8)
    }

    // MARK: - TOML Parser (簡易)

    /// パーサーの一時的なプロジェクト状態
    private struct PartialProject {
        var name: String?
        var path: String?
        var port: UInt16?

        mutating func reset() {
            name = nil
            path = nil
            port = nil
        }

        /// 完成した ProjectEntry を返す（name と path が揃っている場合）
        func toEntry() -> ProjectEntry? {
            guard let name, let path else { return nil }
            return ProjectEntry(name: name, path: path, port: port)
        }
    }

    private func parse(_ content: String) -> VpConfig {
        var config = VpConfig()
        var current = PartialProject()
        var inProjects = false

        for line in content.components(separatedBy: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }

            if trimmed == "[[projects]]" {
                if let entry = current.toEntry() { config.projects.append(entry) }
                current.reset()
                inProjects = true
                continue
            }

            if trimmed.hasPrefix("[") {
                if let entry = current.toEntry() { config.projects.append(entry) }
                current.reset()
                inProjects = false
                continue
            }

            guard let eqIndex = trimmed.firstIndex(of: "=") else { continue }
            let key = trimmed[..<eqIndex].trimmingCharacters(in: .whitespaces)
            let value = trimmed[trimmed.index(after: eqIndex)...]
                .trimmingCharacters(in: .whitespaces)

            if inProjects {
                parseProjectField(key: key, value: value, current: &current)
            } else {
                parseGlobalField(key: key, value: value, config: &config)
            }
        }

        if let entry = current.toEntry() { config.projects.append(entry) }
        return config
    }

    private func parseProjectField(key: String, value: String, current: inout PartialProject) {
        switch key {
        case "name": current.name = unquote(value)
        case "path": current.path = unquote(value)
        case "port": current.port = UInt16(value)
        default: break
        }
    }

    private func parseGlobalField(key: String, value: String, config: inout VpConfig) {
        switch key {
        case "default_project_dir":
            config.defaultProjectDir = unquote(value)
        case "default_port":
            if let port = UInt16(value) { config.defaultPort = port }
        case "claude_cli_path":
            config.claudeCliPath = unquote(value)
        default: break
        }
    }

    private func serialize(_ config: VpConfig) -> String {
        var lines: [String] = []

        // グローバル設定
        if let dir = config.defaultProjectDir {
            lines.append("default_project_dir = \"\(dir)\"")
        }
        lines.append("default_port = \(config.defaultPort)")
        if let path = config.claudeCliPath {
            lines.append("claude_cli_path = \"\(path)\"")
        }

        // プロジェクト
        for project in config.projects {
            lines.append("")
            lines.append("[[projects]]")
            lines.append("name = \"\(project.name)\"")
            lines.append("path = \"\(project.path)\"")
            if let port = project.port {
                lines.append("port = \(port)")
            }
        }

        lines.append("") // 末尾改行
        return lines.joined(separator: "\n")
    }

    /// TOML 文字列のクオート除去
    private func unquote(_ value: String) -> String {
        value.trimmingCharacters(in: CharacterSet(charactersIn: "\""))
    }
}
