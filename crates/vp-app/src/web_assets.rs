//! Phase 5-C: vp-app webview 共通の bundled asset 集約モジュール。
//!
//! Nerd Font icon を CJK + Latin と統合した PlemolJP Console NF を bundle、
//! `vp-asset://` custom protocol で WebView に配信する。
//!
//! ## 構成
//!
//! - **VPMono** family: PlemolJP Console NF (Latin 1.0 : JP 2.0 比率) — sidebar / chrome 標準
//! - **VPMono35** family: PlemolJP 35Console NF (Latin 0.6 : JP 1.0、 Latin やや広め) — Editor 切替候補
//! - 各 family に 16 weight/style variant: Thin / ExtraLight / Light / Text / Regular / Medium / SemiBold / Bold (× normal/italic)
//!
//! ## 使い方 (1 webview = 3 step)
//!
//! 1. WebViewBuilder に `with_custom_protocol("vp-asset", ...)` で `serve()` を closure に capture
//! 2. HTML の `<style>` に `NERD_FONT_CSS` を含める
//! 3. HTML の `<script>` 冒頭に `NERD_FONT_LOADER_JS` を含める
//!
//! 後は `<span class="nf-icon"></span>` のように `.nf-icon` class を使えば
//! `VPMono` family の Nerd Font glyph で描画される。 body も `font-family: 'VPMono'` で統一。
//!
//! ## 設計理由
//!
//! - WKWebView は CSS @font-face url() を WKURLSchemeHandler に投げない既知制限あり、
//!   JS の `fetch` + `new FontFace` 経由で ArrayBuffer から動的 register する path のみ確実に動く。
//! - CSS variable 経由 (`var(--typography-family-icon)`) も動的 FontFace を再評価しないため、
//!   `.nf-icon` は direct family 宣言 (`'VPMono', monospace`) で固定する。
//! - PlemolJP Console NF は Latin (JetBrains Mono) + JP (IBM Plex Sans JP) hybrid + Nerd Font icon の
//!   3 in 1 構成、 sidebar / chrome / icon を単一 family で統一できる。

use std::borrow::Cow;
use wry::http::{Request, Response};
use wry::WebViewId;

// ── PlemolJP Console NF (default 1.0:2.0 ratio、 family="VPMono") ──────────────
const PLEMOL_THIN: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Thin.ttf");
const PLEMOL_THIN_I: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-ThinItalic.ttf");
const PLEMOL_EXTRALIGHT: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-ExtraLight.ttf");
const PLEMOL_EXTRALIGHT_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJPConsoleNF-ExtraLightItalic.ttf");
const PLEMOL_LIGHT: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Light.ttf");
const PLEMOL_LIGHT_I: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-LightItalic.ttf");
const PLEMOL_TEXT: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Text.ttf");
const PLEMOL_TEXT_I: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-TextItalic.ttf");
const PLEMOL_REGULAR: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Regular.ttf");
const PLEMOL_ITALIC: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Italic.ttf");
const PLEMOL_MEDIUM: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Medium.ttf");
const PLEMOL_MEDIUM_I: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-MediumItalic.ttf");
const PLEMOL_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-SemiBold.ttf");
const PLEMOL_SEMIBOLD_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJPConsoleNF-SemiBoldItalic.ttf");
const PLEMOL_BOLD: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-Bold.ttf");
const PLEMOL_BOLD_I: &[u8] = include_bytes!("../assets/fonts/PlemolJPConsoleNF-BoldItalic.ttf");

// ── PlemolJP 35Console NF (Latin 0.6:1.0、 family="VPMono35") ──────────────────
const PLEMOL35_THIN: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Thin.ttf");
const PLEMOL35_THIN_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-ThinItalic.ttf");
const PLEMOL35_EXTRALIGHT: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-ExtraLight.ttf");
const PLEMOL35_EXTRALIGHT_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-ExtraLightItalic.ttf");
const PLEMOL35_LIGHT: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Light.ttf");
const PLEMOL35_LIGHT_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-LightItalic.ttf");
const PLEMOL35_TEXT: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Text.ttf");
const PLEMOL35_TEXT_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-TextItalic.ttf");
const PLEMOL35_REGULAR: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Regular.ttf");
const PLEMOL35_ITALIC: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Italic.ttf");
const PLEMOL35_MEDIUM: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Medium.ttf");
const PLEMOL35_MEDIUM_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-MediumItalic.ttf");
const PLEMOL35_SEMIBOLD: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-SemiBold.ttf");
const PLEMOL35_SEMIBOLD_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-SemiBoldItalic.ttf");
const PLEMOL35_BOLD: &[u8] = include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-Bold.ttf");
const PLEMOL35_BOLD_I: &[u8] =
    include_bytes!("../assets/fonts/PlemolJP35ConsoleNF-BoldItalic.ttf");

/// `vp-asset://` 経路で配信する font 一覧 (32 entries = 16 weights × 2 series)。
/// 不要 weight は将来 trim 可、 path は JS variants 配列と一致させる必要がある。
pub const FONT_ASSETS: &[(&str, &[u8], &str)] = &[
    // VPMono = PlemolJP Console NF (16 variants)
    ("font/plemol-thin.ttf", PLEMOL_THIN, "font/ttf"),
    ("font/plemol-thinitalic.ttf", PLEMOL_THIN_I, "font/ttf"),
    ("font/plemol-extralight.ttf", PLEMOL_EXTRALIGHT, "font/ttf"),
    ("font/plemol-extralightitalic.ttf", PLEMOL_EXTRALIGHT_I, "font/ttf"),
    ("font/plemol-light.ttf", PLEMOL_LIGHT, "font/ttf"),
    ("font/plemol-lightitalic.ttf", PLEMOL_LIGHT_I, "font/ttf"),
    ("font/plemol-text.ttf", PLEMOL_TEXT, "font/ttf"),
    ("font/plemol-textitalic.ttf", PLEMOL_TEXT_I, "font/ttf"),
    ("font/plemol-regular.ttf", PLEMOL_REGULAR, "font/ttf"),
    ("font/plemol-italic.ttf", PLEMOL_ITALIC, "font/ttf"),
    ("font/plemol-medium.ttf", PLEMOL_MEDIUM, "font/ttf"),
    ("font/plemol-mediumitalic.ttf", PLEMOL_MEDIUM_I, "font/ttf"),
    ("font/plemol-semibold.ttf", PLEMOL_SEMIBOLD, "font/ttf"),
    ("font/plemol-semibolditalic.ttf", PLEMOL_SEMIBOLD_I, "font/ttf"),
    ("font/plemol-bold.ttf", PLEMOL_BOLD, "font/ttf"),
    ("font/plemol-bolditalic.ttf", PLEMOL_BOLD_I, "font/ttf"),
    // VPMono35 = PlemolJP 35Console NF (16 variants)
    ("font/plemol35-thin.ttf", PLEMOL35_THIN, "font/ttf"),
    ("font/plemol35-thinitalic.ttf", PLEMOL35_THIN_I, "font/ttf"),
    ("font/plemol35-extralight.ttf", PLEMOL35_EXTRALIGHT, "font/ttf"),
    ("font/plemol35-extralightitalic.ttf", PLEMOL35_EXTRALIGHT_I, "font/ttf"),
    ("font/plemol35-light.ttf", PLEMOL35_LIGHT, "font/ttf"),
    ("font/plemol35-lightitalic.ttf", PLEMOL35_LIGHT_I, "font/ttf"),
    ("font/plemol35-text.ttf", PLEMOL35_TEXT, "font/ttf"),
    ("font/plemol35-textitalic.ttf", PLEMOL35_TEXT_I, "font/ttf"),
    ("font/plemol35-regular.ttf", PLEMOL35_REGULAR, "font/ttf"),
    ("font/plemol35-italic.ttf", PLEMOL35_ITALIC, "font/ttf"),
    ("font/plemol35-medium.ttf", PLEMOL35_MEDIUM, "font/ttf"),
    ("font/plemol35-mediumitalic.ttf", PLEMOL35_MEDIUM_I, "font/ttf"),
    ("font/plemol35-semibold.ttf", PLEMOL35_SEMIBOLD, "font/ttf"),
    ("font/plemol35-semibolditalic.ttf", PLEMOL35_SEMIBOLD_I, "font/ttf"),
    ("font/plemol35-bold.ttf", PLEMOL35_BOLD, "font/ttf"),
    ("font/plemol35-bolditalic.ttf", PLEMOL35_BOLD_I, "font/ttf"),
];

/// `<style>` 内に取り込む CSS。 .nf-icon class を VPMono family で固定 (var() 経由しない)。
/// 実体は `assets/nerd-font.css` ── `app.rs` 側 `SIDEBAR_HTML` も同 file を `include_str!` で取り込み、
/// **single source of truth** を保つ (両方が同じ bytes を見る)。
pub const NERD_FONT_CSS: &str = include_str!("../assets/nerd-font.css");

/// `<script>` 冒頭に取り込む JS。 全 32 variant を fetch + FontFace 登録、 完了後 state 再 apply。
/// VPMono と VPMono35 の 2 family を同時 register、 CSS で family 名選択するだけで切替可能。
/// Promise.all で並列 fetch、 ~32 リクエスト同時飛ばし → load 時間を短縮 (sequential 版より ~10x 高速)。
/// 実体は `assets/nerd-font-loader.js` ── `app.rs` 側 `SIDEBAR_HTML` も同 file を `include_str!` で取り込む。
pub const NERD_FONT_LOADER_JS: &str = include_str!("../assets/nerd-font-loader.js");

/// `vp-asset://` URI から bundled asset を lookup。 webview-specific HTML 等を `extra` に積めば
/// FONT_ASSETS と一緒に並列探索される (chain order は font 優先、 no shadow 担保)。
pub fn lookup_asset(
    uri: &str,
    extra: &'static [(&'static str, &'static [u8], &'static str)],
) -> Option<(&'static [u8], &'static str)> {
    let path = uri.split("://").nth(1)?;
    FONT_ASSETS
        .iter()
        .chain(extra.iter())
        .find(|(p, _, _)| *p == path)
        .map(|(_, b, c)| (*b, *c))
}

/// vp-asset:// custom protocol handler の base 関数。
/// webview ごとに自分の HTML を vp-asset 配信したい場合は、
/// `extra` slice にその entry を入れて closure に capture する形で wrap する。
///
/// 例:
/// ```ignore
/// const SIDEBAR_ASSETS: &[(&str, &[u8], &str)] = &[
///     ("app/sidebar.html", SIDEBAR_HTML.as_bytes(), "text/html; charset=utf-8"),
/// ];
/// builder.with_custom_protocol("vp-asset".to_string(), |id, req| {
///     web_assets::serve(id, req, SIDEBAR_ASSETS)
/// })
/// ```
pub fn serve(
    _id: WebViewId,
    request: Request<Vec<u8>>,
    extra: &'static [(&'static str, &'static [u8], &'static str)],
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();
    match lookup_asset(&uri, extra) {
        Some((bytes, content_type)) => {
            tracing::info!(
                target: "vp_app::asset",
                %uri,
                bytes = bytes.len(),
                %content_type,
                "HIT"
            );
            Response::builder()
                .status(200)
                .header("Content-Type", content_type)
                .header("Access-Control-Allow-Origin", "*")
                .body(Cow::Borrowed(bytes))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
        }
        None => {
            tracing::warn!(target: "vp_app::asset", %uri, "MISS (404)");
            Response::builder()
                .status(404)
                .body(Cow::Borrowed(&[][..]))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FONT_ASSETS 全 entry が存在 + 正しい content-type + TTF magic
    #[test]
    fn font_assets_have_valid_entries() {
        assert!(!FONT_ASSETS.is_empty(), "FONT_ASSETS is empty");
        assert_eq!(FONT_ASSETS.len(), 32, "expected 32 font variants (16 weights × 2 series)");
        for (path, bytes, ct) in FONT_ASSETS {
            assert!(path.starts_with("font/"), "asset path missing 'font/' prefix: {}", path);
            assert!(bytes.len() > 1_000_000, "font {} suspiciously small: {} bytes", path, bytes.len());
            assert_eq!(*ct, "font/ttf", "unexpected content-type for {}: {}", path, ct);
            let sig = &bytes[..4];
            let valid = sig == [0x00, 0x01, 0x00, 0x00] || sig == b"OTTO" || sig == b"true";
            assert!(valid, "invalid font signature for {}: {:?}", path, sig);
        }
    }

    #[test]
    fn lookup_asset_hits_known_paths() {
        let r = lookup_asset("vp-asset://font/plemol-regular.ttf", &[]);
        assert!(r.is_some(), "lookup returned None for known font path");
        let (bytes, ct) = r.unwrap();
        assert_eq!(ct, "font/ttf");
        assert!(bytes.len() > 1_000_000);

        assert_eq!(lookup_asset("vp-asset://font/unknown.ttf", &[]), None);
        assert_eq!(lookup_asset("garbage", &[]), None);
    }

    #[test]
    fn lookup_asset_chains_extra() {
        const EXTRA: &[(&str, &[u8], &str)] =
            &[("app/test.html", b"<html>x</html>", "text/html")];
        let r = lookup_asset("vp-asset://app/test.html", EXTRA);
        assert!(r.is_some());
        let (bytes, ct) = r.unwrap();
        assert_eq!(bytes, b"<html>x</html>");
        assert_eq!(ct, "text/html");
    }

    #[test]
    fn nerd_font_css_declares_vpmono() {
        assert!(NERD_FONT_CSS.contains(".nf-icon{font-family:'VPMono'"));
    }

    #[test]
    fn nerd_font_loader_js_has_fetch_and_fontface() {
        assert!(NERD_FONT_LOADER_JS.contains("fetch('vp-asset://font/"));
        assert!(NERD_FONT_LOADER_JS.contains("new FontFace("));
        assert!(NERD_FONT_LOADER_JS.contains("document.fonts.add"));
        assert!(NERD_FONT_LOADER_JS.contains("VPMono"));
        assert!(NERD_FONT_LOADER_JS.contains("VPMono35"));
        assert!(NERD_FONT_LOADER_JS.contains("Promise.all"));
    }

    /// 全 16 weight variant 名が JS variants 配列に含まれる
    #[test]
    fn nerd_font_loader_js_lists_all_16_weights() {
        for v in &[
            "thin", "thinitalic", "extralight", "extralightitalic",
            "light", "lightitalic", "text", "textitalic",
            "regular", "italic", "medium", "mediumitalic",
            "semibold", "semibolditalic", "bold", "bolditalic",
        ] {
            assert!(NERD_FONT_LOADER_JS.contains(&format!("'{}'", v)),
                "JS loader missing variant: {}", v);
        }
    }
}
