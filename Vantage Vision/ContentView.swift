//
//  ContentView.swift
//  Vantage
//
//  Created by Makoto Itoh on 2025/07/03.
//

import SwiftUI
import RealityKit
import RealityKitContent

struct ContentView: View {
    @State private var showConsole = false
    @State private var consoleViewModel = ConsoleViewModel()
    @Environment(AppModel.self) private var appModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow

    var body: some View {
        VStack {
            // メインコンテンツ
            VStack {
                Model3D(named: "Scene", bundle: realityKitContentBundle)
                    .padding(.bottom, 50)

                Text("Hello, world!")

                HStack(spacing: 20) {
                    ToggleImmersiveSpaceButton()
                    
                    Button(action: { showConsole.toggle() }) {
                        Label(showConsole ? "Hide Console" : "Show Console", 
                              systemImage: showConsole ? "terminal.fill" : "terminal")
                    }
                    .buttonStyle(.borderedProminent)
                    
                    Button(action: { 
                        // 環境値をAppModelに設定
                        appModel.openWindow = openWindow
                        appModel.dismissWindow = dismissWindow
                        appModel.toggleAIAssistant() 
                    }) {
                        Label("AI Assistant", systemImage: "cpu")
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.blue)
                }
                
                // テストボタン
                HStack(spacing: 10) {
                    Button("Log Info") {
                        consoleViewModel.info("テスト情報メッセージ", category: "User")
                    }
                    
                    Button("Log Warning") {
                        consoleViewModel.warning("テスト警告メッセージ", category: "User")
                    }
                    
                    Button("Log Error") {
                        consoleViewModel.error("テストエラーメッセージ", category: "User")
                    }
                }
                .buttonStyle(.bordered)
                .padding(.top, 10)
            }
            .padding()
            
            // コンソール表示
            if showConsole {
                ConsoleView()
                    .environment(consoleViewModel)
                    .frame(height: 300)
                    .padding()
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.3), value: showConsole)
    }
}

#Preview(windowStyle: .automatic) {
    ContentView()
        .environment(AppModel())
}
