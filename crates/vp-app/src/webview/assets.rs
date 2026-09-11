//! vp-app webview の `vp-asset://` custom protocol で静的 asset を配信する汎用モジュール。
//!
//! webview が自分の HTML を `serve()` の `extra` slice に積んで配信する
//! (現行は `app.rs` の `MAIN_VIEW_ASSETS` = 統合 HTML `app/index.html` + SolidJS bundle 2 本。
//! bundle は doc 48 Phase 1 で inline → 外部 `<script src>` 化し、`VP_WEBVIEW_DEV` の
//! disk-read HMR を可能にした)。
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

    // dev HMR (doc 48 Phase 1): `VP_WEBVIEW_DEV=<assets dir>` の時、`*.bundle.js` request を
    // disk の fresh bundle で応える。`bun run dev` (esbuild watch) で bundle を更新 →
    // WebView reload (View → Reload WebView) だけで反映され、cargo build が不要になる。
    // miss / read 失敗は下の baked asset に fallback (= prod と同じ挙動)。
    // 旧 #494 実装の復活 — #815 で dead branch として撤去されていたが、bundle の外部
    // `<script src>` 化 (main_area.rs) で `*.bundle.js` URL が実際に request されるようになった。
    if let Ok(dir) = std::env::var("VP_WEBVIEW_DEV")
        && let Some(bytes) = dev_bundle_read(&uri, &dir)
    {
        return Response::builder()
            .status(200)
            .header("Content-Type", "application/javascript; charset=utf-8")
            .header("Access-Control-Allow-Origin", "*")
            .header("Cache-Control", "no-store")
            .body(Cow::Owned(bytes))
            .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])));
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

/// `VP_WEBVIEW_DEV` disk-read の判定 + 読込 (純 calculation + fs read、env 非依存で単体テスト可)。
///
/// `*.bundle.js` request のみ対象。path の最終 filename を `dev_dir` 直下から読む
/// (bundle は flat に出力される — `webview/build.mjs` の outdir 規約)。
/// 対象外 / read 失敗は `None` (= caller が baked asset に fallback)。
fn dev_bundle_read(uri: &str, dev_dir: &str) -> Option<Vec<u8>> {
    let path = uri.split("://").nth(1)?;
    if !path.ends_with(".bundle.js") {
        return None;
    }
    let fname = path.rsplit('/').next().unwrap_or(path);
    let disk = std::path::Path::new(dev_dir).join(fname);
    match std::fs::read(&disk) {
        Ok(bytes) => {
            tracing::info!(
                target: "vp_app::asset",
                %uri, disk = %disk.display(), bytes = bytes.len(),
                "DEV disk-read (VP_WEBVIEW_DEV)"
            );
            Some(bytes)
        }
        Err(e) => {
            tracing::warn!(
                target: "vp_app::asset",
                %uri, disk = %disk.display(), error = %e,
                "DEV disk-read 失敗 → baked に fallback"
            );
            None
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

    /// dev disk-read: `*.bundle.js` のみ対象、実在 file を読み、miss は None (baked fallback)。
    #[test]
    fn dev_bundle_read_reads_bundle_js_only() {
        let dir = std::env::temp_dir().join(format!("vp-webview-dev-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("editor-host.bundle.js"), b"console.log('dev')").unwrap();
        let dir_s = dir.to_string_lossy().into_owned();

        // hit: path の最終 filename で dir 直下から読む
        let hit = dev_bundle_read("vp-asset://app/editor-host.bundle.js", &dir_s);
        assert_eq!(hit.as_deref(), Some(b"console.log('dev')".as_slice()));

        // 対象外 (bundle.js でない) は fs を見ずに None
        assert_eq!(dev_bundle_read("vp-asset://app/index.html", &dir_s), None);

        // 対象だが disk に無い → None (= baked fallback、serve は prod 挙動に落ちる)
        assert_eq!(
            dev_bundle_read("vp-asset://app/sidebar.bundle.js", &dir_s),
            None
        );

        // scheme 無し garbage は None
        assert_eq!(dev_bundle_read("garbage.bundle.js", &dir_s), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
