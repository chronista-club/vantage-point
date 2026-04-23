import Foundation
import SwiftUI

/// VP App 全体の theme system (VP-83 refinement 29)
///
/// creo-ui の 8 preset + VP オリジナル 1 つ = 9 theme を runtime 切替可能に。
/// Phase A MVP: 3 theme + Sidebar 主要色のみ theme 経由化。
///
/// Phase B で残り 6 theme (contrast / oldschool / sora × light/dark) 移植、
/// Phase C で全 VP View を theme 対応予定。
///
/// 色値は creo-ui の tokens/color/themes/*.json (oklch) を近似 sRGB に変換して
/// hardcode。将来 Style Dictionary を拡張して Swift 自動生成にする。
struct VPTheme: Identifiable, Equatable, Hashable {
    let id: String
    let displayName: String
    let isDark: Bool
    let palette: ThemePalette

    static func == (lhs: VPTheme, rhs: VPTheme) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

/// Theme で切り替える palette (最小セット、MVP)
struct ThemePalette: Equatable {
    // Semantic
    let semanticSuccess: Color
    let semanticSuccessSubtle: Color
    let semanticWarning: Color

    // Surface
    let surfaceBgBase: Color
    let surfaceBgSubtle: Color
    let surfaceBgEmphasis: Color
    let surface: Color
    let surfaceBorder: Color
    let surfaceBorderSubtle: Color

    // Text
    let textPrimary: Color
    let textSecondary: Color
    let textTertiary: Color
}

// MARK: - Palette definitions (9 theme)

extension ThemePalette {
    /// creo-dark — 現状 CreoUITokens.swift 相当 (navy blue base)
    static let creoDark = ThemePalette(
        semanticSuccess: Color(red: 0.3569, green: 0.7137, blue: 0.3804),
        semanticSuccessSubtle: Color(red: 0.0431, green: 0.1608, blue: 0.0549),
        semanticWarning: Color(red: 0.9216, green: 0.6588, blue: 0.2980),
        surfaceBgBase: Color(red: 0.0275, green: 0.0431, blue: 0.0784),
        surfaceBgSubtle: Color(red: 0.0471, green: 0.0706, blue: 0.1020),
        surfaceBgEmphasis: Color(red: 0.1373, green: 0.1608, blue: 0.2000),
        surface: Color(red: 0.0667, green: 0.0863, blue: 0.1216),
        surfaceBorder: Color(red: 0.1569, green: 0.1804, blue: 0.2196),
        surfaceBorderSubtle: Color(red: 0.1137, green: 0.1333, blue: 0.1608),
        textPrimary: Color(red: 0.9294, green: 0.9373, blue: 0.9490),
        textSecondary: Color(red: 0.6784, green: 0.6863, blue: 0.7020),
        textTertiary: Color(red: 0.4902, green: 0.5020, blue: 0.5255)
    )

    /// mint-dark — creo-ui "mint" family、mint green base
    /// oklch(0.15 0.02 260) 等を sRGB 近似
    static let mintDark = ThemePalette(
        semanticSuccess: Color(red: 0.42, green: 0.76, blue: 0.52),
        semanticSuccessSubtle: Color(red: 0.08, green: 0.18, blue: 0.10),
        semanticWarning: Color(red: 0.85, green: 0.62, blue: 0.26),
        surfaceBgBase: Color(red: 0.08, green: 0.11, blue: 0.13),
        surfaceBgSubtle: Color(red: 0.11, green: 0.14, blue: 0.16),
        surfaceBgEmphasis: Color(red: 0.20, green: 0.23, blue: 0.25),
        surface: Color(red: 0.13, green: 0.16, blue: 0.18),
        surfaceBorder: Color(red: 0.22, green: 0.25, blue: 0.27),
        surfaceBorderSubtle: Color(red: 0.17, green: 0.20, blue: 0.22),
        textPrimary: Color(red: 0.93, green: 0.94, blue: 0.95),
        textSecondary: Color(red: 0.71, green: 0.72, blue: 0.73),
        textTertiary: Color(red: 0.56, green: 0.57, blue: 0.58)
    )

    /// vantage-dark — VP オリジナル (JoJo メタファー × AI ネイティブ)
    /// 深い紫夜 + 鮮やかな緑 accent で Mountain icon と呼応
    static let vantageDark = ThemePalette(
        semanticSuccess: Color(red: 0.20, green: 0.85, blue: 0.48),   // より鮮やかな緑
        semanticSuccessSubtle: Color(red: 0.03, green: 0.15, blue: 0.08),
        semanticWarning: Color(red: 1.00, green: 0.60, blue: 0.25),    // 山の夕焼け orange
        surfaceBgBase: Color(red: 0.04, green: 0.03, blue: 0.08),      // より深い夜紫
        surfaceBgSubtle: Color(red: 0.07, green: 0.06, blue: 0.12),
        surfaceBgEmphasis: Color(red: 0.16, green: 0.14, blue: 0.24),  // 紫寄り
        surface: Color(red: 0.09, green: 0.08, blue: 0.15),
        surfaceBorder: Color(red: 0.20, green: 0.18, blue: 0.28),
        surfaceBorderSubtle: Color(red: 0.14, green: 0.12, blue: 0.20),
        textPrimary: Color(red: 0.97, green: 0.96, blue: 0.99),
        textSecondary: Color(red: 0.75, green: 0.73, blue: 0.80),
        textTertiary: Color(red: 0.55, green: 0.52, blue: 0.62)
    )
}

// MARK: - Theme catalog

extension VPTheme {
    static let creoDark = VPTheme(
        id: "creo-dark",
        displayName: "Creo Dark",
        isDark: true,
        palette: .creoDark
    )

    static let mintDark = VPTheme(
        id: "mint-dark",
        displayName: "Mint Dark",
        isDark: true,
        palette: .mintDark
    )

    static let vantageDark = VPTheme(
        id: "vantage-dark",
        displayName: "Vantage Dark ⛰",
        isDark: true,
        palette: .vantageDark
    )

    /// 利用可能な全 theme (MVP: 3 つ、Phase B で 9 つに拡張)
    static let all: [VPTheme] = [.vantageDark, .creoDark, .mintDark]
}

// MARK: - Theme Manager

/// Runtime で theme を切替・永続化する manager (@Observable)
@Observable
@MainActor
final class ThemeManager {
    static let shared = ThemeManager()

    private static let userDefaultsKey = "vp.theme.id"

    /// 現在選択中の theme ID
    var currentId: String {
        didSet {
            UserDefaults.standard.set(currentId, forKey: Self.userDefaultsKey)
        }
    }

    /// 利用可能な全 theme
    let available: [VPTheme] = VPTheme.all

    /// 現在の theme (id で lookup、見つからなければ vantage-dark fallback)
    var current: VPTheme {
        available.first { $0.id == currentId } ?? .vantageDark
    }

    /// 現在の palette (shortcut accessor)
    var palette: ThemePalette { current.palette }

    private init() {
        self.currentId = UserDefaults.standard.string(forKey: Self.userDefaultsKey) ?? VPTheme.vantageDark.id
    }

    /// Theme を切替える
    func select(_ theme: VPTheme) {
        currentId = theme.id
    }
}
