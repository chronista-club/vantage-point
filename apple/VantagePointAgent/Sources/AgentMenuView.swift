import SwiftUI
import UnisonClient

/// menu bar から開く popover の中身。 `AgentModel.status` を描くだけの純粋 View。
struct AgentMenuView: View {
    @ObservedObject var model: AgentModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header

            Divider()

            Button("再接続") {
                Task { await model.connect() }
            }
            .keyboardShortcut("r")

            Button("終了") {
                NSApplication.shared.terminate(nil)
            }
            .keyboardShortcut("q")
        }
        .padding(12)
        .frame(width: 280)
    }

    /// 接続状態ごとの見出し。 M1 のゴール = ここに daemon の serverIdentity が出ること。
    @ViewBuilder
    private var header: some View {
        switch model.status {
        case .connecting:
            Label("daemon に接続中…", systemImage: "circle.dotted")

        case .connected(let id):
            Label("World: \(id.name) v\(id.version)", systemImage: "circle.fill")
                .foregroundStyle(.primary)
            Text("namespace: \(id.namespace)")
                .font(.caption)
                .foregroundStyle(.secondary)
            if !id.channels.isEmpty {
                Text("channels: \(id.channels.joined(separator: ", "))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // M2: CoreMIDI hot-plug を daemon に報告中の device。
            Text("devices: \(model.reportedDevices.count)")
                .font(.caption)
                .foregroundStyle(.secondary)
            if !model.reportedDevices.isEmpty {
                Text(model.reportedDevices.sorted().joined(separator: ", "))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }

        case .failed(let reason):
            Label("接続失敗", systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
            Text(reason)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
    }
}
