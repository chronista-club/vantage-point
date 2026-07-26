import Foundation

/// 稼働中の VP 本体 (daemon) 1 件。 menu の表示・操作の単位。
///
/// doc 44 P1 (fold-in) 以前は project ごとに SP プロセスが居たため複数件あり得たが、
/// fold-in で project は daemon プロセス内の状態になったので **高々 1 件**になった。
struct VpInstance: Identifiable, Sendable, Equatable {
    /// daemon の HTTP port (32000)
    let port: Int
    /// 表示名 (daemon は project に属さないので `"daemon"` 固定)
    let projectName: String
    let pid: Int

    var id: Int { port }
}

/// 稼働中 VP の探索・制御 (旧 daemon tray.rs の scan/stop を Swift menu bar agent に移管)。
///
/// 旧 daemon tray が持っていた「Open WebUI」(`http://localhost:{port}`) は、 root が
/// WebUI を出さなくなった (native vp-app へ移行、 旧 browser canvas 撤去) ため移管しない。
/// 有用な probe (`/api/health`) と stop (`/api/shutdown`) のみ引き継ぐ。
///
/// ## port scan の撤去 (doc 45 §5-5、 2026-07-22)
///
/// 旧実装は SP の port range 33000...33015 を並行 probe していたが、 **SP-portless 化で
/// 33000 番台は誰も listen しなくなった**ため、 この scan は常に空を返す no-op だった
/// (「読み手のない書き込み」の逆 = 答えの返らない問い合わせ)。 daemon 単発 probe に置換。
///
/// `/api/health` と `/api/shutdown` が HTTP のまま残っているのは doc 45 §2 の設計判断
/// (Unison 層が wedge した時に診断手段ごと失わないための、 意図的に鈍い外殻)。
/// この agent はまさにその VP 外の消費者にあたる。
enum InstanceControl {
    /// daemon の HTTP port。 deterministic port layout (CLAUDE.md): daemon = 32000 唯一の listener。
    ///
    /// dev profile (`VP_PROFILE=dev` = 32100) は追わない — この agent は brew 導入の
    /// 常駐 menu bar なので、 見る相手は常に brew namespace の daemon。
    static let worldPort = 32000

    /// 稼働中の VP を列挙する。 fold-in 後は「daemon が居るか居ないか」の 0/1 件。
    static func scan() async -> [VpInstance] {
        guard let daemon = await probe(port: worldPort) else { return [] }
        return [daemon]
    }

    /// `/api/health` を叩いて、 200 なら instance を返す。
    private static func probe(port: Int) async -> VpInstance? {
        guard let url = URL(string: "http://[::1]:\(port)/api/health") else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = 0.5

        guard let (data, response) = try? await URLSession.shared.data(for: request),
            let http = response as? HTTPURLResponse, http.statusCode == 200,
            let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        let pid = json["pid"] as? Int ?? 0
        // fold-in 後の daemon は特定 project に属さないため `project_dir` は空。
        // 旧 SP の health は project_dir を持っていたので、 表示名の導出はここで固定する。
        return VpInstance(port: port, projectName: "daemon", pid: pid)
    }

    /// instance を graceful shutdown する (POST `/api/shutdown` = shutdown_token cancel)。
    ///
    /// ⚠️ fold-in 後は **daemon を止める = 全 project / 全 lane が落ちる** (CLAUDE.md)。
    /// 旧 SP 時代の「1 project だけ止める」意味論はもう無い。
    static func stop(port: Int) async {
        guard let url = URL(string: "http://[::1]:\(port)/api/shutdown") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 2
        _ = try? await URLSession.shared.data(for: request)
    }
}
