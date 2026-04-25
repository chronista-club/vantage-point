//! vp-bridge 内部表現の型定義
//!
//! 設計方針 (2026-04-25 ratatui 脱却 + ultrathink simplification):
//! 中間表現を廃止し、alacritty grid → `CellData` → Swift FFI の直線パスのみで運用。
//! `CellData` は #[repr(C)] で FFI 互換、同時に内部バッファの要素型でもある。

use std::str;

/// セル単位の文字 + スタイル情報。FFI 互換 (#[repr(C)]) + 内部表現を兼ねる
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CellData {
    /// UTF-8 encoded char (max 4 byte) + null terminator
    pub ch: [u8; 5],
    /// 前景色 RGBA (0xRRGGBBAA, alpha=0 でデフォルト透明扱い)
    pub fg: u32,
    /// 背景色 RGBA
    pub bg: u32,
    /// 修飾子 bit flags (`flags::BOLD` 等の OR)
    pub flags: u8,
}

impl Default for CellData {
    fn default() -> Self {
        Self {
            ch: [b' ', 0, 0, 0, 0],
            fg: 0,
            bg: 0,
            flags: 0,
        }
    }
}

impl CellData {
    /// 文字をセット (char → UTF-8 encode)
    pub fn set_char(&mut self, c: char) {
        let mut buf = [0u8; 5];
        let _ = c.encode_utf8(&mut buf[..4]);
        self.ch = buf;
    }

    /// シンボルを &str として取得 (null 終端で切り詰め)
    pub fn symbol_str(&self) -> &str {
        let len = self
            .ch
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.ch.len());
        str::from_utf8(&self.ch[..len]).unwrap_or(" ")
    }
}

/// 修飾子・状態 bit flags (`CellData.flags` で使う)
pub mod flags {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINED: u8 = 1 << 2;
    pub const REVERSED: u8 = 1 << 3;
    pub const CROSSED_OUT: u8 = 1 << 4;
    pub const DIM: u8 = 1 << 5;
    /// VT パーサー由来のワイドキャラクター flag
    pub const WIDE: u8 = 1 << 6;
}

/// `Rgb(r,g,b)` → `0xRRGGBBAA` (alpha=0xFF)。`Rgb(0,0,0)` は alpha=0 で透明扱い (デフォルト)
pub const fn rgb_to_rgba(r: u8, g: u8, b: u8) -> u32 {
    if r == 0 && g == 0 && b == 0 {
        0
    } else {
        ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 0xFF
    }
}

/// グリッド上の位置 (x, y)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Position {
    pub x: u16,
    pub y: u16,
}

impl Position {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// グリッドサイズ (width, height)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl Size {
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celldata_default_is_space() {
        let c = CellData::default();
        assert_eq!(c.symbol_str(), " ");
        assert_eq!(c.fg, 0);
        assert_eq!(c.bg, 0);
        assert_eq!(c.flags, 0);
    }

    #[test]
    fn celldata_set_char() {
        let mut c = CellData::default();
        c.set_char('漢');
        assert_eq!(c.symbol_str(), "漢");
    }

    #[test]
    fn celldata_set_char_emoji_4byte() {
        let mut c = CellData::default();
        c.set_char('🎉');
        assert_eq!(c.symbol_str(), "🎉");
    }

    #[test]
    fn rgba_conversion() {
        assert_eq!(rgb_to_rgba(255, 0, 128), 0xFF0080FF);
        assert_eq!(rgb_to_rgba(0, 0, 0), 0); // 透明扱い
    }

    #[test]
    fn flag_bits_unique() {
        let bits = [
            flags::BOLD,
            flags::ITALIC,
            flags::UNDERLINED,
            flags::REVERSED,
            flags::CROSSED_OUT,
            flags::DIM,
            flags::WIDE,
        ];
        for (i, &a) in bits.iter().enumerate() {
            assert!(a.is_power_of_two());
            for &b in &bits[i + 1..] {
                assert_eq!(a & b, 0);
            }
        }
    }
}
