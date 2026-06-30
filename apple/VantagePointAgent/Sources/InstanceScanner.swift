import Foundation

/// 稼働中の VP Process (SP) 1 件。 menu の instance 一覧表示・操作の単位。
struct VpInstance: Identifiable, Sendable, Equatable {
    /// SP の HTTP port (33000〜)
    let port: Int
    /// project ディレクトリ名 (project_dir の末尾)
    let projectName: String
    let pid: Int

    var id: Int { port }
}

/// 稼働中 SP の探索・制御 (旧 daemon tray.rs の scan/stop を Swift menu bar agent に移管)。
///
/// 旧 daemon tray が持っていた「Open WebUI」(`http://localhost:{port}`) は、 SP root が
/// WebUI を出さなくなった (native vp-app へ移行、 旧 browser canvas 撤去) ため移管しない。
/// 有用な scan (`/api/health`) と stop (`/api/shutdown`) のみ引き継ぐ。
enum InstanceControl {
    /// SP の port range。 deterministic port layout (CLAUDE.md): Project = 33000〜。
    /// 余裕を見て 33015 まで並行 probe する (タイムアウト 0.5s なので空き port も安価)。
    static let portRange = 33000...33015

    /// port range を並行に叩いて稼働中 SP を列挙する (`vp ps` 相当)。
    static func scan() async -> [VpInstance] {
        await withTaskGroup(of: VpInstance?.self) { group in
            for port in portRange {
                group.addTask { await probe(port: port) }
            }
            var found: [VpInstance] = []
            for await instance in group {
                if let instance { found.append(instance) }
            }
            return found.sorted { $0.port < $1.port }
        }
    }

    /// 1 port の `/api/health` を叩いて、 200 なら instance を返す。
    private static func probe(port: Int) async -> VpInstance? {
        guard let url = URL(string: "http://[::1]:\(port)/api/health") else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = 0.5

        guard let (data, response) = try? await URLSession.shared.data(for: request),
            let http = response as? HTTPURLResponse, http.statusCode == 200,
            let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        let pid = json["pid"] as? Int ?? 0
        let projectDir = json["project_dir"] as? String ?? ""
        let name = projectDir.split(separator: "/").last.map(String.init) ?? ""

        return VpInstance(port: port, projectName: name.isEmpty ? "unknown" : name, pid: pid)
    }

    /// instance を graceful shutdown する (POST `/api/shutdown` = shutdown_token cancel)。
    static func stop(port: Int) async {
        guard let url = URL(string: "http://[::1]:\(port)/api/shutdown") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 2
        _ = try? await URLSession.shared.data(for: request)
    }
}
