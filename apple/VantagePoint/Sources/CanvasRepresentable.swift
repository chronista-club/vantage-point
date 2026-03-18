import SwiftUI
import WebKit

/// Paisley Park Canvas — WKWebView を SwiftUI で表示するラッパー
///
/// SP（Star Platinum）の HTTP サーバーから canvas.html をロードし、
/// WebSocket 経由でリアルタイムにコンテンツを表示する。
/// SP 未起動時はプレースホルダーを表示。
struct CanvasRepresentable: NSViewRepresentable {
    /// SP の HTTP ポート（nil なら未接続）
    let port: UInt16?

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        let webView = WKWebView(frame: .zero, configuration: config)
        webView.setValue(false, forKey: "drawsBackground") // 背景透過
        loadContent(webView, coordinator: context.coordinator)
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        // ポートが変わった場合のみリロード（SwiftUI の state 変化で不要なリロードを防止）
        loadContent(webView, coordinator: context.coordinator)
    }

    private func loadContent(_ webView: WKWebView, coordinator: Coordinator) {
        // 前回と同じポートなら何もしない（初回は必ずロード）
        let currentPort = port
        if coordinator.hasLoaded && coordinator.lastPort == currentPort { return }
        coordinator.lastPort = currentPort
        coordinator.hasLoaded = true

        if let port = currentPort {
            // SP が起動中 → canvas.html をロード（direct モード: SP 個別の PP コンテンツ表示）
            let url = URL(string: "http://localhost:\(port)/canvas?direct")!
            webView.load(URLRequest(url: url))
        } else {
            // SP 未起動 → プレースホルダー HTML
            let html = """
            <!DOCTYPE html>
            <html>
            <head>
            <style>
                body {
                    margin: 0;
                    display: flex;
                    align-items: center;
                    justify-content: center;
                    height: 100vh;
                    background: rgb(24, 24, 28);
                    color: rgb(120, 120, 140);
                    font-family: -apple-system, sans-serif;
                    font-size: 14px;
                }
                .container { text-align: center; }
                .icon { font-size: 48px; margin-bottom: 12px; }
                .title { font-size: 16px; color: rgb(160, 160, 180); margin-bottom: 4px; }
            </style>
            </head>
            <body>
                <div class="container">
                    <div class="icon">🧭</div>
                    <div class="title">Paisley Park</div>
                    <div>Canvas ready</div>
                </div>
            </body>
            </html>
            """
            webView.loadHTMLString(html, baseURL: nil)
        }
    }

    /// ポート変化の追跡用 Coordinator
    class Coordinator {
        var lastPort: UInt16?
        var hasLoaded = false
    }
}
