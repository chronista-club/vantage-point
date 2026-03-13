import AppKit
import CoreText
import VPBridge

// MARK: - セッションレジストリ（マルチウィンドウ対応）

/// C コールバックから TerminalView を逆引きするための静的レジストリ
///
/// C の関数ポインタは Swift closure をキャプチャできないため、
/// session ID をキーとした辞書で「どのウィンドウを再描画するか」を解決する。
@MainActor
private var sessionRegistry: [UInt32: TerminalView] = [:]

/// C コールバック用の関数ポインタ
///
/// vp_bridge_create に渡す唯一のコールバック。
/// 呼ばれたら全登録済み TerminalView を再描画する。
/// NOTE: セッション固有コールバックが Rust 側で未対応のため、
///       全ビューをインバリデートする（軽量: setNeedsDisplay は O(1)）
private let sharedFrameCallback: VPFrameReadyCallback = {
    DispatchQueue.main.async {
        for (_, view) in sessionRegistry {
            view.needsDisplay = true
        }
    }
}

// MARK: - TerminalView

/// ratatui NativeBackend の Cell グリッドを Core Text で描画する NSView
///
/// vp-bridge (Rust) が保持する Cell バッファを読み取り、
/// 各セルの文字・色・スタイルを Core Text で描画する。
/// フレーム更新コールバックで setNeedsDisplay が呼ばれ、
/// macOS のディスプレイサイクルに合わせて再描画される。
@MainActor
class TerminalView: NSView {

    // MARK: - 設定

    /// コンソールフォント名（等幅 — Nerd Font Mono）
    var fontName: String = "FiraCode Nerd Font Mono" {
        didSet { updateFontMetrics() }
    }

    /// フォントフォールバックチェーン
    private static let consoleFontChain = [
        "FiraCode Nerd Font Mono",  // Primary: 等幅 Nerd Font
        "FiraCode Nerd Font",       // Fallback: 非 Mono（アイコン本来幅）
        "Menlo",                    // Last resort
    ]

    /// フォントサイズ（コンソール用: 16pt）
    var fontSize: CGFloat = 16.0 {
        didSet { updateFontMetrics() }
    }

    /// 行の高さ倍率（1.0 = フォントメトリクスそのまま、1.3 = 130%）
    var lineHeightMultiplier: CGFloat = 1.3 {
        didSet { updateFontMetrics() }
    }

    /// デフォルト前景色（Reset 時に使用）
    var defaultForeground: NSColor = .labelColor

    /// デフォルト背景色（Reset 時に使用）
    var defaultBackground: NSColor = NSColor(red: 0.12, green: 0.12, blue: 0.14, alpha: 1.0)

    // MARK: - 内部状態

    /// Rust 側のセッション ID（0 = 未初期化）
    private(set) var sessionId: UInt32 = 0

    /// 外部からセッション ID を参照するアクセサ
    var currentSessionId: UInt32 { sessionId }

    /// 現在のフォント
    private var font: CTFont!

    /// Bold フォント
    private var boldFont: CTFont!

    /// Italic フォント
    private var italicFont: CTFont!

    /// Bold Italic フォント
    private var boldItalicFont: CTFont!

    /// CJK フォールバック（日本語用）
    private var cjkFont: CTFont!
    private var cjkBoldFont: CTFont!

    /// 絵文字フォールバック
    private var emojiFont: CTFont!

    /// セル幅（ピクセル）
    private var cellWidth: CGFloat = 0

    /// セル高さ（ピクセル）
    private var cellHeight: CGFloat = 0

    /// ベースラインオフセット
    private var baselineOffset: CGFloat = 0

    /// グリッドサイズ（列数）
    private var gridCols: UInt16 = 80

    /// グリッドサイズ（行数）
    private var gridRows: UInt16 = 24

    /// セルデータバッファ（バッチ読み取り用）
    private var cellBuffer: [VPCellData] = []

    /// Bridge 初期化済みフラグ
    private var bridgeInitialized: Bool { sessionId != 0 }

    // MARK: - 初期化

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        commonInit()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        commonInit()
    }

    private func commonInit() {
        wantsLayer = true
        layer?.backgroundColor = defaultBackground.cgColor

        updateFontMetrics()
    }

    /// ウィンドウに追加されたら自動的に first responder を取得
    ///
    /// NSViewRepresentable 経由で配置された場合、SwiftUI が first responder を
    /// 制御するため、明示的に要求しないとキー入力を受け取れない。
    /// SwiftUI のレイアウトパスがフォーカスを奪い返すため、複数回リトライする。
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard let window = self.window else { return }
        // SwiftUI のレイアウトサイクル完了を待って複数回リトライ
        for delay in [0.05, 0.1, 0.3, 0.5] {
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                guard let self, self.window != nil else { return }
                window.makeFirstResponder(self)
            }
        }
    }

    /// NSView が first responder を受け入れ可能であることを明示
    override var acceptsFirstResponder: Bool { true }

    /// キーウィンドウ外でもキー入力を受け取る
    override var needsPanelToBecomeKey: Bool { true }

    deinit {
        if sessionId != 0 {
            let sid = sessionId
            // deinit は nonisolated — MainActor 外から呼ばれる可能性がある
            // レジストリ解除を安全にディスパッチし、bridge 破棄はその後に実行
            DispatchQueue.main.async {
                sessionRegistry.removeValue(forKey: sid)
                vp_bridge_destroy(sid)
            }
        }
    }

    // MARK: - Bridge セットアップ

    /// Bridge セッションを作成してフレームコールバックを登録
    func setupBridge() {
        guard sessionId == 0 else { return }

        calculateGridSize()

        // セッション作成（Rust 側で Backend + PTY スロットが確保される）
        let id = vp_bridge_create(gridCols, gridRows, sharedFrameCallback)
        guard id != 0 else {
            NSLog("[VP] vp_bridge_create failed")
            return
        }

        sessionId = id
        sessionRegistry[id] = self
        NSLog("[VP] Bridge session created: %d (%dx%d)", id, gridCols, gridRows)

        // バッファを確保
        let totalCells = Int(gridCols) * Int(gridRows)
        cellBuffer = [VPCellData](repeating: VPCellData(), count: totalCells)
    }

    // MARK: - フォント

    private func updateFontMetrics() {
        // フォールバックチェーンで最初に見つかるフォントを使用
        var resolvedFont: NSFont?
        for name in Self.consoleFontChain {
            if let f = NSFont(name: name, size: fontSize) {
                resolvedFont = f
                break
            }
        }
        let nsFont = resolvedFont ?? NSFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
        font = nsFont as CTFont

        NSLog("[VP] Font resolved: %@ (size: %.1f)", nsFont.fontName, fontSize)

        // Bold / Italic バリアント
        let boldTraits: CTFontSymbolicTraits = .boldTrait
        let italicTraits: CTFontSymbolicTraits = .italicTrait
        let boldItalicTraits: CTFontSymbolicTraits = [.boldTrait, .italicTrait]

        boldFont = CTFontCreateCopyWithSymbolicTraits(font, fontSize, nil, boldTraits, boldTraits) ?? font
        italicFont = CTFontCreateCopyWithSymbolicTraits(font, fontSize, nil, italicTraits, italicTraits) ?? font
        boldItalicFont = CTFontCreateCopyWithSymbolicTraits(font, fontSize, nil, boldItalicTraits, boldItalicTraits) ?? font

        // CJK フォールバック（日本語グリフ用）
        let cjkFontNames = ["HiraginoSans-W3", "HiraKakuProN-W3"]
        var cjkResolved: NSFont?
        for name in cjkFontNames {
            if let f = NSFont(name: name, size: fontSize) {
                cjkResolved = f
                break
            }
        }
        cjkFont = (cjkResolved ?? NSFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)) as CTFont
        cjkBoldFont = CTFontCreateCopyWithSymbolicTraits(cjkFont, fontSize, nil, boldTraits, boldTraits) ?? cjkFont
        NSLog("[VP] CJK font: %@", CTFontCopyPostScriptName(cjkFont) as String)

        // 絵文字フォールバック
        emojiFont = (NSFont(name: "AppleColorEmoji", size: fontSize) ?? nsFont) as CTFont

        // セルサイズ計算 — 'M' グリフのアドバンスで計測（等幅フォントなら全 ASCII 同じ）
        var testChar: UniChar = 0x4D // 'M'
        var glyph: CGGlyph = 0
        CTFontGetGlyphsForCharacters(font, &testChar, &glyph, 1)
        var advance: CGSize = .zero
        CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, &advance, 1)
        cellWidth = advance.width

        let naturalHeight = CTFontGetAscent(font) + CTFontGetDescent(font) + CTFontGetLeading(font)
        cellHeight = naturalHeight * lineHeightMultiplier
        baselineOffset = CTFontGetDescent(font) + (cellHeight - naturalHeight) / 2.0

        NSLog("[VP] Cell metrics: width=%.2f height=%.2f (natural=%.2f, multiplier=%.1f) baseline=%.2f",
              cellWidth, cellHeight, naturalHeight, lineHeightMultiplier, baselineOffset)

        // セルサイズが 0 の場合のフォールバック
        if cellWidth <= 0 { cellWidth = fontSize * 0.6 }
        if cellHeight <= 0 { cellHeight = fontSize * 1.2 }

        if bridgeInitialized {
            calculateGridSize()
            vp_bridge_resize_session(sessionId, gridCols, gridRows)
            let totalCells = Int(gridCols) * Int(gridRows)
            cellBuffer = [VPCellData](repeating: VPCellData(), count: totalCells)
        }

        needsDisplay = true
    }

    // MARK: - レイアウト

    private func calculateGridSize() {
        let viewWidth = bounds.width
        let viewHeight = bounds.height

        guard cellWidth > 0, cellHeight > 0 else { return }

        gridCols = max(1, UInt16(viewWidth / cellWidth))
        gridRows = max(1, UInt16(viewHeight / cellHeight))
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)

        let oldCols = gridCols
        let oldRows = gridRows

        calculateGridSize()

        if bridgeInitialized && (gridCols != oldCols || gridRows != oldRows) {
            vp_bridge_resize_session(sessionId, gridCols, gridRows)
            let totalCells = Int(gridCols) * Int(gridRows)
            cellBuffer = [VPCellData](repeating: VPCellData(), count: totalCells)
        }
    }

    // MARK: - 描画

    override func draw(_ dirtyRect: NSRect) {
        guard let ctx = NSGraphicsContext.current?.cgContext else { return }
        guard bridgeInitialized else {
            // Bridge 未初期化時はデフォルト背景のみ
            ctx.setFillColor(defaultBackground.cgColor)
            ctx.fill(bounds)
            return
        }

        // バッファ一括読み取り
        let totalCells = Int(gridCols) * Int(gridRows)
        guard totalCells > 0 else { return }

        cellBuffer.withUnsafeMutableBufferPointer { ptr in
            _ = vp_bridge_get_buffer_session(sessionId, ptr.baseAddress!, UInt32(totalCells))
        }

        // 背景色をまとめて描画（パフォーマンス最適化）
        ctx.setFillColor(defaultBackground.cgColor)
        ctx.fill(bounds)

        // Retina ピクセルスナップ: backing scale factor を使って
        // 物理ピクセル境界に正確にアラインすることで行間の隙間を排除
        let scale = window?.backingScaleFactor ?? 2.0
        let cols = Int(gridCols)
        let rows = Int(gridRows)

        // 各セルを描画
        for row in 0..<rows {
            // Y 座標: 物理ピクセル境界にスナップして隙間を完全排除
            let y = floor((bounds.height - CGFloat(row + 1) * cellHeight) * scale) / scale
            let yNext = floor((bounds.height - CGFloat(row) * cellHeight) * scale) / scale
            let rowHeight = yNext - y

            for col in 0..<cols {
                let idx = row * cols + col
                guard idx < cellBuffer.count else { continue }

                let cell = cellBuffer[idx]
                // X 座標: 物理ピクセル境界にスナップ
                let x = floor(CGFloat(col) * cellWidth * scale) / scale
                let xNext = floor(CGFloat(col + 1) * cellWidth * scale) / scale
                let colWidth = xNext - x

                // 文字を取得（背景幅の計算にも必要）
                let ch = cellString(from: cell)

                // 全角文字かどうか判定（背景を 2 セル幅で描画する）
                let charIsFullWidth = ch.unicodeScalars.first.map { isFullWidth($0) } ?? false

                // 背景色（デフォルト以外の場合のみ描画）
                let bgColor = colorFromRGBA(cell.bg)
                if cell.bg != 0 { // 0 = 透明（デフォルト）
                    let bgWidth = charIsFullWidth ? colWidth * 2 : colWidth
                    ctx.setFillColor(bgColor.cgColor)
                    ctx.fill(CGRect(x: x, y: y, width: bgWidth, height: rowHeight))
                }

                guard !ch.isEmpty, ch != " " else { continue }

                // Box-drawing 文字はフォントグリフではなく CGContext で直接描画
                // lineHeightMultiplier でセルが膨らんでもセル境界にピッタリ合う
                if let scalar = ch.unicodeScalars.first,
                   drawBoxCharacter(scalar, in: ctx,
                                    cellRect: CGRect(x: x, y: y, width: colWidth, height: rowHeight),
                                    color: (cell.fg != 0 ? colorFromRGBA(cell.fg) : defaultForeground).cgColor,
                                    isBold: (cell.flags & (1 << 0)) != 0) {
                    continue
                }

                // フォント選択
                let isBold = (cell.flags & (1 << 0)) != 0
                let isItalic = (cell.flags & (1 << 1)) != 0
                let isUnderline = (cell.flags & (1 << 2)) != 0
                let isInverse = (cell.flags & (1 << 3)) != 0
                let isDim = (cell.flags & (1 << 5)) != 0

                let selectedFont: CTFont
                if isBold && isItalic {
                    selectedFont = boldItalicFont
                } else if isBold {
                    selectedFont = boldFont
                } else if isItalic {
                    selectedFont = italicFont
                } else {
                    selectedFont = font
                }

                // 色（inverse 対応）
                var fgColor = cell.fg != 0 ? colorFromRGBA(cell.fg) : defaultForeground
                var effectiveBg = cell.bg != 0 ? colorFromRGBA(cell.bg) : defaultBackground

                if isInverse {
                    swap(&fgColor, &effectiveBg)
                    // inverse 時は背景も描画（全角は 2 セル幅）
                    let invWidth = charIsFullWidth ? colWidth * 2 : colWidth
                    ctx.setFillColor(effectiveBg.cgColor)
                    ctx.fill(CGRect(x: x, y: y, width: invWidth, height: rowHeight))
                }

                if isDim {
                    fgColor = fgColor.withAlphaComponent(0.5)
                }

                // グリフを直接描画（フォールバック対応）
                ctx.setFillColor(fgColor.cgColor)
                let chars = Array(ch.utf16)
                var glyphs = [CGGlyph](repeating: 0, count: chars.count)
                let found = CTFontGetGlyphsForCharacters(selectedFont, chars, &glyphs, chars.count)

                // フォントフォールバックチェーン:
                //   1. プライマリフォント（Nerd Font Mono）
                //   2. 絵文字 → Apple Color Emoji
                //   3. Nerd Font シンボル → CTFontCreateForString（システムフォールバック）
                //   4. CJK 文字 → CJK フォールバックフォント
                //   5. その他 → CTFontCreateForString（最終手段）
                let drawFont: CTFont
                let firstScalar = ch.unicodeScalars.first
                let isNerd = firstScalar.map { isNerdFontSymbol($0) } ?? false
                let isEmojiChar = firstScalar.map { isEmoji($0) } ?? false

                if !found || glyphs.contains(0) {
                    if isEmojiChar {
                        // 絵文字 → Apple Color Emoji
                        var fbGlyphs = [CGGlyph](repeating: 0, count: chars.count)
                        if CTFontGetGlyphsForCharacters(emojiFont, chars, &fbGlyphs, chars.count) {
                            glyphs = fbGlyphs
                            drawFont = emojiFont
                        } else {
                            let ctFallback = CTFontCreateForString(selectedFont, ch as CFString,
                                                                   CFRange(location: 0, length: ch.utf16.count))
                            var ctGlyphs = [CGGlyph](repeating: 0, count: chars.count)
                            if CTFontGetGlyphsForCharacters(ctFallback, chars, &ctGlyphs, chars.count) {
                                glyphs = ctGlyphs
                                drawFont = ctFallback
                            } else {
                                drawFont = selectedFont
                            }
                        }
                    } else if isNerd {
                        // Nerd Font シンボル → システムフォールバック（CJK に流さない）
                        let ctFallback = CTFontCreateForString(selectedFont, ch as CFString,
                                                               CFRange(location: 0, length: ch.utf16.count))
                        var fbGlyphs = [CGGlyph](repeating: 0, count: chars.count)
                        if CTFontGetGlyphsForCharacters(ctFallback, chars, &fbGlyphs, chars.count) {
                            glyphs = fbGlyphs
                            drawFont = ctFallback
                        } else {
                            drawFont = selectedFont
                        }
                    } else {
                        // CJK / その他 → CJK フォールバック → システムフォールバック
                        let fallback = isBold ? cjkBoldFont! : cjkFont!
                        var fbGlyphs = [CGGlyph](repeating: 0, count: chars.count)
                        if CTFontGetGlyphsForCharacters(fallback, chars, &fbGlyphs, chars.count) {
                            glyphs = fbGlyphs
                            drawFont = fallback
                        } else {
                            let ctFallback = CTFontCreateForString(selectedFont, ch as CFString,
                                                                   CFRange(location: 0, length: ch.utf16.count))
                            var ctGlyphs = [CGGlyph](repeating: 0, count: chars.count)
                            if CTFontGetGlyphsForCharacters(ctFallback, chars, &ctGlyphs, chars.count) {
                                glyphs = ctGlyphs
                                drawFont = ctFallback
                            } else {
                                drawFont = selectedFont
                            }
                        }
                    }
                } else {
                    drawFont = selectedFont
                }

                var position = CGPoint(x: x, y: y + baselineOffset)
                CTFontDrawGlyphs(drawFont, glyphs, &position, glyphs.count, ctx)

                // アンダーライン
                if isUnderline {
                    ctx.setStrokeColor(fgColor.cgColor)
                    ctx.setLineWidth(1.0)
                    ctx.move(to: CGPoint(x: x, y: y + 1))
                    ctx.addLine(to: CGPoint(x: x + cellWidth, y: y + 1))
                    ctx.strokePath()
                }
            }
        }

        // 選択範囲ハイライト描画
        if let sel = normalizedSelection() {
            ctx.setFillColor(NSColor.selectedTextBackgroundColor.withAlphaComponent(0.35).cgColor)
            for row in sel.start.row...sel.end.row {
                let colStart = (row == sel.start.row) ? sel.start.col : 0
                let colEnd = (row == sel.end.row) ? sel.end.col : cols - 1
                let sx = round(CGFloat(colStart) * cellWidth)
                let ex = round(CGFloat(colEnd + 1) * cellWidth)
                let sy = round(bounds.height - CGFloat(row + 1) * cellHeight)
                let sh = round(bounds.height - CGFloat(row) * cellHeight) - sy
                ctx.fill(CGRect(x: sx, y: sy, width: ex - sx, height: sh))
            }
        }

        // カーソル描画（全角文字の上では幅 2 セル）
        let cursor = vp_bridge_get_cursor_session(sessionId)
        if cursor.visible {
            let cursorX = CGFloat(cursor.x) * cellWidth
            let cursorY = bounds.height - CGFloat(Int(cursor.y) + 1) * cellHeight

            // カーソル位置の文字が全角かチェック
            let idx = Int(cursor.y) * Int(gridCols) + Int(cursor.x)
            var cursorWidth = cellWidth
            if idx < cellBuffer.count {
                let ch = cellString(from: cellBuffer[idx])
                if let scalar = ch.unicodeScalars.first, isFullWidth(scalar) {
                    cursorWidth = cellWidth * 2
                }
            }

            ctx.setFillColor(NSColor.white.withAlphaComponent(0.5).cgColor)
            ctx.fill(CGRect(x: cursorX, y: cursorY, width: cursorWidth, height: cellHeight))
        }
    }

    // MARK: - ヘルパー

    /// VPCellData の ch フィールドから Swift String を生成
    private func cellString(from cell: VPCellData) -> String {
        let bytes = [cell.ch.0, cell.ch.1, cell.ch.2, cell.ch.3, cell.ch.4]
        // null 終端まで
        let len = bytes.firstIndex(of: 0) ?? 5
        guard len > 0 else { return "" }
        return String(bytes: bytes[0..<len], encoding: .utf8) ?? ""
    }

    /// RGBA u32 → NSColor 変換
    private func colorFromRGBA(_ rgba: UInt32) -> NSColor {
        let r = CGFloat((rgba >> 24) & 0xFF) / 255.0
        let g = CGFloat((rgba >> 16) & 0xFF) / 255.0
        let b = CGFloat((rgba >> 8) & 0xFF) / 255.0
        let a = CGFloat(rgba & 0xFF) / 255.0
        return NSColor(red: r, green: g, blue: b, alpha: a)
    }

    // MARK: - Nerd Font v3 シンボル範囲

    private static let nerdFontRanges: [ClosedRange<UInt32>] = [
        // Powerline シンボル
        0xE0A0...0xE0A3,   // Powerline
        0xE0B0...0xE0D4,   // Powerline Extra
        // Seti-UI + Custom
        0xE5FA...0xE6AC,
        // Devicons
        0xE700...0xE7C5,
        // Font Awesome
        0xF000...0xF2E0,
        // Font Awesome Extension
        0xE200...0xE2A9,
        // Octicons
        0xF400...0xF532,
        0x2665...0x2665,   // ♥
        0x26A1...0x26A1,   // ⚡
        // Material Design Icons
        0xF0001...0xF1AF0,
        // Weather Icons
        0xE300...0xE3E3,
        // Font Logos (formerly Font Linux)
        0xF300...0xF375,
        // Pomicons
        0xE000...0xE00A,
        // Codicons
        0xEA60...0xEBEB,
        // IEC Power Symbols
        0x23FB...0x23FE,
        0x2B58...0x2B58,
    ]

    private func isNerdFontSymbol(_ scalar: Unicode.Scalar) -> Bool {
        let v = scalar.value
        for range in Self.nerdFontRanges {
            if range.contains(v) { return true }
        }
        return false
    }

    private func isFullWidth(_ scalar: Unicode.Scalar) -> Bool {
        let v = scalar.value
        if (0x4E00...0x9FFF).contains(v) { return true }
        if (0x3400...0x4DBF).contains(v) { return true }
        if (0x20000...0x2A6DF).contains(v) { return true }
        if (0x3040...0x309F).contains(v) { return true }
        if (0x30A0...0x30FF).contains(v) { return true }
        if (0xFF01...0xFF60).contains(v) { return true }
        if (0xFFE0...0xFFE6).contains(v) { return true }
        if (0x3000...0x303F).contains(v) { return true }
        if (0x3200...0x32FF).contains(v) { return true }
        if (0x3300...0x33FF).contains(v) { return true }
        if (0xF900...0xFAFF).contains(v) { return true }
        if (0xAC00...0xD7AF).contains(v) { return true }
        return false
    }

    private func isEmoji(_ scalar: Unicode.Scalar) -> Bool {
        let v = scalar.value
        if (0x1F600...0x1F64F).contains(v) { return true }
        if (0x1F300...0x1F5FF).contains(v) { return true }
        if (0x1F680...0x1F6FF).contains(v) { return true }
        if (0x1F900...0x1F9FF).contains(v) { return true }
        if (0x1FA00...0x1FA6F).contains(v) { return true }
        if (0x1FA70...0x1FAFF).contains(v) { return true }
        if (0x2600...0x26FF).contains(v) { return true }
        if (0x2700...0x27BF).contains(v) { return true }
        if (0xFE00...0xFE0F).contains(v) { return true }
        if (0x200D...0x200D).contains(v) { return true }
        if (0x2300...0x23FF).contains(v) { return true }
        if (0x2B50...0x2B55).contains(v) { return true }
        if (0x231A...0x231B).contains(v) { return true }
        if (0x25AA...0x25FE).contains(v) { return true }
        return false
    }

    // MARK: - Box-drawing 文字のカスタム描画

    /// Box-drawing 文字（U+2500〜U+257F）を CGContext で直接描画する
    ///
    /// フォントグリフは lineHeightMultiplier でセルが膨らむと隙間ができるため、
    /// セル境界にピッタリ合うよう自前で線を引く。Alacritty / Ghostty と同じアプローチ。
    /// - Returns: 描画した場合 true（呼び出し元で continue する）
    @discardableResult
    private func drawBoxCharacter(_ scalar: Unicode.Scalar, in ctx: CGContext,
                                  cellRect: CGRect, color: CGColor, isBold: Bool) -> Bool {
        let v = scalar.value
        guard (0x2500...0x257F).contains(v) else { return false }

        let x = cellRect.minX
        let y = cellRect.minY
        let w = cellRect.width
        let h = cellRect.height
        let mx = x + w / 2  // 中心 X
        let my = y + h / 2  // 中心 Y
        let lw: CGFloat = isBold ? 2.0 : 1.0

        ctx.saveGState()
        ctx.setStrokeColor(color)
        ctx.setLineWidth(lw)
        ctx.setLineCap(.square)

        switch v {
        // ─ 水平線
        case 0x2500, 0x2501:
            ctx.move(to: CGPoint(x: x, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))

        // │ 垂直線
        case 0x2502, 0x2503:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: y + h))

        // ┌ 左上角
        case 0x250C, 0x250D, 0x250E, 0x250F:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))

        // ┐ 右上角
        case 0x2510, 0x2511, 0x2512, 0x2513:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x, y: my))

        // └ 左下角
        case 0x2514, 0x2515, 0x2516, 0x2517:
            ctx.move(to: CGPoint(x: mx, y: y + h))
            ctx.addLine(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))

        // ┘ 右下角
        case 0x2518, 0x2519, 0x251A, 0x251B:
            ctx.move(to: CGPoint(x: mx, y: y + h))
            ctx.addLine(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x, y: my))

        // ├ 左 T 字
        case 0x251C, 0x251D, 0x251E, 0x251F, 0x2520, 0x2521, 0x2522, 0x2523:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: y + h))
            ctx.move(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))

        // ┤ 右 T 字
        case 0x2524, 0x2525, 0x2526, 0x2527, 0x2528, 0x2529, 0x252A, 0x252B:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: y + h))
            ctx.move(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x, y: my))

        // ┬ 上 T 字
        case 0x252C, 0x252D, 0x252E, 0x252F, 0x2530, 0x2531, 0x2532, 0x2533:
            ctx.move(to: CGPoint(x: x, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))
            ctx.move(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: mx, y: y))

        // ┴ 下 T 字
        case 0x2534, 0x2535, 0x2536, 0x2537, 0x2538, 0x2539, 0x253A, 0x253B:
            ctx.move(to: CGPoint(x: x, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))
            ctx.move(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: mx, y: y + h))

        // ┼ 十字
        case 0x253C, 0x253D, 0x253E, 0x253F, 0x2540, 0x2541, 0x2542, 0x2543,
             0x2544, 0x2545, 0x2546, 0x2547, 0x2548, 0x2549, 0x254A, 0x254B:
            ctx.move(to: CGPoint(x: x, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: y + h))

        // ═ 二重水平線
        case 0x2550:
            let gap: CGFloat = 2.0
            ctx.move(to: CGPoint(x: x, y: my - gap))
            ctx.addLine(to: CGPoint(x: x + w, y: my - gap))
            ctx.move(to: CGPoint(x: x, y: my + gap))
            ctx.addLine(to: CGPoint(x: x + w, y: my + gap))

        // ║ 二重垂直線
        case 0x2551:
            let gap: CGFloat = 2.0
            ctx.move(to: CGPoint(x: mx - gap, y: y))
            ctx.addLine(to: CGPoint(x: mx - gap, y: y + h))
            ctx.move(to: CGPoint(x: mx + gap, y: y))
            ctx.addLine(to: CGPoint(x: mx + gap, y: y + h))

        // ╭ 丸角 左上
        case 0x256D:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addQuadCurve(to: CGPoint(x: x + w, y: my),
                             control: CGPoint(x: mx, y: my))

        // ╮ 丸角 右上
        case 0x256E:
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addQuadCurve(to: CGPoint(x: x, y: my),
                             control: CGPoint(x: mx, y: my))

        // ╰ 丸角 左下
        case 0x2570:
            ctx.move(to: CGPoint(x: mx, y: y + h))
            ctx.addQuadCurve(to: CGPoint(x: x + w, y: my),
                             control: CGPoint(x: mx, y: my))

        // ╯ 丸角 右下
        case 0x256F:
            ctx.move(to: CGPoint(x: mx, y: y + h))
            ctx.addQuadCurve(to: CGPoint(x: x, y: my),
                             control: CGPoint(x: mx, y: my))

        // ╴╵╶╷ 半分ライン
        case 0x2574: // ╴ 左半分水平
            ctx.move(to: CGPoint(x: x, y: my))
            ctx.addLine(to: CGPoint(x: mx, y: my))
        case 0x2575: // ╵ 上半分垂直
            ctx.move(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: mx, y: y + h))
        case 0x2576: // ╶ 右半分水平
            ctx.move(to: CGPoint(x: mx, y: my))
            ctx.addLine(to: CGPoint(x: x + w, y: my))
        case 0x2577: // ╷ 下半分垂直
            ctx.move(to: CGPoint(x: mx, y: y))
            ctx.addLine(to: CGPoint(x: mx, y: my))

        default:
            // 未対応の box-drawing 文字はフォントグリフにフォールバック
            ctx.restoreGState()
            return false
        }

        ctx.strokePath()
        ctx.restoreGState()
        return true
    }

    // MARK: - テストパターン

    func drawTestPattern() {
        guard bridgeInitialized else { return }
        vp_bridge_draw_test_pattern()
        needsDisplay = true
    }

    // MARK: - PTY

    /// PTY を起動
    func startPty(cwd: String? = nil) {
        guard bridgeInitialized else { return }

        let start = { (ptr: UnsafePointer<CChar>?) -> Int32 in
            vp_bridge_pty_start_session(self.sessionId, ptr, self.gridCols, self.gridRows)
        }

        let result: Int32
        if let cwdPath = cwd {
            result = cwdPath.withCString { start($0) }
        } else {
            result = start(nil)
        }

        if result == 0 {
            needsDisplay = true
        }
    }

    /// PTY を停止
    func stopPty() {
        guard bridgeInitialized else { return }
        vp_bridge_pty_stop_session(sessionId)
    }

    // MARK: - クリップボード

    /// クリップボードからテキスト/画像を PTY にペースト
    private func pasteFromClipboard() {
        let pb = NSPasteboard.general

        // テキストペースト（Bracketed Paste Mode）
        if let text = pb.string(forType: .string), !text.isEmpty {
            let bracketStart: [UInt8] = [0x1B, 0x5B, 0x32, 0x30, 0x30, 0x7E] // \e[200~
            let bracketEnd: [UInt8] = [0x1B, 0x5B, 0x32, 0x30, 0x31, 0x7E]   // \e[201~

            bracketStart.withUnsafeBufferPointer { ptr in
                _ = vp_bridge_pty_write_session(sessionId, ptr.baseAddress!, UInt32(ptr.count))
            }

            if let data = text.data(using: .utf8) {
                data.withUnsafeBytes { ptr in
                    _ = vp_bridge_pty_write_session(
                        sessionId,
                        ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                        UInt32(ptr.count)
                    )
                }
            }

            bracketEnd.withUnsafeBufferPointer { ptr in
                _ = vp_bridge_pty_write_session(sessionId, ptr.baseAddress!, UInt32(ptr.count))
            }
            return
        }

        // 画像ペースト: Ctrl+V (0x16) を送信して Claude Code に処理を委譲
        if pb.types?.contains(.png) == true || pb.types?.contains(.tiff) == true {
            let ctrlV: [UInt8] = [0x16]
            ctrlV.withUnsafeBufferPointer { ptr in
                _ = vp_bridge_pty_write_session(sessionId, ptr.baseAddress!, UInt32(ptr.count))
            }
        }
    }

    // MARK: - キーボード入力

    /// IME 変換中テキスト
    private var markedString: NSMutableAttributedString = NSMutableAttributedString()
    private var _markedRange: NSRange = NSRange(location: NSNotFound, length: 0)
    private var selectedRangeValue: NSRange = NSRange(location: 0, length: 0)

    /// Cmd ショートカットを keyDown より先に捕捉
    ///
    /// macOS のイベント処理順: performKeyEquivalent → menu shortcuts → keyDown
    /// NSViewRepresentable 内の NSView はメニューショートカットが効きにくいため、
    /// ここで Cmd+V / Cmd+C を直接処理する。
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard event.modifierFlags.contains(.command),
              let ch = event.charactersIgnoringModifiers else {
            return super.performKeyEquivalent(with: event)
        }

        switch ch {
        case "v":
            // Cmd+V: ペースト
            if vp_bridge_pty_is_running_session(sessionId) {
                pasteFromClipboard()
            }
            return true
        case "c":
            // Cmd+C: コピー（選択テキストがあれば）
            return true
        default:
            return super.performKeyEquivalent(with: event)
        }
    }

    override func keyDown(with event: NSEvent) {
        guard vp_bridge_pty_is_running_session(sessionId) else {
            super.keyDown(with: event)
            return
        }

        // Cmd ショートカットは performKeyEquivalent で処理済み
        // ここに到達する Cmd イベントは未処理のもの（Cmd+`, Cmd+Q 等）
        if event.modifierFlags.contains(.command) {
            super.keyDown(with: event)
            return
        }

        // IME 変換中はすべて IME に委譲
        if hasMarkedText() {
            _ = inputContext?.handleEvent(event)
            return
        }

        // 特殊キーをエスケープシーケンスに変換して PTY に送信
        if let bytes = keyEventToBytes(event) {
            bytes.withUnsafeBufferPointer { ptr in
                _ = vp_bridge_pty_write_session(sessionId, ptr.baseAddress!, UInt32(ptr.count))
            }
            return
        }

        // IME を経由して処理（日本語入力対応）
        if inputContext?.handleEvent(event) == true {
            return
        }

        // IME が処理しなかった文字を直接送信
        if let chars = event.characters, let data = chars.data(using: .utf8) {
            data.withUnsafeBytes { ptr in
                _ = vp_bridge_pty_write_session(
                    sessionId,
                    ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                    UInt32(ptr.count)
                )
            }
        }
    }

    /// キーイベントをターミナルバイト列に変換
    private func keyEventToBytes(_ event: NSEvent) -> [UInt8]? {
        let modifiers = event.modifierFlags
        let keyCode = event.keyCode

        // Ctrl + 文字キー → 制御コード
        if modifiers.contains(.control), let chars = event.charactersIgnoringModifiers {
            if let ch = chars.first, ch.isASCII {
                let ascii = ch.asciiValue!
                if ascii >= 0x61 && ascii <= 0x7A { return [ascii - 0x60] }
                if ascii >= 0x41 && ascii <= 0x5A { return [ascii - 0x40] }
                switch ascii {
                case 0x5B: return [0x1B]
                case 0x5C: return [0x1C]
                case 0x5D: return [0x1D]
                case 0x5E: return [0x1E]
                case 0x5F: return [0x1F]
                default: break
                }
            }
        }

        // 特殊キー → VT100 エスケープシーケンス
        switch keyCode {
        case 36:  return [0x0D]                    // Return → CR
        case 48:  return [0x09]                    // Tab
        case 51:  return [0x7F]                    // Delete (Backspace)
        case 53:  return [0x1B]                    // Escape
        case 117: return [0x1B, 0x5B, 0x33, 0x7E] // Forward Delete
        case 123: return [0x1B, 0x5B, 0x44]        // ←
        case 124: return [0x1B, 0x5B, 0x43]        // →
        case 125: return [0x1B, 0x5B, 0x42]        // ↓
        case 126: return [0x1B, 0x5B, 0x41]        // ↑
        case 115: return [0x1B, 0x5B, 0x48]        // Home
        case 119: return [0x1B, 0x5B, 0x46]        // End
        case 116: // Page Up — スクロールバックを上に移動
            vp_bridge_scroll_session(sessionId, Int32.max)
            return []
        case 121: // Page Down — スクロールバックを下に移動
            vp_bridge_scroll_session(sessionId, Int32.min)
            return []
        default:  break
        }

        // F キー
        let fKeys: [UInt16: [UInt8]] = [
            122: [0x1B, 0x4F, 0x50],               // F1
            120: [0x1B, 0x4F, 0x51],               // F2
            99:  [0x1B, 0x4F, 0x52],               // F3
            118: [0x1B, 0x4F, 0x53],               // F4
            96:  [0x1B, 0x5B, 0x31, 0x35, 0x7E],   // F5
            97:  [0x1B, 0x5B, 0x31, 0x37, 0x7E],   // F6
            98:  [0x1B, 0x5B, 0x31, 0x38, 0x7E],   // F7
            100: [0x1B, 0x5B, 0x31, 0x39, 0x7E],   // F8
        ]
        if let fKeyBytes = fKeys[keyCode] {
            return fKeyBytes
        }

        return nil
    }

    // MARK: - フラグ変更（Modifier キー）

    override func flagsChanged(with event: NSEvent) {
        // Modifier キー単独では PTY に何も送らない
    }

    // MARK: - テキスト選択

    private var selectionStart: (col: Int, row: Int)?
    private var selectionEnd: (col: Int, row: Int)?
    private var isDragging = false

    private func gridPosition(from point: NSPoint) -> (col: Int, row: Int) {
        let local = convert(point, from: nil)
        let col = max(0, min(Int(gridCols) - 1, Int(local.x / cellWidth)))
        let row = max(0, min(Int(gridRows) - 1, Int((bounds.height - local.y) / cellHeight)))
        return (col, row)
    }

    private func normalizedSelection() -> (start: (col: Int, row: Int), end: (col: Int, row: Int))? {
        guard let s = selectionStart, let e = selectionEnd else { return nil }
        if s.row < e.row || (s.row == e.row && s.col <= e.col) {
            return (s, e)
        }
        return (e, s)
    }

    override func mouseDown(with event: NSEvent) {
        // クリックで first responder を取得（SwiftUI サイドバーからフォーカスを奪う）
        window?.makeFirstResponder(self)

        let pos = gridPosition(from: event.locationInWindow)
        selectionStart = pos
        selectionEnd = pos
        isDragging = true
        needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) {
        guard isDragging else { return }
        selectionEnd = gridPosition(from: event.locationInWindow)
        needsDisplay = true
    }

    override func mouseUp(with event: NSEvent) {
        guard isDragging else { return }
        isDragging = false
        selectionEnd = gridPosition(from: event.locationInWindow)

        if selectionStart?.col == selectionEnd?.col && selectionStart?.row == selectionEnd?.row {
            selectionStart = nil
            selectionEnd = nil
            needsDisplay = true
            return
        }

        copySelectionToClipboard()
        needsDisplay = true
    }

    private func copySelectionToClipboard() {
        guard let sel = normalizedSelection() else { return }
        var text = ""
        let cols = Int(gridCols)

        for row in sel.start.row...sel.end.row {
            let colStart = (row == sel.start.row) ? sel.start.col : 0
            let colEnd = (row == sel.end.row) ? sel.end.col : cols - 1

            var line = ""
            for col in colStart...colEnd {
                let idx = row * cols + col
                guard idx < cellBuffer.count else { continue }
                let ch = cellString(from: cellBuffer[idx])
                line += ch.isEmpty ? " " : ch
            }
            text += line.replacingOccurrences(of: "\\s+$", with: "", options: .regularExpression)
            if row < sel.end.row {
                text += "\n"
            }
        }

        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(text, forType: .string)
    }

    // MARK: - マウススクロール

    override func scrollWheel(with event: NSEvent) {
        guard vp_bridge_pty_is_running_session(sessionId) else { return }

        let lines = Int(round(event.scrollingDeltaY / 3.0))
        guard lines != 0 else { return }

        let upArrow: [UInt8] = [0x1B, 0x5B, 0x41]
        let downArrow: [UInt8] = [0x1B, 0x5B, 0x42]

        let sequence = lines > 0 ? upArrow : downArrow
        let count = abs(lines)

        for _ in 0..<min(count, 10) {
            sequence.withUnsafeBufferPointer { ptr in
                _ = vp_bridge_pty_write_session(sessionId, ptr.baseAddress!, UInt32(ptr.count))
            }
        }
    }

    // MARK: - Input Context

    override var inputContext: NSTextInputContext? {
        if _inputContext == nil {
            _inputContext = NSTextInputContext(client: self)
        }
        return _inputContext
    }
    private var _inputContext: NSTextInputContext?
}

// MARK: - NSTextInputClient（IME 日本語入力対応）

extension TerminalView: @preconcurrency NSTextInputClient {

    @MainActor
    func insertText(_ string: Any, replacementRange: NSRange) {
        let text: String
        if let attrStr = string as? NSAttributedString {
            text = attrStr.string
        } else if let str = string as? String {
            text = str
        } else {
            return
        }

        markedString = NSMutableAttributedString()
        _markedRange = NSRange(location: NSNotFound, length: 0)

        guard vp_bridge_pty_is_running_session(sessionId) else { return }
        if let data = text.data(using: .utf8) {
            data.withUnsafeBytes { ptr in
                _ = vp_bridge_pty_write_session(
                    sessionId,
                    ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                    UInt32(ptr.count)
                )
            }
        }
        needsDisplay = true
    }

    @MainActor
    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        if let attrStr = string as? NSAttributedString {
            markedString = NSMutableAttributedString(attributedString: attrStr)
        } else if let str = string as? String {
            markedString = NSMutableAttributedString(string: str)
        }
        _markedRange = NSRange(location: 0, length: markedString.length)
        selectedRangeValue = selectedRange
        needsDisplay = true
    }

    @MainActor
    func unmarkText() {
        markedString = NSMutableAttributedString()
        _markedRange = NSRange(location: NSNotFound, length: 0)
        needsDisplay = true
    }

    @MainActor
    func selectedRange() -> NSRange {
        selectedRangeValue
    }

    @MainActor
    func markedRange() -> NSRange {
        _markedRange
    }

    @MainActor
    func hasMarkedText() -> Bool {
        markedString.length > 0
    }

    @MainActor
    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        nil
    }

    @MainActor
    func validAttributedString() -> NSAttributedString {
        NSAttributedString()
    }

    @MainActor
    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        let cursor = vp_bridge_get_cursor_session(sessionId)
        let cursorCol = CGFloat(cursor.x) + CGFloat(markedString.length)
        let localX = cursorCol * cellWidth
        let localY = bounds.height - CGFloat(Int(cursor.y) + 1) * cellHeight

        let localRect = NSRect(x: localX, y: localY, width: cellWidth, height: cellHeight)

        guard let window = self.window else {
            return NSRect(x: 0, y: 0, width: 0, height: 0)
        }
        let windowRect = convert(localRect, to: nil)
        let screenRect = window.convertToScreen(windowRect)
        return screenRect
    }

    @MainActor
    func characterIndex(for point: NSPoint) -> Int {
        0
    }

    @MainActor
    func attributedString() -> NSAttributedString {
        NSAttributedString()
    }

    @MainActor
    func fractionOfDistanceThroughGlyph(for point: NSPoint) -> CGFloat {
        0
    }

    @MainActor
    func baselineDeltaForCharacter(at index: Int) -> CGFloat {
        0
    }

    @MainActor
    func windowLevel() -> Int {
        guard let window = self.window else { return 0 }
        return Int(window.level.rawValue)
    }

    @MainActor
    func drawsVerticallyForCharacter(at index: Int) -> Bool {
        false
    }

    @MainActor
    func validAttributesForMarkedText() -> [NSAttributedString.Key] {
        [.font, .foregroundColor, .underlineStyle]
    }
}
