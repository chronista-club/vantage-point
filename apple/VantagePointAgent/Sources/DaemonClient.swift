import Foundation
import UnisonClient

/// Mac daemon (TheWorld :32000) への Unison 接続を司る。
///
/// 接続状態 (`Connection`) を所有するので `actor` で thread-safe に閉じ込める
/// (doc 26 §4 が `Connection` を actor にした意図と同じ)。 UI からは `AgentModel`
/// 経由でのみ叩かれ、 main thread には `Connection` を晒さない。
///
/// M1 は connect + identity handshake のみ。 M2 で device stream channel
/// (CoreMIDI 報告) と world subscribe をここに足す。
actor DaemonClient {
    /// 現在の接続。 再接続時は古い接続を畳んでから張り直す。
    private var connection: Connection?

    /// daemon に接続し、 server identity を取得する。
    ///
    /// transport は doc 25 §3.5: `NWProtocolQUIC` (生 QUIC) + ALPN `"unison"` + TLS1.3 +
    /// identity handshake。 これらは `UnisonClient.connect` の内部で完結する。
    /// dev の daemon は `dev_localhost` 自己署名 cert なので loopback では `.skipVerify`。
    func connectAndIdentify() async throws -> ServerIdentity {
        // 既存接続があれば畳む (再接続で leak させない)。
        await connection?.disconnect()

        let connection = try await UnisonClient.connect(
            to: .localDaemon(port: 32000),
            trust: .skipVerify
        )
        self.connection = connection
        return try await connection.serverIdentity()
    }

    /// 接続を明示的に閉じる。
    func disconnect() async {
        await connection?.disconnect()
        connection = nil
    }
}
