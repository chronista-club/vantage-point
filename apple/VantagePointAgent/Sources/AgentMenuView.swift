import SwiftUI
import UnisonClient

/// menu bar から開く popover の中身。 `AgentModel` の状態を描くだけの純粋 View。
///
/// M2b: 旧 daemon tray の instance 一覧/操作をここに一本化し、 Swift agent を唯一の
/// menu bar 入口にする。
struct AgentMenuView: View {
    @ObservedObject var model: AgentModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header

            Divider()

            instancesSection

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
        .frame(width: 320)
        // menu を開くたびに稼働中 VP (TheWorld) を再 probe する。
        .task { await model.refreshInstances() }
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

    /// M2b: 稼働中 VP の表示 + 停止操作 (旧 daemon tray の instance submenu に相当)。
    /// fold-in 後は World の 0/1 件 (doc 45 §5-5 で port scan を撤去)。
    @ViewBuilder
    private var instancesSection: some View {
        HStack {
            Text("稼働中 VP: \(model.instances.count)")
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
            Button {
                Task { await model.refreshInstances() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .help("再スキャン")
        }

        if model.instances.isEmpty {
            Text("（なし）")
                .font(.caption2)
                .foregroundStyle(.secondary)
        } else {
            ForEach(model.instances) { instance in
                HStack {
                    Text(instance.projectName)
                        .font(.caption)
                    Text(":\(instance.port)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("停止") {
                        Task { await model.stopInstance(port: instance.port) }
                    }
                    .buttonStyle(.borderless)
                    .font(.caption2)
                    .foregroundStyle(.red)
                }
            }
        }
    }
}
