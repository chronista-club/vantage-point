//! VP の canonical screenshot 機構 ── platform-agnostic な trait + 各 OS native backend。
//!
//! ## 用途
//!
//! - `vp shot` CLI subcommand (vp-cli)
//! - MCP `capture_terminal` tool (vantage-point/src/mcp.rs)
//! - 将来の HTTP endpoint / 別ツール
//!
//! ## 設計
//!
//! 1 個の `Capture` trait に各 backend を実装、 `default_backend()` が cfg で OS 別 backend を返す。
//! caller は backend を意識せず `default_backend().capture(filter, output)` で取れる。
//!
//! ## Backend
//!
//! - macOS: `objc2-core-graphics` で window 列挙 + `screencapture` CLI で capture (hybrid、 ~80ms)
//! - Windows: 将来 `Windows.Graphics.Capture` API or BitBlt + GDI
//! - Linux: 将来 X11 (`xwd`) / Wayland (`grim`) backend 切替

use std::path::PathBuf;

pub mod compose;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

/// window の列挙結果 (1 entry)
#[derive(Debug, Clone)]
pub struct WindowInfo {
    /// platform-specific window id (macOS: CGWindowID、 Win: HWND、 X11: Window XID)
    pub id: u64,
    /// owning process / app name (macOS: kCGWindowOwnerName)
    pub owner: String,
    /// window title (空の場合あり)
    pub title: String,
    /// 画面座標での window 左上 (logical pixel)
    pub x: i32,
    pub y: i32,
    /// pixel size (logical)
    pub width: u32,
    pub height: u32,
    /// window layer / z-order (macOS: kCGWindowLayer、 0 = 通常)
    pub layer: i32,
}

/// 画面座標 (logical px) で表す矩形領域 (`screencapture -R x,y,w,h` と一致)。
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// 文字列 "x,y,w,h" から parse (CLI flag 用)
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 4 {
            return Err(format!("rect must be 'x,y,w,h' (got {:?})", s));
        }
        let x: i32 = parts[0]
            .trim()
            .parse()
            .map_err(|e| format!("rect.x: {}", e))?;
        let y: i32 = parts[1]
            .trim()
            .parse()
            .map_err(|e| format!("rect.y: {}", e))?;
        let w: u32 = parts[2]
            .trim()
            .parse()
            .map_err(|e| format!("rect.w: {}", e))?;
        let h: u32 = parts[3]
            .trim()
            .parse()
            .map_err(|e| format!("rect.h: {}", e))?;
        if w == 0 || h == 0 {
            return Err(format!("rect width/height must be > 0 (got {}x{})", w, h));
        }
        Ok(Rect { x, y, w, h })
    }
}

/// capture 対象 window の絞り込み
#[derive(Debug, Clone)]
pub struct CaptureFilter {
    /// owning process name で絞り込む (default: "vp-app")
    pub owner: String,
    /// 絞り込んだ window list の n 番目 (0-based、 None = 0 = frontmost)
    pub index: Option<usize>,
    /// title 部分一致でさらに絞り込む (None = 全部)
    pub title_match: Option<String>,
}

impl Default for CaptureFilter {
    fn default() -> Self {
        Self {
            owner: "vp-app".into(),
            index: None,
            title_match: None,
        }
    }
}

/// capture 完了時の metadata
#[derive(Debug, Clone)]
pub struct CaptureResult {
    /// 保存 path
    pub path: PathBuf,
    /// 画像 width (px)
    pub width: u32,
    /// 画像 height (px)
    pub height: u32,
    /// 経過時間 (ms) ── window enum + capture 込み
    pub elapsed_ms: u64,
    /// captured window の元情報
    pub window: WindowInfo,
}

/// platform 横断の capture API
pub trait Capture: Send + Sync {
    /// owner name (などの filter) に該当する window list を返す。
    /// 順序は frontmost first (z-order 先頭から)。
    fn list_windows(&self, filter: &CaptureFilter) -> Result<Vec<WindowInfo>, String>;

    /// 1 個の window を捕って path に保存。 output が None なら default path
    /// (`/tmp/vp/shot-latest.png`、 dir 自動作成)。
    fn capture(
        &self,
        filter: &CaptureFilter,
        output: Option<PathBuf>,
    ) -> Result<CaptureResult, String>;

    /// 画面座標 (logical px) の任意矩形を capture。 window 全体ではなく sub-region 用。
    /// `Rect.x/y` は screen 座標 (window 左上 + 相対 offset 計算済み)。
    /// `--region sidebar` 等の名付き region も内部でこの API に解決される。
    fn capture_rect(&self, rect: Rect, output: Option<PathBuf>) -> Result<CaptureResult, String>;
}

/// 名付き region を window 内 sub-rect に解決。 unknown name は None。
///
/// `sidebar`: 左 280px (`SIDEBAR_WIDTH`、 vp-app の layout 固定値と同期)
/// `main` / `main-area`: sidebar 右側全部
/// `full`: window 全体
pub fn region_for_name(name: &str, window: &WindowInfo) -> Option<Rect> {
    const SIDEBAR_WIDTH: u32 = 280; // vp-app/src/app.rs の SIDEBAR_WIDTH と同期
    match name {
        "sidebar" => Some(Rect {
            x: window.x,
            y: window.y,
            w: SIDEBAR_WIDTH.min(window.width),
            h: window.height,
        }),
        "main" | "main-area" => Some(Rect {
            x: window.x + SIDEBAR_WIDTH as i32,
            y: window.y,
            w: window.width.saturating_sub(SIDEBAR_WIDTH),
            h: window.height,
        }),
        "full" => Some(Rect {
            x: window.x,
            y: window.y,
            w: window.width,
            h: window.height,
        }),
        _ => None,
    }
}

/// 現 OS に対応した backend を返す。 caller はこの 1 関数だけ意識すれば済む。
pub fn default_backend() -> Box<dyn Capture> {
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacOsBackend);
    #[cfg(target_os = "windows")]
    return Box::new(windows::WindowsBackend);
    #[cfg(target_os = "linux")]
    return Box::new(linux::LinuxBackend);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        struct NoBackend;
        impl Capture for NoBackend {
            fn list_windows(&self, _: &CaptureFilter) -> Result<Vec<WindowInfo>, String> {
                Err("screenshot: this OS is not supported".into())
            }
            fn capture(
                &self,
                _: &CaptureFilter,
                _: Option<PathBuf>,
            ) -> Result<CaptureResult, String> {
                Err("screenshot: this OS is not supported".into())
            }
            fn capture_rect(&self, _: Rect, _: Option<PathBuf>) -> Result<CaptureResult, String> {
                Err("screenshot: this OS is not supported".into())
            }
        }
        Box::new(NoBackend)
    }
}

/// default 出力 path: `/tmp/vp/shot-latest.png`
pub fn default_output_path() -> PathBuf {
    PathBuf::from("/tmp/vp/shot-latest.png")
}

/// `pick_window`: filter.index / filter.title_match を candidates に適用。
///
/// `index` が指定されてなければ **「title 非空優先 + 面積大優先」** で sort して先頭を返す。
/// VP の sub-window (Editor overlay 等、 title 空で size 小) を main window と誤認しない対策。
/// `index` 指定時は z-order そのままで n 番目 (= sort せず元順序)。
pub fn pick_window(
    candidates: &[WindowInfo],
    filter: &CaptureFilter,
) -> Result<WindowInfo, String> {
    let mut filtered: Vec<WindowInfo> = candidates
        .iter()
        .filter(|w| match &filter.title_match {
            Some(t) => w.title.contains(t),
            None => true,
        })
        .cloned()
        .collect();

    if filter.index.is_none() {
        // default: title 非空 + 面積大 で sort、 先頭が main window
        filtered.sort_by(|a, b| {
            let a_titled = !a.title.is_empty();
            let b_titled = !b.title.is_empty();
            b_titled
                .cmp(&a_titled)
                .then((b.width as u64 * b.height as u64).cmp(&(a.width as u64 * a.height as u64)))
        });
    }
    let idx = filter.index.unwrap_or(0);
    filtered.get(idx).cloned().ok_or_else(|| {
        format!(
            "no matching window: owner={}, title_match={:?}, index={}, candidates={}",
            filter.owner,
            filter.title_match,
            idx,
            filtered.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_window(id: u64, owner: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id,
            owner: owner.into(),
            title: title.into(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            layer: 0,
        }
    }

    #[test]
    fn pick_window_default_picks_titled_largest() {
        // title 空 + 面積大 vs title 有り + 面積中 → 後者が選ばれる
        let mut small_titled = sample_window(2, "vp-app", "main");
        small_titled.width = 800;
        small_titled.height = 600;
        let mut large_untitled = sample_window(1, "vp-app", "");
        large_untitled.width = 1920;
        large_untitled.height = 1080;
        let windows = vec![large_untitled, small_titled];
        let f = CaptureFilter {
            owner: "vp-app".into(),
            index: None,
            title_match: None,
        };
        let r = pick_window(&windows, &f).unwrap();
        assert_eq!(
            r.id, 2,
            "expected titled window to win over untitled larger"
        );
    }

    #[test]
    fn pick_window_default_picks_largest_among_titled() {
        let mut small = sample_window(1, "vp-app", "alpha");
        small.width = 800;
        small.height = 600;
        let mut large = sample_window(2, "vp-app", "beta");
        large.width = 1920;
        large.height = 1080;
        let windows = vec![small, large];
        let f = CaptureFilter {
            owner: "vp-app".into(),
            index: None,
            title_match: None,
        };
        let r = pick_window(&windows, &f).unwrap();
        assert_eq!(r.id, 2, "expected larger titled window");
    }

    #[test]
    fn pick_window_index_picks_nth() {
        let windows = vec![
            sample_window(1, "vp-app", "alpha"),
            sample_window(2, "vp-app", "beta"),
            sample_window(3, "vp-app", "gamma"),
        ];
        let f = CaptureFilter {
            owner: "vp-app".into(),
            index: Some(2),
            title_match: None,
        };
        let r = pick_window(&windows, &f).unwrap();
        assert_eq!(r.id, 3);
    }

    #[test]
    fn pick_window_title_match() {
        // 「vp」 部分一致 で filter、 「creo」 のみ除外、 残った 2 個のうち面積大が選ばれる
        let mut small_vp = sample_window(1, "vp-app", "vp-alpha");
        small_vp.width = 800;
        small_vp.height = 600;
        let mut large_vp = sample_window(2, "vp-app", "vp-beta-main");
        large_vp.width = 1920;
        large_vp.height = 1080;
        let mut not_vp = sample_window(3, "vp-app", "creo-memories");
        not_vp.width = 1500;
        not_vp.height = 900;
        let windows = vec![small_vp, not_vp, large_vp];
        let f = CaptureFilter {
            owner: "vp-app".into(),
            index: None,
            title_match: Some("vp".into()),
        };
        let r = pick_window(&windows, &f).unwrap();
        assert_eq!(r.id, 2, "expected larger 'vp'-titled window after filter");
    }

    #[test]
    fn pick_window_no_match_returns_error() {
        let windows = vec![sample_window(1, "vp-app", "alpha")];
        let f = CaptureFilter {
            owner: "vp-app".into(),
            index: None,
            title_match: Some("nomatch".into()),
        };
        assert!(pick_window(&windows, &f).is_err());
    }

    #[test]
    fn default_output_path_is_under_tmp() {
        let p = default_output_path();
        assert!(p.starts_with("/tmp"));
    }
}
