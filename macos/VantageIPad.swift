import SwiftUI

// @main  // macOS target用にコメントアウト
struct VantageIPad: App {
    var body: some Scene {
        WindowGroup {
            Text("Vantage Point for iPad")
                .font(.largeTitle)
                .padding()
                .onAppear {
                    // iPad固有の初期化処理をここに追加
                    configureIPadEnvironment()
                }
        }
    }
    
    private func configureIPadEnvironment() {
        // iPad向けの設定
        // 例: Split View対応、マルチタスキング設定など
    }
}
