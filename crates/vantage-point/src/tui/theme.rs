//! TUI テーマ定数（Arctic Nord カラーパレット）

use ratatui::style::Color;

// Arctic Nord カラー定数
pub const NORD_BG: Color = Color::Rgb(11, 17, 32); // #0B1120
pub const NORD_FG: Color = Color::Rgb(216, 222, 233); // #D8DEE9
pub const NORD_CYAN: Color = Color::Rgb(136, 192, 208); // #88C0D0
pub const NORD_POLAR: Color = Color::Rgb(46, 52, 64); // #2E3440
pub const NORD_COMMENT: Color = Color::Rgb(76, 86, 106); // #4C566A
pub const NORD_GREEN: Color = Color::Rgb(163, 190, 140); // #A3BE8C
pub const NORD_RED: Color = Color::Rgb(191, 97, 106); // #BF616A
pub const NORD_YELLOW: Color = Color::Rgb(235, 203, 139); // #EBCB8B

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_colors_are_distinct() {
        // 主要色が互いに異なることを検証
        let colors = [NORD_BG, NORD_FG, NORD_CYAN, NORD_POLAR, NORD_GREEN, NORD_RED, NORD_YELLOW];
        for (i, a) in colors.iter().enumerate() {
            for (j, b) in colors.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "Color {} and {} should be distinct", i, j);
                }
            }
        }
    }
}
