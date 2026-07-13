//! App icon — runtime で OS に portal の山アイコンを当てる。
//!
//! ## macOS (dock)
//!
//! `vp app start` が起動する bare binary (`~/.cargo/bin/vp-app`) は .app bundle 外なので
//! bundle の `icon.icns` (release:mac が同梱) が効かず、 dock が generic icon になる。 起動時に
//! `NSApplication.setApplicationIconImage` で portal favicon (`assets/icon.png`、 portal の
//! `assets/favicon.svg` 由来の山シルエット) を当て、 dev / cargo 起動でも dock を portal icon にする。
//! .dmg bundle 版は icns と二重掛けになるが冪等。
//!
//! ## Windows (taskbar / Alt-Tab)
//!
//! 見た目の主役は **exe に焼いた icon resource** (`build.rs` + `assets/icon.ico`) で、
//! Explorer / taskbar / Alt-Tab はそれを引く。 本 module はそれを補う 2 つを持つ:
//!
//! - [`set_app_user_model_id`] — taskbar の identity。 pin 留め / grouping が壊れないようにする
//! - [`icon_rgba`] — window icon (tao) と tray icon が要求する生 RGBA の供給元

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
        // event loop 開始後 ~1.5s 間 再アサートされるため debug (info だと spam)。
        tracing::debug!(target: "vp_app::icon", "dock app icon = portal favicon を適用");
    }
}

/// `assets/icon.png` を `size` x `size` の RGBA8 に decode する。
///
/// tray icon (`tray_icon::Icon`) と window icon (`tao::window::Icon`) が共に生 RGBA を要求する
/// ため 1 本に集約する。 asset は `include_bytes!` で binary に焼くので、 bare exe 起動でも
/// 外部ファイル無しに効く (dock icon が icon.png を include するのと同じ流儀)。
///
/// 元画像は 1024x1024 なので、 tray (数十 px) にそのまま渡さず使う寸法へ落としてから返す。
/// 失敗しても致命ではない (アイコンが出ないだけ) ので `None` を返して呼出側で握り潰す。
pub fn icon_rgba(size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let png: &[u8] = include_bytes!("../assets/icon.png");
    let img = match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(target: "vp_app::icon", error = %e, "icon.png の decode に失敗");
            return None;
        }
    };
    let resized = image::imageops::resize(
        &img.to_rgba8(),
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    Some((resized.into_raw(), size, size))
}

/// Windows の AppUserModelID を明示設定する（非 Windows は no-op）。
///
/// 未設定だと taskbar が exe path から暗黙の ID を作るため、 「Start Menu shortcut から起動」と
/// 「cargo target から直接起動」が別アプリ扱いになり pin 留めが機能しない。 shortcut 側にも同じ
/// AUMID を焼いて初めて両者が結びつく（値の SSOT は `vp_paths::app_user_model_id`）。
///
/// **window 生成より前に呼ぶこと** — 既に作られた window の identity は後から変えられない。
pub fn set_app_user_model_id() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        use windows::core::HSTRING;

        let id = vp_paths::app_user_model_id();
        // SAFETY: 有効な null 終端 wide string を渡すだけ。 失敗しても致命ではない（pin 留めが
        // 効かなくなるだけ）ので log に留める。
        match unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(id)) } {
            Ok(()) => tracing::debug!(target: "vp_app::icon", id, "AppUserModelID を設定"),
            Err(e) => {
                tracing::warn!(target: "vp_app::icon", error = %e, "AppUserModelID の設定に失敗")
            }
        }
    }
}
