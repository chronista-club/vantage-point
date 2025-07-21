import SwiftUI

/// キーボードショートカット一覧表示ビュー
struct ShortcutHelpView: View {
    let shortcuts: [ShortcutItem] = [
        ShortcutItem(key: "⌘ + Enter", description: "メッセージを送信"),
        ShortcutItem(key: "⌘ + K", description: "入力欄をクリア"),
        ShortcutItem(key: "⌘ + /", description: "コンソールの表示/非表示"),
        ShortcutItem(key: "⌘ + N", description: "新規チャット"),
        ShortcutItem(key: "⌘ + Shift + H", description: "チャット履歴を表示"),
        ShortcutItem(key: "⌘ + ↑", description: "前のメッセージ履歴"),
        ShortcutItem(key: "⌘ + ↓", description: "次のメッセージ履歴"),
        ShortcutItem(key: "Shift + Enter", description: "改行（TextEditor内）")
    ]
    
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("キーボードショートカット")
                .font(.headline)
            
            Divider()
            
            VStack(alignment: .leading, spacing: 8) {
                ForEach(shortcuts) { shortcut in
                    HStack {
                        Text(shortcut.key)
                            .font(.system(.body, design: .monospaced))
                            .foregroundColor(.primary)
                            .frame(width: 120, alignment: .leading)
                        
                        Text(shortcut.description)
                            .font(.body)
                            .foregroundColor(.secondary)
                        
                        Spacer()
                    }
                }
            }
        }
        .padding()
        .frame(width: 400)
    }
}

struct ShortcutItem: Identifiable {
    let id = UUID()
    let key: String
    let description: String
}

// プレビュー
#Preview {
    ShortcutHelpView()
}