//! frame list → 1 枚の grid / strip 画像 に合成する utility。
//!
//! `vp shot --series --layout 4x4` 等の compose 段で使う。
//! pure な image manipulation (capture backend に依存しない) なので独立 module。

use std::path::{Path, PathBuf};

use image::{DynamicImage, GenericImage, GenericImageView, ImageBuffer, Rgba};

/// layout 指定: matrix(cols, rows) / vertical / horizontal
#[derive(Debug, Clone, Copy)]
pub enum Layout {
    /// `cols × rows` の grid
    Matrix { cols: u32, rows: u32 },
    /// 縦 1 列 (= 1 × N)
    Vertical,
    /// 横 1 行 (= N × 1)
    Horizontal,
}

impl Layout {
    /// 文字列から parse:
    /// `"4x4"` / `"3x5"` → Matrix
    /// `"vertical"` / `"v"` → Vertical
    /// `"horizontal"` / `"h"` → Horizontal
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_lowercase().as_str() {
            "vertical" | "v" => Ok(Layout::Vertical),
            "horizontal" | "h" => Ok(Layout::Horizontal),
            other => {
                let parts: Vec<&str> = other.split('x').collect();
                if parts.len() != 2 {
                    return Err(format!(
                        "invalid layout '{}': expected 'CxR' / 'vertical' / 'horizontal'",
                        s
                    ));
                }
                let cols: u32 = parts[0]
                    .parse()
                    .map_err(|e| format!("layout cols: {}", e))?;
                let rows: u32 = parts[1]
                    .parse()
                    .map_err(|e| format!("layout rows: {}", e))?;
                if cols == 0 || rows == 0 {
                    return Err(format!("layout dims must be > 0 (got {}x{})", cols, rows));
                }
                Ok(Layout::Matrix { cols, rows })
            }
        }
    }

    /// frame_count に対し (cols, rows) を解決。 Matrix はそのまま、 Vertical/Horizontal は count から計算。
    pub fn resolve(&self, frame_count: u32) -> (u32, u32) {
        match *self {
            Layout::Matrix { cols, rows } => (cols, rows),
            Layout::Vertical => (1, frame_count),
            Layout::Horizontal => (frame_count, 1),
        }
    }
}

/// frame の path list を layout に従って 1 枚の image に合成、 output に PNG 保存。
///
/// 全 frame が同 size 前提 (vp shot --series で同 Rect から撮ってるので保証済)。
/// layout が Matrix(cols, rows) で `cols * rows < frames.len()` の場合、 余り frame は無視。
/// 余り cell (frames.len() < cols * rows) は黒で残る。
pub fn compose(
    frames: &[PathBuf],
    layout: Layout,
    output: &Path,
) -> Result<(u32, u32), String> {
    if frames.is_empty() {
        return Err("compose: no frames provided".into());
    }
    let (cols, rows) = layout.resolve(frames.len() as u32);

    // 1 frame 目で size を決定 (全 frame 同 size 前提)
    let first =
        image::open(&frames[0]).map_err(|e| format!("open {}: {}", frames[0].display(), e))?;
    let (frame_w, frame_h) = first.dimensions();

    let total_w = cols * frame_w;
    let total_h = rows * frame_h;
    let mut canvas: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(total_w, total_h);

    let max_frames = (cols * rows) as usize;
    for (i, frame_path) in frames.iter().take(max_frames).enumerate() {
        let img = if i == 0 {
            first.clone()
        } else {
            image::open(frame_path)
                .map_err(|e| format!("open {}: {}", frame_path.display(), e))?
        };
        let col = i as u32 % cols;
        let row = i as u32 / cols;
        let x = col * frame_w;
        let y = row * frame_h;
        canvas
            .copy_from(&img.to_rgba8(), x, y)
            .map_err(|e| format!("copy_from frame {}: {}", i, e))?;
    }

    // 親 dir 作成
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }

    DynamicImage::ImageRgba8(canvas)
        .save_with_format(output, image::ImageFormat::Png)
        .map_err(|e| format!("save {}: {}", output.display(), e))?;
    Ok((total_w, total_h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_parse_matrix() {
        match Layout::parse("4x4").unwrap() {
            Layout::Matrix { cols, rows } => assert_eq!((cols, rows), (4, 4)),
            other => panic!("expected Matrix, got {:?}", other),
        }
    }

    #[test]
    fn layout_parse_vertical() {
        assert!(matches!(Layout::parse("vertical").unwrap(), Layout::Vertical));
        assert!(matches!(Layout::parse("v").unwrap(), Layout::Vertical));
    }

    #[test]
    fn layout_parse_horizontal() {
        assert!(matches!(
            Layout::parse("horizontal").unwrap(),
            Layout::Horizontal
        ));
        assert!(matches!(Layout::parse("h").unwrap(), Layout::Horizontal));
    }

    #[test]
    fn layout_parse_rejects_invalid() {
        assert!(Layout::parse("0x4").is_err());
        assert!(Layout::parse("4x0").is_err());
        assert!(Layout::parse("garbage").is_err());
        assert!(Layout::parse("4x4x4").is_err());
    }

    #[test]
    fn layout_resolve_matrix_ignores_count() {
        let l = Layout::parse("3x5").unwrap();
        assert_eq!(l.resolve(99), (3, 5));
    }

    #[test]
    fn layout_resolve_vertical() {
        assert_eq!(Layout::Vertical.resolve(8), (1, 8));
    }

    #[test]
    fn layout_resolve_horizontal() {
        assert_eq!(Layout::Horizontal.resolve(8), (8, 1));
    }
}
