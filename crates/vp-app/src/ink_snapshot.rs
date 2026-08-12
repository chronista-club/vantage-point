//! ink（対話面, doc 52 §3）の pane snapshot — WKWebView.takeSnapshot で board pane を PNG 化する。
//!
//! ## なぜ WKWebView.takeSnapshot か（doc 52 §3 反転記録）
//!
//! screencapture `-l` + crop 案は 3 コストで却下した:
//! ① Screen Recording **権限がコア動線に入る**（対話面は製品の中心対話）
//! ② 隣 pane（別 session の秘密）込みの中間 full-window PNG が disk に一瞬実体化する
//! ③ title bar / Retina の座標写像を自前で持つ
//!
//! takeSnapshot は webview 自身に描画内容を吐かせるので、**権限不要・pane 以外を一切実体化
//! しない・座標は webview 系のまま**（getBoundingClientRect がそのまま rect になる = Retina
//! 換算不要）。「表象を所有する surface 自身が画像を吐く」= 表象の共有の純度も上がる。
//!
//! completion handler は main thread で発火する。event loop（tao）も main thread なので、
//! `take_snapshot` は event loop から呼び、結果は `done`（= `InkSnapshotReady` を送る closure）
//! で event loop に戻す。

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// snapshot 対象の矩形（webview 論理座標 = WKWebView の view 座標系）。`AppEvent` が運ぶので
/// cross-platform に always-compiled（FFI 本体だけ macOS gate）。
#[derive(Debug, Clone, Copy)]
pub struct InkRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// PNG 保存先 `state_dir/ink/<lane_key>/<unix_millis>.png` を組み、親 dir を作って返す。
/// lane_key は board の flat key（'main' / sub 名）。念のため path 安全化する。
pub fn snapshot_path(lane_key: &str) -> std::io::Result<PathBuf> {
    let dir = ink_root().join(sanitize(lane_key));
    std::fs::create_dir_all(&dir)?;
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Ok(dir.join(format!("{millis}.png")))
}

/// ink の保存根 `state_dir/ink`。
fn ink_root() -> PathBuf {
    vp_paths::vp_state_dir().join("ink")
}

/// lane address → 保存 dir 用の flat key。root/lead → `main` / それ以外 → lane 名。
///
/// ⚠️ **最後の分節が lane 名**（`<repo>/lane/<name>` / 旧 `<repo>/sub/<name>` /
/// 旧 `<repo>/<name>` のいずれでも成り立つ）。旧実装は `/sub/` `/wing/` を**探して**おり、
/// canonical（`/lane/`）が来ると 1 つも当たらず**全 sub が `main` に倒れて**互いの
/// snapshot を上書きし合う状態になっていた。`ends_with("/root")` は偶然通るので、
/// **root だけ動いて sub が壊れる**という気づきにくい形だった。
///
/// snapshot を lane ごとに分けるためだけの folder 名なので、取れない形は `main` に倒す。
pub fn lane_key_from_address(addr: &str) -> String {
    match addr.rsplit('/').next() {
        Some("root" | "lead" | "") | None => "main".to_string(),
        Some(name) => name.to_string(),
    }
}

/// path segment 安全化（区切り・危険文字を `_` に）。flat key は通常安全だが fail-safe に。
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "main".to_string()
    } else {
        cleaned
    }
}

/// 起動時 prune: `state_dir/ink` 配下の `max_age` 超 PNG を削除し、空になった lane dir も掃除する。
/// ink は ephemeral（送信 = 手放す）だが snapshot ファイルは disk に残るので、「消し手のない
/// ファイルを作らない」（doc 52 §3、terminal replay disk leak の轍を踏まない）。
pub fn prune_old(max_age: Duration) {
    let root = ink_root();
    let Ok(now) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        return;
    };
    let Ok(lane_dirs) = std::fs::read_dir(&root) else {
        return; // 未生成なら何もしない
    };
    let mut removed = 0u32;
    for lane_dir in lane_dirs.flatten() {
        let dir = lane_dir.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut remaining = 0u32;
        for f in files.flatten() {
            let p = f.path();
            let too_old = f
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|mt| mt.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|age| now.saturating_sub(age) > max_age)
                .unwrap_or(false);
            if too_old {
                if std::fs::remove_file(&p).is_ok() {
                    removed += 1;
                }
            } else {
                remaining += 1;
            }
        }
        if remaining == 0 {
            let _ = std::fs::remove_dir(&dir); // 空 lane dir を掃除
        }
    }
    if removed > 0 {
        tracing::info!(target: "vp_app::ink", removed, "起動時 prune: 古い ink snapshot を削除");
    }
}

/// board pane（`rect`）を WKWebView.takeSnapshot で撮り `out_path` に PNG を書く。**macOS + main
/// thread から呼ぶこと**。completion（main thread）で `done(path, error)` を呼ぶ:
/// 成功 = `done(Some(path), None)` / 失敗 = `done(None, Some(理由))`。
///
/// 非 macOS では即 `done(None, Some(...))`（platform 非対応）。
pub fn take_snapshot<F>(webview: &wry::WebView, rect: InkRect, out_path: PathBuf, done: F)
where
    F: Fn(Option<String>, Option<String>) + 'static,
{
    #[cfg(target_os = "macos")]
    {
        use block2::RcBlock;
        use objc2::AnyThread;
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        use objc2_foundation::{NSData, NSDictionary, NSError, NSString};
        use objc2_web_kit::WKSnapshotConfiguration;
        use wry::WebViewExtMacOS;

        let Some(mtm) = MainThreadMarker::new() else {
            done(None, Some("main thread でないため snapshot 不可".into()));
            return;
        };

        // WKWebView.takeSnapshot は Option<NSImage> と Option<NSError> を completion に渡す。
        // NSImage → TIFF → NSBitmapImageRep → PNG → fs::write の一本道。
        let out_for_block = out_path.clone();
        let write_png = move |img: *mut NSImage, err: *mut NSError| -> Result<(), String> {
            if img.is_null() {
                let has_err = !err.is_null();
                return Err(format!(
                    "takeSnapshot が null image を返した (err={has_err})"
                ));
            }
            // SAFETY: img は非 null を上で確認済み。completion が渡す有効な NSImage。
            let img: &NSImage = unsafe { &*img };
            let tiff = img
                .TIFFRepresentation()
                .ok_or_else(|| "TIFFRepresentation 失敗".to_string())?;
            let rep = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
                .ok_or_else(|| "NSBitmapImageRep 生成失敗".to_string())?;
            // properties key は NSBitmapImageRepPropertyKey(= NSString)。空 dict を渡す。
            let props: objc2::rc::Retained<NSDictionary<NSString>> = NSDictionary::new();
            // SAFETY: 有効な rep に PNG 表現を要求する。properties は空 dict。
            let png: objc2::rc::Retained<NSData> = unsafe {
                rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props)
            }
            .ok_or_else(|| "PNG 変換失敗".to_string())?;
            let bytes = png.to_vec();
            std::fs::write(&out_for_block, &bytes)
                .map_err(|e| format!("PNG 書き込み失敗 {}: {e}", out_for_block.display()))?;
            Ok(())
        };

        let out_for_done = out_path.clone();
        let completion = RcBlock::new(move |img: *mut NSImage, err: *mut NSError| match write_png(
            img, err,
        ) {
            Ok(()) => done(Some(out_for_done.display().to_string()), None),
            Err(msg) => done(None, Some(msg)),
        });

        // config.rect は view 座標系。webview からの getBoundingClientRect（CSS px = points）を
        // そのまま渡す（Retina 換算不要 — WKWebView が device scale を内部で吸収する）。
        let cg = CGRect::new(CGPoint::new(rect.x, rect.y), CGSize::new(rect.w, rect.h));
        // SAFETY: main thread（mtm で保証）で config を生成し、rect / afterScreenUpdates を
        // 設定する（値渡しのみ）。
        let config = unsafe {
            let config = WKSnapshotConfiguration::new(mtm);
            config.setRect(cg);
            config.setAfterScreenUpdates(true); // 直前の palette 隠しを反映してから撮る
            config
        };

        let wk = webview.webview(); // Retained<WryWebView>（WKWebView subclass）
        // SAFETY: 有効な WKWebView に snapshot を要求する。completion は main thread で 1 回発火し、
        // block が保持する done / write_png もそこで消費される（RcBlock が完了まで生存を保証）。
        unsafe {
            wk.takeSnapshotWithConfiguration_completionHandler(Some(&config), &completion);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (webview, rect, out_path);
        done(None, Some("ink snapshot は macOS のみ対応".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠️ **canonical でも旧形でも「最後の分節が lane 名」**。
    ///
    /// 旧実装は `/sub/` `/wing/` を探す形で、canonical（`/lane/`）が来ると 1 つも当たらず
    /// **全 sub が `main` に倒れて snapshot を上書きし合う**状態になっていた。
    /// `ends_with("/root")` は偶然通るため **root だけ動いて sub が壊れる**という
    /// 気づきにくい壊れ方で、これがその検出器。
    #[test]
    fn lane_key_takes_last_segment_in_every_form() {
        // canonical
        assert_eq!(lane_key_from_address("vp/lane/root"), "main");
        assert_eq!(lane_key_from_address("vp/lane/foo"), "foo");
        // 旧 3 分節
        assert_eq!(lane_key_from_address("vp/sub/foo"), "foo");
        assert_eq!(lane_key_from_address("vp/wing/foo"), "foo");
        // 旧 2 分節
        assert_eq!(lane_key_from_address("vp/root"), "main");
        assert_eq!(lane_key_from_address("vp/foo"), "foo");
        // 旧予約名
        assert_eq!(lane_key_from_address("vp/lead"), "main");
    }

    /// 取れない形は `main` に倒す（folder 名なので落とさない）。
    #[test]
    fn lane_key_falls_back_to_main() {
        assert_eq!(lane_key_from_address(""), "main");
        assert_eq!(lane_key_from_address("vp/"), "main");
    }
}
