import SwiftUI
import CreoUI

/// Design Inspector — VP 内で design token を live 編集 (VP-83 refinement 27)
///
/// creo-ui Editor + SwiftUI 連携の MVP。build/push/run の往復なしに
/// sidebar token を slider で微調整できる。
struct DesignInspectorView: View {
    @Bindable var tokens: DesignTokenStore = .shared

    var body: some View {
        Form {
            Section {
                sliderRow("Header text leading",
                          value: $tokens.sidebarHeaderTextLeading,
                          range: 0...24, step: 1, unit: "pt")
                sliderRow("List row leading inset",
                          value: $tokens.sidebarListRowLeadingInset,
                          range: -40 ... 0, step: 1, unit: "pt")
                sliderRow("List row trailing inset",
                          value: $tokens.sidebarListRowTrailingInset,
                          range: -40 ... 0, step: 1, unit: "pt")
            } header: {
                Text("Sidebar — Layout")
                    .font(.headline)
            }

            Section {
                sliderRow("Card base tint",
                          value: $tokens.sidebarCardBaseOpacity,
                          range: 0.0 ... 0.5, step: 0.01, unit: "")
                sliderRow("Header overlay tint",
                          value: $tokens.sidebarHeaderOverlayOpacity,
                          range: 0.0 ... 0.5, step: 0.01, unit: "")
                sliderRow("Focused Lane tint",
                          value: $tokens.sidebarLaneFocusOpacity,
                          range: 0.0 ... 0.8, step: 0.01, unit: "")
            } header: {
                Text("Sidebar — Tint opacity (緑 semanticSuccess)")
                    .font(.headline)
            }

            Section {
                HStack {
                    Button("Reset to defaults") {
                        tokens.reset()
                    }
                    .buttonStyle(.bordered)

                    Button("Copy JSON") {
                        let json = tokens.exportJSON()
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(json, forType: .string)
                    }
                    .buttonStyle(.bordered)

                    Spacer()
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Design Inspector")
        .frame(minWidth: 520, minHeight: 480)
    }

    @ViewBuilder
    private func sliderRow(
        _ label: String,
        value: Binding<Double>,
        range: ClosedRange<Double>,
        step: Double,
        unit: String
    ) -> some View {
        HStack(spacing: 12) {
            Text(label)
                .frame(width: 180, alignment: .leading)

            Slider(value: value, in: range, step: step)

            Text(formatted(value.wrappedValue, unit: unit))
                .monospacedDigit()
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(Color.colorTextSecondary)
                .frame(width: 56, alignment: .trailing)
        }
    }

    private func formatted(_ v: Double, unit: String) -> String {
        if unit == "pt" {
            return "\(Int(v))\(unit)"
        } else {
            return String(format: "%.2f", v)
        }
    }
}
