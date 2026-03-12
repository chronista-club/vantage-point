//! TUI テーマ定数（Arctic Nord カラーパレット + Nerd Font アイコン）

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
pub const NORD_PURPLE: Color = Color::Rgb(180, 142, 173); // #B48EAD
pub const NORD_ORANGE: Color = Color::Rgb(208, 135, 112); // #D08770

// Nerd Font アイコン（Powerline + Stand メタファー）
// Nerd Font v3: Material Design = U+F0000+, Font Awesome = U+F000-F2E0
pub const NF_PL_RIGHT: &str = "\u{e0b0}"; //  パワーラインセパレータ（右向き）
pub const NF_PL_LEFT: &str = "\u{e0b2}"; //  パワーラインセパレータ（左向き）
pub const NF_STAR: &str = "\u{f04ce}"; // 󰓎 Star Platinum (nf-md-star)
pub const NF_COMPASS: &str = "\u{f018b}"; // 󰆋 Paisley Park / Canvas (nf-md-compass)
pub const NF_BOOK: &str = "\u{f14f7}"; // 󱓷 Heaven's Door / Claude CLI (nf-md-book_open_variant)
pub const NF_REFRESH: &str = "\u{f0450}"; // 󰑐 再接続 (nf-md-refresh)
pub const NF_ETHERNET: &str = "\u{f0200}"; // 󰈀 ポート (nf-md-ethernet)
pub const NF_HOME: &str = "\u{f02dc}"; // 󰋜 Home キー (nf-md-home)
pub const NF_SIGN_OUT: &str = "\u{f0343}"; // 󰍃 Detach (nf-md-logout)
pub const NF_ARROWS_V: &str = "\u{f04e2}"; // 󰓢 スクロール (nf-md-swap_vertical)
pub const NF_DIAMOND: &str = "\u{f01c8}"; // 󰇈 SP ロゴ (nf-md-diamond_stone — Star Platinum)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_colors_are_distinct() {
        // 主要色が互いに異なることを検証
        let colors = [
            NORD_BG,
            NORD_FG,
            NORD_CYAN,
            NORD_POLAR,
            NORD_GREEN,
            NORD_RED,
            NORD_YELLOW,
        ];
        for (i, a) in colors.iter().enumerate() {
            for (j, b) in colors.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "Color {} and {} should be distinct", i, j);
                }
            }
        }
    }
}
