import Foundation
import SwiftUI
import CreoUI

/// Design token の override store (VP-83 refinement 27)
///
/// ## 思想
///
/// creo-ui Editor + SwiftUI 連携の第一歩。CreoUI tokens は compile-time
/// static let 定数だが、UI 調整ループ (build / push / run の往復) は 25 回の
/// refinement でも非効率だったので、**runtime で token を slider 編集** できる
/// override store を提供する。
///
/// ## 動作
///
/// - Default 値は `CreoUITokens` に由来 (override 未設定なら CreoUITokens の値)
/// - override が set されていれば、View はそちらを優先して使う
/// - UserDefaults で永続化、次回起動時も残る
/// - `reset()` で全 override クリア → CreoUITokens の default に戻る
///
/// ## 将来
///
/// - creo-ui Editor (SolidJS + Live design surface) と protocol を共有し、
///   vp-bridge 経由で cross-platform token editing
/// - 納得した値を `tokens.json` → Style Dictionary build → CreoUITokens.swift
///   に export
@Observable
@MainActor
final class DesignTokenStore {
    static let shared = DesignTokenStore()

    // MARK: - Sidebar layout tokens

    /// Sidebar header の text leading padding (label 単体に適用、bg は edge 到達のまま)
    /// Default: CreoUITokens.spacingSm (8pt)
    var sidebarHeaderTextLeading: Double {
        didSet {
            UserDefaults.standard.set(sidebarHeaderTextLeading, forKey: Keys.sidebarHeaderTextLeading)
        }
    }

    /// Sidebar list row の leading inset (負値で system padding を食い込ませる)
    /// Default: -24
    var sidebarListRowLeadingInset: Double {
        didSet {
            UserDefaults.standard.set(sidebarListRowLeadingInset, forKey: Keys.sidebarListRowLeadingInset)
        }
    }

    /// Sidebar list row の trailing inset
    /// Default: -12
    var sidebarListRowTrailingInset: Double {
        didSet {
            UserDefaults.standard.set(sidebarListRowTrailingInset, forKey: Keys.sidebarListRowTrailingInset)
        }
    }

    // MARK: - Sidebar tint opacity (Project card の階層感を形成する 4 段階)

    /// Open Project card の base tint opacity
    /// Default: 0.08 (subtle 緑 area fill)
    var sidebarCardBaseOpacity: Double {
        didSet {
            UserDefaults.standard.set(sidebarCardBaseOpacity, forKey: Keys.sidebarCardBaseOpacity)
        }
    }

    /// Open Project header の extra tint opacity (base に重ねる overlay)
    /// Default: 0.14
    var sidebarHeaderOverlayOpacity: Double {
        didSet {
            UserDefaults.standard.set(sidebarHeaderOverlayOpacity, forKey: Keys.sidebarHeaderOverlayOpacity)
        }
    }

    /// Focused Lane row の tint opacity (最大コントラスト)
    /// Default: 0.28
    var sidebarLaneFocusOpacity: Double {
        didSet {
            UserDefaults.standard.set(sidebarLaneFocusOpacity, forKey: Keys.sidebarLaneFocusOpacity)
        }
    }

    // MARK: - Defaults

    private enum Defaults {
        static let sidebarHeaderTextLeading: Double = 8       // CreoUITokens.spacingSm
        static let sidebarListRowLeadingInset: Double = -24
        static let sidebarListRowTrailingInset: Double = -12
        // VP-83 refinement 28: user 要望「通常からこれくらい目立っててもいい」で
        // default 値を大幅アップ。緑系 semanticSuccess tint で selection を強調。
        static let sidebarCardBaseOpacity: Double = 0.18
        static let sidebarHeaderOverlayOpacity: Double = 0.32
        static let sidebarLaneFocusOpacity: Double = 0.55
    }

    private enum Keys {
        static let sidebarHeaderTextLeading = "vp.design.sidebar.headerTextLeading"
        static let sidebarListRowLeadingInset = "vp.design.sidebar.listRowLeadingInset"
        static let sidebarListRowTrailingInset = "vp.design.sidebar.listRowTrailingInset"
        static let sidebarCardBaseOpacity = "vp.design.sidebar.cardBaseOpacity"
        static let sidebarHeaderOverlayOpacity = "vp.design.sidebar.headerOverlayOpacity"
        static let sidebarLaneFocusOpacity = "vp.design.sidebar.laneFocusOpacity"
    }

    private init() {
        let ud = UserDefaults.standard
        self.sidebarHeaderTextLeading = ud.object(forKey: Keys.sidebarHeaderTextLeading) as? Double
            ?? Defaults.sidebarHeaderTextLeading
        self.sidebarListRowLeadingInset = ud.object(forKey: Keys.sidebarListRowLeadingInset) as? Double
            ?? Defaults.sidebarListRowLeadingInset
        self.sidebarListRowTrailingInset = ud.object(forKey: Keys.sidebarListRowTrailingInset) as? Double
            ?? Defaults.sidebarListRowTrailingInset
        self.sidebarCardBaseOpacity = ud.object(forKey: Keys.sidebarCardBaseOpacity) as? Double
            ?? Defaults.sidebarCardBaseOpacity
        self.sidebarHeaderOverlayOpacity = ud.object(forKey: Keys.sidebarHeaderOverlayOpacity) as? Double
            ?? Defaults.sidebarHeaderOverlayOpacity
        self.sidebarLaneFocusOpacity = ud.object(forKey: Keys.sidebarLaneFocusOpacity) as? Double
            ?? Defaults.sidebarLaneFocusOpacity
    }

    /// 全 token を default に戻す
    func reset() {
        sidebarHeaderTextLeading = Defaults.sidebarHeaderTextLeading
        sidebarListRowLeadingInset = Defaults.sidebarListRowLeadingInset
        sidebarListRowTrailingInset = Defaults.sidebarListRowTrailingInset
        sidebarCardBaseOpacity = Defaults.sidebarCardBaseOpacity
        sidebarHeaderOverlayOpacity = Defaults.sidebarHeaderOverlayOpacity
        sidebarLaneFocusOpacity = Defaults.sidebarLaneFocusOpacity
    }

    /// 現在の override 値を JSON で export (将来 Style Dictionary に渡す用)
    func exportJSON() -> String {
        let dict: [String: Double] = [
            "sidebar.headerTextLeading": sidebarHeaderTextLeading,
            "sidebar.listRowLeadingInset": sidebarListRowLeadingInset,
            "sidebar.listRowTrailingInset": sidebarListRowTrailingInset,
            "sidebar.cardBaseOpacity": sidebarCardBaseOpacity,
            "sidebar.headerOverlayOpacity": sidebarHeaderOverlayOpacity,
            "sidebar.laneFocusOpacity": sidebarLaneFocusOpacity,
        ]
        let data = try? JSONSerialization.data(withJSONObject: dict, options: .prettyPrinted)
        return data.flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
    }
}
