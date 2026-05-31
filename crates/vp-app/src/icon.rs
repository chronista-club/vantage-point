//! App icon — runtime で dock (macOS) に portal の山アイコンを当てる。
//!
//! `vp app start` が起動する bare binary (`~/.cargo/bin/vp-app`) は .app bundle 外なので
//! bundle の `icon.icns` (release:mac が同梱) が効かず、 dock が generic icon になる。 起動時に
//! `NSApplication.setApplicationIconImage` で portal favicon (`assets/icon.png`、 portal の
//! `assets/favicon.svg` 由来の山シルエット) を当て、 dev / cargo 起動でも dock を portal icon にする。
//! .dmg bundle 版は icns と二重掛けになるが冪等。

/// dock の app icon を portal の山アイコンに設定する。
///
/// **macOS のみ + main thread から呼ぶこと**（AppKit 制約）。 非 macOS は no-op。
pub fn set_app_icon() {
    #[cfg(target_os = "macos")]
    {
        use objc2::{AnyThread, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;

        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!(target: "vp_app::icon", "main thread でないため dock icon 設定を skip");
            return;
        };
        let png: &[u8] = include_bytes!("../assets/icon.png");
        let data = NSData::with_bytes(png);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            tracing::warn!(target: "vp_app::icon", "icon.png から NSImage 生成に失敗");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: main thread (mtm で保証) から、 有効な NSImage を渡して dock icon を設定する。
        unsafe { app.setApplicationIconImage(Some(&image)) };
        tracing::info!(target: "vp_app::icon", "dock app icon = portal favicon を適用");
    }
}
