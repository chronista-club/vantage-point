//! vp-app webview の `vp-asset://` custom protocol で静的 asset を配信する汎用モジュール。
//!
//! webview が自分の HTML を `serve()` の `extra` slice に積んで配信する
//! (現行は `app.rs` の `MAIN_VIEW_ASSETS` = 統合 HTML `app/index.html` の 1 entry のみ。
//! JS bundle は `MAIN_AREA_HTML` 内に inline 済なので個別配信しない)。
//!
//! ## 履歴 (2026-05-30 縮小)
//!
//! 旧称は「font 集約モジュール」。 PlemolJP Console NF (`VPMono` / `VPMono35`、 32 variant、 ~248MB) を
//! `include_bytes!` で bundle + JS の `FontFace` loader (`NERD_FONT_LOADER_JS`) で動的登録していた。
//! しかし SolidJS sidebar 化 (VP-208) で loader の consumer が消滅し、
//! bundle font は **未登録の orphan** に。 font の SSOT は `vp-tokens.css` の principal 2 token
//! (font zero-start 2026-07-11: sans='Gen Interface JP' / mono='UDEV Gothic NF'、 いずれも
//! OS install 済 font の名前参照 + local() weight 束ね) に移行済み。
//! よって bundle font 一式 + nerd-font loader (`nerd-font.css` / `nerd-font-loader.js`) を撤去し、
//! 当モジュールは汎用 asset 配信のみに縮小した (binary ~248MB 減)。

use std::borrow::Cow;
use wry::WebViewId;
use wry::http::{Request, Response};

/// `vp-asset://` URI から `extra` 内の asset を lookup。
pub fn lookup_asset(
    uri: &str,
    extra: &'static [(&'static str, &'static [u8], &'static str)],
) -> Option<(&'static [u8], &'static str)> {
    let path = uri.split("://").nth(1)?;
    extra
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, b, c)| (*b, *c))
}

/// `vp-asset://` custom protocol handler の base 関数。
/// webview ごとに自分の HTML / bundle を配信したい場合は、 `extra` slice にその entry を入れて
/// closure に capture する形で wrap する。
///
/// 例:
/// ```ignore
/// const MAIN_VIEW_ASSETS: &[(&str, &[u8], &str)] = &[
///     ("app/index.html", MAIN_AREA_HTML.as_bytes(), "text/html; charset=utf-8"),
/// ];
/// builder.with_custom_protocol("vp-asset".to_string(), |id, req| {
///     web_assets::serve(id, req, MAIN_VIEW_ASSETS)
/// })
/// ```
pub fn serve(
    _id: WebViewId,
    request: Request<Vec<u8>>,
    extra: &'static [(&'static str, &'static [u8], &'static str)],
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().to_string();

    // 注: 旧 `VP_WEBVIEW_DEV` (bundle.js の disk-read HMR) は撤去した。現行 `MAIN_AREA_HTML` は
    // 各 bundle を include_str! で inline するため browser が `*.bundle.js` URL を request せず、
    // 分岐は発火しない dead branch だった。復活手順（bundle の外部 `<script src>` 化）は git history。
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

    #[test]
    fn lookup_asset_resolves_extra() {
        const EXTRA: &[(&str, &[u8], &str)] = &[("app/test.html", b"<html>x</html>", "text/html")];
        let r = lookup_asset("vp-asset://app/test.html", EXTRA);
        assert!(r.is_some());
        let (bytes, ct) = r.unwrap();
        assert_eq!(bytes, b"<html>x</html>");
        assert_eq!(ct, "text/html");

        // 未知 path / garbage は None
        assert_eq!(lookup_asset("vp-asset://app/unknown.html", EXTRA), None);
        assert_eq!(lookup_asset("garbage", EXTRA), None);
    }
}
