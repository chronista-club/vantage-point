import Foundation

/// Claude CLI セッション JSONL から session title を取得する reader
///
/// `~/.claude/projects/<project-dir-key>/<uuid>.jsonl` の最新 session から
/// **最初の user message** を抽出、VP Sidebar の Lane 名 L1 に表示する。
///
/// VP-83 refinement 44: Lane row L1 に CC session title 連動。
///
/// 同等の Rust 実装: `crates/vantage-point/src/tui/session.rs`
enum ClaudeSessionReader {

    /// project_dir (`/Users/makoto/repos/vantage-point`) を
    /// Claude projects directory key (`-Users-makoto-repos-vantage-point`) に変換
    static func projectDirToKey(_ dir: String) -> String {
        dir.replacingOccurrences(of: "/", with: "-")
    }

    /// `~/.claude/projects` directory
    static func claudeProjectsDir() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".claude/projects")
    }

    /// project_dir に紐づく最新 session の title を取得
    /// - Parameters:
    ///   - projectDir: 絶対パス (e.g. "/Users/makoto/repos/vantage-point")
    ///   - maxChars: 先頭 n 文字で cap (default 60)
    /// - Returns: 最初の user message (trimmed)、取得失敗は nil
    static func latestSessionTitle(for projectDir: String, maxChars: Int = 60) -> String? {
        let key = projectDirToKey(projectDir)
        let sessionsDir = claudeProjectsDir().appendingPathComponent(key)

        guard FileManager.default.fileExists(atPath: sessionsDir.path) else { return nil }

        let keys: Set<URLResourceKey> = [.contentModificationDateKey]
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: sessionsDir,
            includingPropertiesForKeys: Array(keys),
            options: [.skipsHiddenFiles]
        )) ?? []

        // .jsonl のみ、modified 降順
        let jsonls = contents
            .filter { $0.pathExtension == "jsonl" }
            .filter { (try? $0.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0 > 0 }
            .sorted { l, r in
                let lDate = (try? l.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                let rDate = (try? r.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                return lDate > rDate
            }

        guard let latest = jsonls.first else { return nil }
        return parseFirstUserMessage(at: latest, maxChars: maxChars)
    }

    /// JSONL から最初の `type: user` message の text を抽出
    /// 先頭 256KB だけ読む (長時間 session で file size MB 超える可能性を考慮)
    private static func parseFirstUserMessage(at url: URL, maxChars: Int) -> String? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }

        let data: Data? = {
            if #available(macOS 10.15.4, *) {
                return try? handle.read(upToCount: 256 * 1024)
            } else {
                return handle.readData(ofLength: 256 * 1024)
            }
        }()

        guard let data, let content = String(data: data, encoding: .utf8) else { return nil }

        for line in content.split(separator: "\n").prefix(500) {
            let lineStr = String(line)
            guard lineStr.contains("\"type\":\"user\"") else { continue }
            if let text = extractUserText(from: lineStr) {
                let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmed.isEmpty else { continue }
                return String(trimmed.prefix(maxChars))
            }
        }
        return nil
    }

    /// `{"message": {"content": "..." or [...]}}` から text を extract
    private static func extractUserText(from line: String) -> String? {
        guard let data = line.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let message = json["message"] as? [String: Any]
        else { return nil }

        // content: String
        if let content = message["content"] as? String {
            return content
        }
        // content: [{type: "text", text: "..."}]
        if let array = message["content"] as? [[String: Any]] {
            for item in array {
                if let text = item["text"] as? String, !text.isEmpty {
                    return text
                }
            }
        }
        return nil
    }
}
