import SwiftUI
import CreoUI

/// Command Palette — ⌘K fuzzy search & action launcher (Tab Bar T6)
///
/// VP IDE 化の最大 leverage surface。全 Lane / action を横断検索で即実行。
///
/// ## item source
/// - **Lane**: LaneRegistry.records (project switch、Lane selection 切替)
/// - **Action**: App action (Restart / Inspector / Sidebar toggle 等)
///
/// ## UX
/// - ⌘K で open、ESC で close
/// - TextField に fuzzy query
/// - ↑↓ で item navigate、Enter で実行
struct CommandPaletteView: View {
    let laneRegistry: LaneRegistry
    let appActions: [AppAction]
    let onSelectLane: (LaneRecord) -> Void
    let onSelectAction: (AppAction) -> Void
    let onClose: () -> Void

    @State private var query: String = ""
    @State private var selectedIndex: Int = 0
    @FocusState private var inputFocused: Bool

    private var items: [PaletteItem] {
        // Lanes + Actions を fuzzy filter、Lane 優先
        let lanes = laneRegistry.records.filter { matches(query, record: $0) }
            .map { PaletteItem.lane($0) }
        let actions = appActions.filter { matches(query, action: $0) }
            .map { PaletteItem.action($0) }
        return lanes + actions
    }

    var body: some View {
        VStack(spacing: 0) {
            // Input field
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 14))
                    .foregroundStyle(Color.colorTextSecondary)
                TextField("Lane, action, or memory…", text: $query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 14))
                    .focused($inputFocused)
                    .onSubmit { activate() }
                    .onChange(of: query) { _, _ in selectedIndex = 0 }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)

            Divider()

            // Item list
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                        PaletteItemRow(item: item, isSelected: index == selectedIndex)
                            .contentShape(Rectangle())
                            .onTapGesture {
                                selectedIndex = index
                                activate()
                            }
                    }

                    if items.isEmpty {
                        Text("No matches")
                            .font(.system(size: 12))
                            .foregroundStyle(Color.colorTextTertiary)
                            .padding(.vertical, 20)
                    }
                }
            }
            .frame(maxHeight: 360)
        }
        .frame(width: 520)
        .background(Color.colorSurfaceSurface)
        .overlay(
            RoundedRectangle(cornerRadius: CreoUITokens.radiusMd)
                .stroke(Color.colorSurfaceBorder, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: CreoUITokens.radiusMd))
        .shadow(color: .black.opacity(0.3), radius: 20, y: 10)
        .onAppear {
            inputFocused = true
            selectedIndex = 0
        }
        .onKeyPress(.upArrow) {
            selectedIndex = max(0, selectedIndex - 1)
            return .handled
        }
        .onKeyPress(.downArrow) {
            selectedIndex = min(items.count - 1, selectedIndex + 1)
            return .handled
        }
        .onKeyPress(.escape) {
            onClose()
            return .handled
        }
    }

    private func activate() {
        guard selectedIndex >= 0 && selectedIndex < items.count else { return }
        switch items[selectedIndex] {
        case .lane(let record):
            onSelectLane(record)
        case .action(let action):
            onSelectAction(action)
        }
        onClose()
    }

    /// fuzzy match: 小文字変換 + 全 word 含有
    private func matches(_ q: String, record: LaneRecord) -> Bool {
        guard !q.isEmpty else { return true }
        let haystack = [
            record.address,
            record.projectName,
            record.ccSessionTitle ?? "",
            record.branch ?? ""
        ].joined(separator: " ").lowercased()
        return q.lowercased().split(separator: " ").allSatisfy { frag in
            haystack.contains(frag)
        }
    }

    private func matches(_ q: String, action: AppAction) -> Bool {
        guard !q.isEmpty else { return true }
        let haystack = action.title.lowercased()
        return q.lowercased().split(separator: " ").allSatisfy { frag in
            haystack.contains(frag)
        }
    }
}

// MARK: - Item types

enum PaletteItem {
    case lane(LaneRecord)
    case action(AppAction)
}

struct AppAction: Identifiable {
    let id: String
    let title: String
    let systemImage: String
    let keyEquivalent: String?
}

// MARK: - Row

private struct PaletteItemRow: View {
    let item: PaletteItem
    let isSelected: Bool

    var body: some View {
        HStack(spacing: 10) {
            icon
                .font(.system(size: 14))
                .foregroundStyle(iconColor)
                .frame(width: 18)

            VStack(alignment: .leading, spacing: 2) {
                Text(primary)
                    .font(.system(size: 13, weight: isSelected ? .semibold : .regular))
                    .foregroundStyle(Color.colorTextPrimary)
                    .lineLimit(1)
                if let sub = secondary {
                    Text(sub)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.colorTextTertiary)
                        .lineLimit(1)
                }
            }

            Spacer()

            if let key = trailingKey {
                Text(key)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Color.colorTextTertiary)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(isSelected ? Color.colorSurfaceBgEmphasis.opacity(0.5) : Color.clear)
    }

    private var icon: some View {
        Image(systemName: iconName)
    }

    private var iconName: String {
        switch item {
        case .lane(let r): r.kind == .lead ? "text.book.closed" : "arrow.branch"
        case .action(let a): a.systemImage
        }
    }

    private var iconColor: Color {
        switch item {
        case .lane(let r): r.status.color
        case .action: Color.colorTextSecondary
        }
    }

    private var primary: String {
        switch item {
        case .lane(let r):
            let laneName = r.kind == .lead ? "Lead" : (r.path as NSString).lastPathComponent
            return "\(r.projectName) › \(r.ccSessionTitle ?? laneName)"
        case .action(let a):
            return a.title
        }
    }

    private var secondary: String? {
        switch item {
        case .lane(let r): r.address
        case .action: nil
        }
    }

    private var trailingKey: String? {
        switch item {
        case .lane: nil
        case .action(let a): a.keyEquivalent
        }
    }
}
