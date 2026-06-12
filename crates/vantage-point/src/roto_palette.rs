//! ROTO-CONTROL の固定色パレット（83 色）。
//!
//! 出典: `Roto-Control.bwextension`（Melbourne 公式 Bitwig 拡張）の `ColorUtil.COLORS` を
//! CFR で decompile して抽出（2026-06-12）。値は decompile 原文の順序・表記を忠実に保持する
//! （10 進・16 進が混在しているのは原データ由来。Rust ではどちらも `u32` リテラルとして有効）。
//!
//! ROTO は任意 RGB を受け付けない。host 側で最近傍パレット index に量子化し、
//! track / parameter の state 更新メッセージに `colorIndex` として同梱する
//! （`docs/design/20-roto-control-sysex-protocol.md` §4 参照）。
//!
//! External Control track E1（`DeviceProfile` trait）の ROTO impl で使用予定の先行データ。

/// ROTO 固定パレット（index → `0xRRGGBB`）。index がそのまま SysEx の `colorIndex`。
///
/// `TrackState` のデフォルト `colorIndex` = 70（= `0x000000` 黒 = track 未割当時の表示）。
pub const ROTO_PALETTE: [u32; 83] = [
    16749734, 16753961, 13408550, 16184445, 0xBFFB00, 2031406, 2686888, 6094824, 9160191, 5538020,
    9611263, 14183652, 15029152, 0xFFFFFF, 16725302, 16149507, 10051915, 14800173, 8912744,
    4113155, 180143, 1632767, 1025262, 163264, 9006308, 11958214, 16726484, 0xD0D0D0, 14968922,
    16753524, 13872497, 15597486, 13821080, 12243060, 10208397, 13958625, 13496824, 12108259,
    13482980, 11442405, 15064289, 0xA9A9A9, 15110795, 12026454, 9995114, 12565098, 10993152,
    9028282, 9879994, 10269636, 8758727, 8622797, 10851765, 12558270, 12349845, 0x7B7B7B, 0xAF3333,
    11096369, 7491393, 14402304, 8754463, 5480241, 564366, 2253700, 1715862, 3101346, 6376365,
    10701741, 13381229, 0x3C3C3C, 0, 0xFF0000, 261888, 0xFFFF00, 255, 0xFF00FF, 262143, 0x800000,
    0x808000, 32770, 32896, 128, 0x800080,
];

/// RGB（各 0–255）を最近傍パレット index に量子化する。
///
/// `ColorUtil.findClosestColor` 等価。元実装はユークリッド距離（`sqrt`）だが、
/// 最小値を取る index は二乗距離でも同一なので `sqrt` を省く。
pub fn closest_index(r: u8, g: u8, b: u8) -> u8 {
    let mut best_index = 0usize;
    let mut best_dist = i64::MAX;
    for (index, &color) in ROTO_PALETTE.iter().enumerate() {
        let cr = ((color >> 16) & 0xFF) as i64;
        let cg = ((color >> 8) & 0xFF) as i64;
        let cb = (color & 0xFF) as i64;
        let dist = (r as i64 - cr).pow(2) + (g as i64 - cg).pow(2) + (b as i64 - cb).pow(2);
        if dist < best_dist {
            best_dist = dist;
            best_index = index;
        }
    }
    best_index as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_has_83_entries() {
        assert_eq!(ROTO_PALETTE.len(), 83);
    }

    #[test]
    fn default_color_index_70_is_black() {
        // TrackState のデフォルト colorIndex = 70 は黒（未割当表示）
        assert_eq!(ROTO_PALETTE[70], 0x000000);
    }

    #[test]
    fn pure_red_quantizes_to_palette_red() {
        let index = closest_index(255, 0, 0);
        assert_eq!(ROTO_PALETTE[index as usize], 0xFF0000);
    }

    #[test]
    fn pure_white_quantizes_to_palette_white() {
        let index = closest_index(255, 255, 255);
        assert_eq!(ROTO_PALETTE[index as usize], 0xFFFFFF);
    }
}
