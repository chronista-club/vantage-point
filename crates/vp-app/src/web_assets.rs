//! vp-app webview の `vp-asset://` custom protocol で静的 asset を配信する汎用モジュール。
//!
//! 各 webview が自分の HTML / JS bundle を `serve()` の `extra` slice に積んで配信する
//! (例: `app.rs` の `SIDEBAR_ASSETS` = sidebar.html + sidebar.bundle.js)。
//!
//! ## 履歴 (2026-05-30 縮小)
//!
//! 旧称は「font 集約モジュール」。 PlemolJP Console NF (`VPMono` / `VPMono35`、 32 variant、 ~248MB) を
//! `include_bytes!` で bundle + JS の `FontFace` loader (`NERD_FONT_LOADER_JS`) で動的登録していた。
//! しかし SolidJS sidebar 化 (VP-208、 旧 `SIDEBAR_HTML` 撤去) で loader の consumer が消滅し、
//! bundle font は **未登録の orphan** に (現行 `SIDEBAR_HTML` / `MAIN_AREA_HTML` は `creo-tokens.css` /
//! `vp-tokens.css` を inline するだけで `@font-face` / `FontFace` を持たない)。 font の SSOT は
//! その 2 つの token file の **system-reference family** (Nerd Font chain + Mizolet/みぞれ) に移行済み。
//! よって bundle font 一式 + nerd-font loader (`nerd-font.css` / `nerd-font-loader.js`) を撤去し、
//! 当モジュールは汎用 asset 配信のみに縮小した (binary ~248MB 減)。

use std::borrow::Cow;
use wry::http::{Request, Response};
use wry::WebViewId;

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
/// const SIDEBAR_ASSETS: &[(&str, &[u8], &str)] = &[
///     ("app/sidebar.html", SIDEBAR_HTML.as_bytes(), "text/html; charset=utf-8"),
///     ("app/sidebar.bundle.js", SIDEBAR_BUNDLE, "application/javascript; charset=utf-8"),
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

    // ── dev HMR: VP_WEBVIEW_DEV=<assets dir> なら *.bundle.js を disk から fresh read。
    // 焼き込み (include_bytes!) を bypass するので、 `bun run dev` (esbuild watch) で
    // bundle を更新 → WebView reload するだけで反映され、 cargo build が不要になる。
    // miss / read 失敗時は下の baked asset に fallback (= prod と同じ挙動)。
    if let Ok(dir) = std::env::var("VP_WEBVIEW_DEV") {
        if let Some(path) = uri.split("://").nth(1) {
            if path.ends_with(".bundle.js") {
                let fname = path.rsplit('/').next().unwrap_or(path);
                let disk = std::path::Path::new(&dir).join(fname);
                match std::fs::read(&disk) {
                    Ok(bytes) => {
                        tracing::info!(
                            target: "vp_app::asset",
                            %uri, disk = %disk.display(), bytes = bytes.len(),
                            "DEV disk-read (VP_WEBVIEW_DEV)"
                        );
                        return Response::builder()
                            .status(200)
                            .header("Content-Type", "application/javascript; charset=utf-8")
                            .header("Access-Control-Allow-Origin", "*")
                            .header("Cache-Control", "no-store")
                            .body(Cow::Owned(bytes))
                            .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])));
                    }
                    Err(e) => tracing::warn!(
                        target: "vp_app::asset",
                        %uri, disk = %disk.display(), error = %e,
                        "DEV disk-read 失敗 → baked に fallback"
                    ),
                }
            }
        }
    }

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
