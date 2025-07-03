//
//  ConsoleView.swift
//  Vantage
//
//  Created by Makoto Itoh on 2025/07/03.
//

import SwiftUI

/// コンソール表示ビュー
struct ConsoleView: View {
    @Environment(ConsoleViewModel.self) private var viewModel
    @State private var scrollProxy: ScrollViewProxy? = nil
    @State private var searchText: String = ""
    @State private var showFilters: Bool = false
    
    var body: some View {
        VStack(spacing: 0) {
            // ヘッダー
            consoleHeader
            
            Divider()
                .background(Color.gray.opacity(0.5))
            
            // メッセージリスト
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(viewModel.filteredMessages) { message in
                            ConsoleMessageRow(message: message)
                                .id(message.id)
                        }
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                }
                .background(Color.black.opacity(0.9))
                .onAppear {
                    scrollProxy = proxy
                    scrollToBottom()
                }
                .onChange(of: viewModel.messages.count) { _, _ in
                    if viewModel.autoScroll {
                        scrollToBottom()
                    }
                }
            }
            
            Divider()
                .background(Color.gray.opacity(0.5))
            
            // フッター（コマンド入力など）
            consoleFooter
        }
        .background(Color.black.opacity(0.95))
        .cornerRadius(12)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Color.gray.opacity(0.3), lineWidth: 1)
        )
    }
    
    // ヘッダービュー
    private var consoleHeader: some View {
        HStack {
            Text("Console")
                .font(.headline)
                .foregroundColor(.white)
            
            Spacer()
            
            // フィルターボタン
            Button(action: { showFilters.toggle() }) {
                Image(systemName: "line.3.horizontal.decrease.circle")
                    .foregroundColor(viewModel.filterLevel != nil || (viewModel.filterCategory != nil && !viewModel.filterCategory!.isEmpty) ? .blue : .gray)
            }
            .buttonStyle(.plain)
            
            // 自動スクロールトグル
            Toggle("", isOn: Bindable(viewModel).autoScroll)
                .toggleStyle(AutoScrollToggleStyle())
            
            // クリアボタン
            Button(action: { viewModel.clear() }) {
                Image(systemName: "trash")
                    .foregroundColor(.gray)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color.black.opacity(0.8))
        
        // フィルターパネル
        .overlay(alignment: .topTrailing) {
            if showFilters {
                filterPanel
                    .offset(y: 40)
            }
        }
    }
    
    // フィルターパネル
    private var filterPanel: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Filters")
                .font(.caption)
                .foregroundColor(.gray)
            
            // レベルフィルター
            VStack(alignment: .leading, spacing: 4) {
                Text("Log Level")
                    .font(.caption2)
                    .foregroundColor(.gray)
                
                Picker("Level", selection: Bindable(viewModel).filterLevel) {
                    Text("All").tag(LogLevel?.none)
                    ForEach(LogLevel.allCases, id: \.self) { level in
                        Label(level.rawValue, systemImage: "circle.fill")
                            .foregroundColor(level.color)
                            .tag(LogLevel?.some(level))
                    }
                }
                .pickerStyle(.segmented)
            }
            
            // カテゴリフィルター
            VStack(alignment: .leading, spacing: 4) {
                Text("Category")
                    .font(.caption2)
                    .foregroundColor(.gray)
                
                TextField("Filter category...", text: Bindable(viewModel).filterCategory ?? Binding.constant(""))
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)
            }
        }
        .padding(12)
        .background(Color.black.opacity(0.95))
        .cornerRadius(8)
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.gray.opacity(0.3), lineWidth: 1)
        )
        .frame(width: 250)
    }
    
    // フッタービュー
    private var consoleFooter: some View {
        HStack {
            Text("\(viewModel.filteredMessages.count) messages")
                .font(.caption)
                .foregroundColor(.gray)
            
            Spacer()
            
            // 将来的にコマンド入力を追加する場所
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Color.black.opacity(0.8))
    }
    
    // 最下部へスクロール
    private func scrollToBottom() {
        if let lastMessage = viewModel.filteredMessages.last {
            withAnimation(.easeOut(duration: 0.2)) {
                scrollProxy?.scrollTo(lastMessage.id, anchor: .bottom)
            }
        }
    }
}

/// コンソールメッセージの行表示
struct ConsoleMessageRow: View {
    let message: ConsoleMessage
    
    var body: some View {
        HStack(alignment: .top, spacing: 4) {
            // タイムスタンプ
            Text(message.formattedTimestamp)
                .font(.system(.caption2, design: .monospaced))
                .foregroundColor(.gray)
            
            // レベルインジケーター
            Text(message.level.symbol)
                .font(.caption2)
            
            // カテゴリ
            Text("[\(message.category)]")
                .font(.system(.caption2, design: .monospaced))
                .foregroundColor(.cyan)
            
            // メッセージ
            Text(message.message)
                .font(.system(.caption, design: .monospaced))
                .foregroundColor(message.level.color)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 1)
    }
}

/// 自動スクロールのトグルスタイル
struct AutoScrollToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        Button(action: { configuration.isOn.toggle() }) {
            Image(systemName: configuration.isOn ? "arrow.down.circle.fill" : "arrow.down.circle")
                .foregroundColor(configuration.isOn ? .blue : .gray)
        }
        .buttonStyle(.plain)
    }
}

// プレビュー
#Preview(windowStyle: .automatic) {
    ConsoleView()
        .environment(ConsoleViewModel())
        .frame(width: 600, height: 400)
}