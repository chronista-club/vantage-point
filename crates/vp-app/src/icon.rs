//! App icon — runtime で OS に Vantage Point のブランドアイコンを当てる。
//!
//! ## macOS (dock)
//!
//! `vp app start` が起動する bare binary (dev root `~/.local/opt/vp-dev/bin/vp-app` 等) は .app bundle 外なので
//! bundle の `icon.icns` (release:mac が同梱) が効かず、 dock が generic icon になる。 起動時に
//! `NSApplication.setApplicationIconImage` で `assets/icon-macos.png` を当て、dev / cargo 起動でも
//! 同じアイコンを使う。原図は repo root の `assets/brand/source.svg`、生成は `scripts/brand_assets.py`。
//! .dmg bundle 版は icns と二重掛けになるが冪等。
//!
//! ## Windows (taskbar / Alt-Tab)
//!
//! 見た目の主役は **exe に焼いた icon resource** (`build.rs` + `assets/icon.ico`) で、
//! Explorer / taskbar / Alt-Tab はそれを引く。 本 module はそれを補う 2 つを持つ:
//!
//! - [`set_app_user_model_id`] — taskbar の identity。 pin 留め / grouping が壊れないようにする
//! - [`ensure_start_menu_shortcut`] — Start Menu の shortcut を起動のたびに用意・追従させる
//!   （winget の portable 配布は shortcut を作らないため、アプリ自身が置く）
//! - [`icon_rgba`] — window icon (tao) と tray icon が要求する生 RGBA の供給元

/// dock の app icon を Vantage Point のブランドアイコンに設定する。
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
        let png: &[u8] = include_bytes!("../assets/icon-macos.png");
        let data = NSData::with_bytes(png);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            tracing::warn!(target: "vp_app::icon", "icon-macos.png から NSImage 生成に失敗");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: main thread (mtm で保証) から、 有効な NSImage を渡して dock icon を設定する。
        unsafe { app.setApplicationIconImage(Some(&image)) };
        // event loop 開始後 ~1.5s 間 再アサートされるため debug (info だと spam)。
        tracing::debug!(target: "vp_app::icon", "dock app icon = macOS ブランドアイコンを適用");
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

/// Start Menu に「Vantage Point」の shortcut を用意する（Windows のみ、非 Windows は no-op）。
///
/// winget の配布は zip を展開して exe を置くだけ（portable）で shortcut を作らないため、
/// 起動のたびに `vp app install --target <自分> --if-changed` を裏で呼ぶ。COM（AUMID を焼く
/// IShellLink）の実装は `vp app install` に 1 本だけ持ち、ここでは複製しない。
/// 既に自分を指していれば vp 側で即 return するので、毎回の負担は vp の起動 1 回分。
///
/// 次の場合は触らない（既存の shortcut を dev build で乗っ取らないため）:
/// - `VP_PROFILE` 付き（dev）— dev の shortcut が欲しければ `vp app install` を明示的に打つ
/// - cargo の build 出力（`target\release` 等）から起動された
///
/// 消すときは `vp app uninstall`。起動を遅らせないよう別 thread で走らせ、結果は log のみ。
pub fn ensure_start_menu_shortcut() {
    #[cfg(windows)]
    {
        if vp_paths::vp_profile().is_some() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        if is_build_output(&exe) {
            tracing::debug!(target: "vp_app::icon", exe = %exe.display(), "build 出力からの起動のため Start Menu shortcut は触らない");
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("start-menu-shortcut".into())
            .spawn(move || {
                use std::os::windows::process::CommandExt;
                // console 窓を一瞬でも出さない
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                let vp = crate::daemon::launcher::locate_vp_binary();
                let result = std::process::Command::new(&vp)
                    .args(["app", "install", "--if-changed", "--target"])
                    .arg(&exe)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                match result {
                    Ok(o) if o.status.success() => {
                        tracing::debug!(target: "vp_app::icon", "Start Menu shortcut を確認")
                    }
                    Ok(o) => tracing::warn!(
                        target: "vp_app::icon",
                        exit = ?o.status.code(),
                        stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                        "Start Menu shortcut の用意に失敗"
                    ),
                    Err(e) => tracing::warn!(
                        target: "vp_app::icon",
                        vp = %vp.display(),
                        error = %e,
                        "Start Menu shortcut 用に vp を起動できない"
                    ),
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(target: "vp_app::icon", error = %e, "shortcut thread の起動に失敗");
        }
    }
}

/// cargo の build 出力（`<…target…>\{release|debug}\vp-app.exe`）から起動されたか（純粋関数）。
///
/// 親 dir が `release` / `debug` で、その親の名前に `target` を含む（`target` /
/// `cargo-target` / `D:\cargo-target-nightly` 等）ものを build 出力とみなす。
#[cfg(any(windows, test))]
fn is_build_output(exe: &std::path::Path) -> bool {
    let Some(profile_dir) = exe.parent() else {
        return false;
    };
    let is_profile = profile_dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("release") || n.eq_ignore_ascii_case("debug"));
    let under_target = profile_dir
        .parent()
        .and_then(|d| d.file_name())
        .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("target"));
    is_profile && under_target
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // path の区切りは OS 依存（mac では `\` が区切りにならない）なので OS ごとに分ける。
    #[cfg(windows)]
    #[test]
    fn build_output_is_detected() {
        assert!(is_build_output(Path::new(
            r"C:\repo\target\release\vp-app.exe"
        )));
        assert!(is_build_output(Path::new(
            r"D:\cargo-target-nightly\debug\vp-app.exe"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn build_output_is_detected() {
        assert!(is_build_output(Path::new(
            "/Users/x/repo/target/release/vp-app"
        )));
        assert!(!is_build_output(Path::new(
            "/Applications/VantagePoint.app/Contents/MacOS/vp-app"
        )));
    }

    #[cfg(windows)]
    #[test]
    fn installed_locations_are_not_build_output() {
        // winget の package dir / cargo install 先 / 単に release という名前の dir
        assert!(!is_build_output(Path::new(
            r"C:\Users\x\AppData\Local\Microsoft\WinGet\Packages\Chronista.VantagePoint__DefaultSource\vp-app.exe"
        )));
        assert!(!is_build_output(Path::new(
            r"C:\Users\x\.cargo\bin\vp-app.exe"
        )));
        assert!(!is_build_output(Path::new(r"C:\tools\release\vp-app.exe")));
    }
}
