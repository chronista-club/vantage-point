import SwiftUI
import WebKit

/// Preview Pane — file / image / URL を WKWebView で表示 (read-only)
///
/// Paisley Park 拡張 kind。Canvas が SP の live content を表示するのに対し、
/// Preview は **指定した URL/ファイルを直接レンダリング**する静的プレビュー。
/// 画像 (PNG/JPEG/GIF)・PDF・HTML・Markdown (レンダリング済) を WebKit に委ねる。
///
/// VP-83 Phase 2.3 実装の MVP:
/// - URL は `PaneLeaf.previewURL` 経由で注入
/// - file URL (`file://`) は `loadFileURL` でサンドボックス対応
/// - http/https は通常 load
/// - nil の場合はドロップヒント HTML 表示
struct PreviewRepresentable: NSViewRepresentable {
    /// 表示対象 URL (nil なら placeholder)
    let url: URL?

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        let webView = WKWebView(frame: .zero, configuration: config)
        webView.setValue(false, forKey: "drawsBackground")  // 背景透過
        loadContent(webView, coordinator: context.coordinator)
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        loadContent(webView, coordinator: context.coordinator)
    }

    private func loadContent(_ webView: WKWebView, coordinator: Coordinator) {
        // 同じ URL なら reload スキップ
        if coordinator.lastURL == url { return }
        coordinator.lastURL = url

        guard let url = url else {
            webView.loadHTMLString(Self.placeholderHTML, baseURL: nil)
            return
        }

        if url.isFileURL {
            // ファイル直接 load — parent directory を allowingReadAccessTo に指定し、
            // 画像・PDF・HTML の相対 asset も解決可能に
            let accessRoot = url.deletingLastPathComponent()
            webView.loadFileURL(url, allowingReadAccessTo: accessRoot)
        } else {
            webView.load(URLRequest(url: url))
        }
    }

    /// URL 未指定時のプレースホルダ HTML
    private static let placeholderHTML = """
    <!DOCTYPE html>
    <html><head><style>
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
    </style></head>
    <body>
        <div class="container">
            <div class="icon">🔍</div>
            <div class="title">Preview</div>
            <div>ファイルをドラッグ &amp; ドロップ</div>
        </div>
    </body></html>
    """

    /// URL 変化追跡用 Coordinator
    class Coordinator {
        var lastURL: URL?
    }
}
