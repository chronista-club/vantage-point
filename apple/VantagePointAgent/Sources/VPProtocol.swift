import Foundation
import UnisonClient

/// VP の `vp-world` protocol（doc 26 §2）の Swift 対応。
///
/// 本来は KDL schema → Swift codegen で生成する想定だが、 codegen が来るまでの手書き定義
/// （doc 26 §5「2 codegen 層」の app 層を当面手書き）。 channel meta + request 型を
/// UnisonClient SDK の `StreamChannelMeta` / `UnisonRequest` に適合させる。
enum VPWorld {
    /// agent → daemon: CoreMIDI hot-plug を報告する stream channel（doc 26 §2 channel "device"）。
    ///
    /// M2 では request（`ReportDevice`）のみ使う一方向。 server → client の push event は
    /// 使わないが、 `StreamChannelMeta` が `Event` を要求するので空の型を置く。
    struct Device: StreamChannelMeta {
        static let name = "device"

        /// device channel は server push event を使わない（M2 では報告のみ）。
        struct Event: Decodable, Sendable {}
    }

    /// `openChannel(VPWorld.device)` 用の meta インスタンス。
    static let device = Device()
}

/// device.report_device request（doc 26 §2 `ReportDevice`）。
///
/// wire は UnisonClient SDK 既定の JSON codec。 daemon の serde（`ReportDeviceRequest`）は
/// snake_case を期待するので、 Swift の camelCase プロパティを `CodingKeys` で snake_case に
/// マップする（`port_name` / `has_input` / `has_output`）。
struct ReportDevice: UnisonRequest {
    static let method = "report_device"

    /// CoreMIDI port の displayName（daemon registry の HashMap key と同値）
    let portName: String
    /// "connected" | "disconnected"
    let state: String
    let hasInput: Bool
    let hasOutput: Bool

    enum CodingKeys: String, CodingKey {
        case portName = "port_name"
        case state
        case hasInput = "has_input"
        case hasOutput = "has_output"
    }

    /// daemon の `handle_device_report` が返す Ack（`{ "ok": true }`）。
    struct Response: Decodable, Sendable {
        let ok: Bool
    }
}
