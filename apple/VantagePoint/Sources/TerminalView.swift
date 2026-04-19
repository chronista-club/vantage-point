import AppKit
import CoreText
import OSLog
import VPBridge

private let logger = Logger(subsystem: "tech.anycreative.vp", category: "Terminal")

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
/// NOTE: セッション固有コールバックが Rust 側で未対応のため全ビューをインバリデートする。
///       非アクティブビューは draw() 内の isActive ガードで早期リターンするため描画コストは最小。
private let sharedFrameCallback: VPFrameReadyCallback = {
    DispatchQueue.main.async {
        for (_, view) in sessionRegistry where view.isActive {
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

    /// コンソールフォント名（FiraCode Nerd Font Mono 固定）
    var fontName: String = "FiraCode Nerd Font Mono" {
        didSet { updateFontMetrics() }
    }

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
    var bridgeInitialized: Bool { sessionId != 0 }

    /// 遅延 PTY 起動用: レイアウト確定後に自動起動するコマンド
    var deferredPtyCommand: String?
    /// 遅延 PTY 起動用: 作業ディレクトリ
    var deferredPtyCwd: String?

    /// アクティブ（表示中）フラグ — false のとき描画をスキップ
    var isActive: Bool = true

    /// このターミナルがフォーカスされているか（first responder 制御用）
    var isFocused: Bool = true

    /// Split Navigator がアクティブ — true のとき矢印/数字/Enter/Esc を PTY に送らずナビゲーターに転送
    var splitNavigatorActive: Bool = false

    /// VP Pane ID（ペインフォーカス通知用）
    var paneId: UUID?

    /// マウスイベントを PTY に送信するか（tmux 内は true、素シェルは false）
    var sendMouseEvents: Bool = true

    /// PTY 起動時のコマンド（再起動用に保持）
    var lastPtyCommand: String?
    /// PTY 起動時の CWD（再起動用に保持）
    var lastPtyCwd: String?
    /// PTY 自動復旧: 終了検知 → 再起動（クールダウン付き）
    private var ptyRestartCount: Int = 0
    private var lastPtyExit: Date?
    private static let maxRestartCount = 3
    private static let restartCooldown: TimeInterval = 5.0

    // MARK: - 初期化

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        commonInit()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        commonInit()
    }

    /// PageUp/PageDown モニター（アプリレベルでインターセプト）
    /// 書き込みは必ず MainActor 上 (commonInit) で行うこと。
    /// nonisolated(unsafe) は deinit からのアクセスのために必要。
    nonisolated(unsafe) private var keyMonitor: Any?

    /// mouseMoved イベント受信用のトラッキングエリア
    private var mouseTrackingArea: NSTrackingArea?
    /// mouseMoved の URL 検出キャッシュ（行が変わった時のみ再計算）
    private var lastHoveredRow: Int = -1
    private var lastHoveredCol: Int = -1
    private var lastHoveredUrl: URL?

    private func commonInit() {
        wantsLayer = true
        layer?.backgroundColor = defaultBackground.cgColor

        updateFontMetrics()
        setupTrackingArea()

        // macOS は PageUp/PageDown を scrollPageUp:/scrollPageDown: NSResponder アクションに変換し、
        // NSViewRepresentable 内の NSView の keyDown には到達させない。
        // アプリレベルでキーイベントをモニターし、PageUp/PageDown を SGR マウスイベントとして PTY に送信する。
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self,
                  event.keyCode == 116 || event.keyCode == 121,
                  self.sessionId != 0,
                  self.window?.firstResponder === self,
                  vp_bridge_pty_is_running_session(self.sessionId) else {
                return event
            }
            // CC は VT100 の \e[5~/\e[6~ に反応しないため、
            // マウスホイールと同じ SGR マウスイベントで送信する（scrollWheel と同じプロトコル）
            let button = event.keyCode == 116 ? 64 : 65  // 64=scroll up, 65=scroll down
            let col = Int(self.gridCols) / 2 + 1  // 画面中央（1-based）
            let row = Int(self.gridRows) / 2 + 1
            let scrollLines = max(Int(self.gridRows) - 3, 1) // ページ分（3行オーバーラップ）
            let singleSeq = "\u{1B}[<\(button);\(col);\(row)M"
            let fullSeq = String(repeating: singleSeq, count: scrollLines)
            if let data = fullSeq.data(using: .ascii) {
                data.withUnsafeBytes { ptr in
                    _ = vp_bridge_pty_write_session(
                        self.sessionId,
                        ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                        UInt32(ptr.count)
                    )
                }
            }
            return nil
        }
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

    /// first responder 変更時にボーダーで視覚化（フォーカスインジケーター）
    override func becomeFirstResponder() -> Bool {
        let result = super.becomeFirstResponder()
        // VP Pane フォーカス通知: クリック等で first responder になったペインを通知
        // isFocused が既に true の場合は updateNSView 経由の自動フォーカスなのでスキップ
        // （通知 → focusedPaneId 更新 → updateNSView のループを防止）
        if let id = paneId, !isFocused {
            NotificationCenter.default.post(
                name: .vpPaneFocused,
                object: nil,
                userInfo: ["paneId": id]
            )
        }
        updateFocusBorder()
        return result
    }

    override func resignFirstResponder() -> Bool {
        let result = super.resignFirstResponder()
        updateFocusBorder()
        return result
    }

    private func updateFocusBorder() {
        if window?.firstResponder === self {
            layer?.borderColor = NSColor.controlAccentColor.withAlphaComponent(0.3).cgColor
            layer?.borderWidth = 1
        } else {
            layer?.borderColor = nil
            layer?.borderWidth = 0
        }
    }


    deinit {
        // deinit は nonisolated — MainActor 外から呼ばれる可能性がある
        // モニター解除もレジストリ解除も安全にディスパッチする
        let monitor = keyMonitor
        let sid = sessionId
        DispatchQueue.main.async {
            if let m = monitor {
                NSEvent.removeMonitor(m)
            }
            if sid != 0 {
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
            logger.debug("[VP] vp_bridge_create failed")
            return
        }

        sessionId = id
        sessionRegistry[id] = self
        logger.debug("Bridge session created: \(id) (\(self.gridCols)x\(self.gridRows))")

        // バッファを確保
        let totalCells = Int(gridCols) * Int(gridRows)
        cellBuffer = [VPCellData](repeating: VPCellData(), count: totalCells)
    }

    // MARK: - フォント

    private func updateFontMetrics() {
        // Fira Code Nerd Font Mono 固定。未インストール時のみシステム等幅にフォールバック
        let nsFont = NSFont(name: fontName, size: fontSize)
            ?? NSFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)

        // CJK フォント cascade list を明示設定（Hiragino Sans → Apple Color Emoji 等）。
        // これにより毎セル CTFontCreateForString を呼ばずに済み、CJK 多用時のスクロール性能が向上。
        // 豆腐表示の予防にもなる。
        let cascadeNames = ["Hiragino Sans", "Hiragino Kaku Gothic ProN", "Apple Color Emoji"]
        let cascadeDescriptors: [CTFontDescriptor] = cascadeNames.map { name in
            CTFontDescriptorCreateWithNameAndSize(name as CFString, 0)
        }
        let attributes: [CFString: Any] = [
            kCTFontCascadeListAttribute: cascadeDescriptors as CFArray,
        ]
        let descriptor = CTFontDescriptorCreateCopyWithAttributes(
            nsFont.fontDescriptor as CTFontDescriptor,
            attributes as CFDictionary
        )
        font = CTFontCreateWithFontDescriptor(descriptor, fontSize, nil)

        logger.debug("Font resolved: \(nsFont.fontName) (size: \(self.fontSize)) with CJK cascade")

        // Bold / Italic バリアント
        let boldTraits: CTFontSymbolicTraits = .boldTrait
        let italicTraits: CTFontSymbolicTraits = .italicTrait
        let boldItalicTraits: CTFontSymbolicTraits = [.boldTrait, .italicTrait]

        boldFont = CTFontCreateCopyWithSymbolicTraits(font, fontSize, nil, boldTraits, boldTraits) ?? font
        italicFont = CTFontCreateCopyWithSymbolicTraits(font, fontSize, nil, italicTraits, italicTraits) ?? font
        boldItalicFont = CTFontCreateCopyWithSymbolicTraits(font, fontSize, nil, boldItalicTraits, boldItalicTraits) ?? font

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

        logger.debug("Cell metrics: w=\(self.cellWidth) h=\(self.cellHeight) baseline=\(self.baselineOffset)")

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

        // 遅延 PTY 起動: レイアウトが確定して有効なグリッドサイズが得られたら起動
        // 1 回だけ試行し、成否に関わらず deferred を消費（無限リトライ防止）
        if let cwd = deferredPtyCwd, gridCols > 1 && gridRows > 1 {
            let cmd = deferredPtyCommand
            deferredPtyCommand = nil
            deferredPtyCwd = nil
            logger.debug("Deferred PTY start: \(self.gridCols)x\(self.gridRows)")
            startPty(cwd: cwd, command: cmd)
        }
    }

    // MARK: - 描画

    override func draw(_ dirtyRect: NSRect) {
        guard let ctx = NSGraphicsContext.current?.cgContext else { return }
        guard bridgeInitialized, isActive else {
            // Bridge 未初期化 or 非アクティブ時はデフォルト背景のみ
            ctx.setFillColor(defaultBackground.cgColor)
            ctx.fill(bounds)
            return
        }

        // バッファ一括読み取り
        let totalCells = Int(gridCols) * Int(gridRows)
        guard totalCells > 0 else { return }

        // cellBuffer のサイズが不整合ならリサイズ（setFrameSize と draw の競合対策）
        if cellBuffer.count != totalCells {
            cellBuffer = [VPCellData](repeating: VPCellData(), count: totalCells)
        }

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

                // ワイド文字判定: VT パーサーの WIDE_CHAR フラグ（bit 6）を使用
                // Unicode テーブルによる推測ではなく、VT パーサーが正確に判定した結果
                let charIsFullWidth = (cell.flags & (1 << 6)) != 0

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

                // グリフが見つからない場合はフォールバックフォントで描画
                if found && !glyphs.contains(0) {
                    // プライマリフォントにグリフあり → 高速パス
                    var position = CGPoint(x: x, y: y + baselineOffset)
                    CTFontDrawGlyphs(selectedFont, glyphs, &position, glyphs.count, ctx)
                } else {
                    // システムフォールバック（CJK・絵文字・記号すべて対応）
                    let fallbackFont = CTFontCreateForString(selectedFont, ch as CFString,
                                                              CFRange(location: 0, length: ch.utf16.count))
                    var fbGlyphs = [CGGlyph](repeating: 0, count: chars.count)
                    if CTFontGetGlyphsForCharacters(fallbackFont, chars, &fbGlyphs, chars.count) {
                        // フォールバックフォントにグリフあり → 直接描画（高速）
                        var position = CGPoint(x: x, y: y + baselineOffset)
                        CTFontDrawGlyphs(fallbackFont, fbGlyphs, &position, fbGlyphs.count, ctx)
                    } else {
                        // 最終手段: CTLine（NSAttributedString 経由）
                        let attrs: [CFString: Any] = [
                            kCTFontAttributeName: fallbackFont,
                            kCTForegroundColorAttributeName: fgColor.cgColor,
                        ]
                        let attrStr = CFAttributedStringCreate(nil, ch as CFString, attrs as CFDictionary)!
                        let line = CTLineCreateWithAttributedString(attrStr)
                        ctx.textPosition = CGPoint(x: x, y: y + baselineOffset)
                        CTLineDraw(line, ctx)
                    }
                }

                // アンダーライン（全角文字は 2 セル幅で描画）
                if isUnderline {
                    let underlineWidth = charIsFullWidth ? cellWidth * 2 : cellWidth
                    ctx.setStrokeColor(fgColor.cgColor)
                    ctx.setLineWidth(1.0)
                    ctx.move(to: CGPoint(x: x, y: y + 1))
                    ctx.addLine(to: CGPoint(x: x + underlineWidth, y: y + 1))
                    ctx.strokePath()
                }
            }
        }

        // 選択範囲ハイライト描画
        if let sel = normalizedSelection() {
            ctx.setFillColor(NSColor.selectedTextBackgroundColor.withAlphaComponent(0.35).cgColor)
            for row in sel.start.row...sel.end.row {
                let colStart = (row == sel.start.row) ? sel.start.col : 0
                var colEnd = (row == sel.end.row) ? sel.end.col : cols - 1
                // colEnd が wide 文字なら spacer セルまでハイライトを延長
                let endIdx = row * cols + colEnd
                if endIdx < cellBuffer.count && (cellBuffer[endIdx].flags & (1 << 6)) != 0 {
                    colEnd = min(colEnd + 1, cols - 1)
                }
                let sx = round(CGFloat(colStart) * cellWidth)
                let ex = round(CGFloat(colEnd + 1) * cellWidth)
                let sy = round(bounds.height - CGFloat(row + 1) * cellHeight)
                let sh = round(bounds.height - CGFloat(row) * cellHeight) - sy
                ctx.fill(CGRect(x: sx, y: sy, width: ex - sx, height: sh))
            }
        }

        // IME 変換中文字の inline 描画（カーソルより前に描画）
        // hasMarkedText() == true のとき、カーソル位置から右に変換中テキストを表示。
        // iTerm2/Terminal.app と同等の UX。
        let cursor = vp_bridge_get_cursor_session(sessionId)
        let markedWidth = drawMarkedTextIfNeeded(ctx: ctx, cursor: cursor)

        // カーソル描画（全角文字の上では幅 2 セル、IME 変換中はマーク末尾に移動）
        if cursor.visible {
            let cursorX = (CGFloat(cursor.x) + CGFloat(markedWidth)) * cellWidth
            let cursorY = bounds.height - CGFloat(Int(cursor.y) + 1) * cellHeight

            // カーソル位置の文字がワイドかチェック（bit 6: WIDE_CHAR）
            // ただし IME 変換中は末尾位置 = 確定後の挿入点なので 1 セル幅で十分
            var cursorWidth = cellWidth
            if markedWidth == 0 {
                let idx = Int(cursor.y) * Int(gridCols) + Int(cursor.x)
                if idx < cellBuffer.count && (cellBuffer[idx].flags & (1 << 6)) != 0 {
                    cursorWidth = cellWidth * 2
                }
            }

            ctx.setFillColor(NSColor.white.withAlphaComponent(0.5).cgColor)
            ctx.fill(CGRect(x: cursorX, y: cursorY, width: cursorWidth, height: cellHeight))
        }
    }

    /// IME 変換中文字（markedString）を半透明背景 + 下線でカーソル位置から描画
    /// selectedRange（編集中の節）は別色でハイライト + 太い実線下線
    /// その他の節は薄い背景 + 点線下線
    /// - Returns: 描画した文字の合計セル幅（カーソル位置補正用）
    @MainActor
    private func drawMarkedTextIfNeeded(ctx: CGContext, cursor: VPCursorInfo) -> Int {
        guard hasMarkedText() else { return 0 }
        let text = markedString.string
        guard !text.isEmpty else { return 0 }

        let cursorY = bounds.height - CGFloat(Int(cursor.y) + 1) * cellHeight
        let totalWidth = TerminalView.displayWidth(of: text)
        let baseX = CGFloat(cursor.x) * cellWidth
        let totalPixelWidth = CGFloat(totalWidth) * cellWidth

        // selectedRange（編集中の節）の表示幅を計算
        // selectedRangeValue は markedString 内の UTF-16 オフセットなので grapheme で再計算
        let (selStartCol, selWidthCols) = selectedRangeWidthInCells(text: text, range: selectedRangeValue)
        let selStartX = baseX + CGFloat(selStartCol) * cellWidth
        let selPixelWidth = CGFloat(selWidthCols) * cellWidth

        ctx.saveGState()

        // 全体背景（薄いアクセント）— rounded で「浮いている」感を出す
        let bgRect = CGRect(x: baseX, y: cursorY, width: totalPixelWidth, height: cellHeight)
        let bgPath = CGPath(roundedRect: bgRect, cornerWidth: 3, cornerHeight: 3, transform: nil)
        ctx.addPath(bgPath)
        ctx.setFillColor(NSColor.controlAccentColor.withAlphaComponent(0.12).cgColor)
        ctx.fillPath()

        // selectedRange（編集中の節）— 濃い背景でアクティブ強調
        if selWidthCols > 0 {
            let selRect = CGRect(x: selStartX, y: cursorY, width: selPixelWidth, height: cellHeight)
            let selPath = CGPath(roundedRect: selRect, cornerWidth: 3, cornerHeight: 3, transform: nil)
            ctx.addPath(selPath)
            ctx.setFillColor(NSColor.controlAccentColor.withAlphaComponent(0.32).cgColor)
            ctx.fillPath()
        }

        // 文字描画（grapheme 単位、各グリフを cascade font で）
        var col = CGFloat(cursor.x)
        let fgColor = NSColor.textColor
        for char in text {
            let charStr = String(char)
            let charWidth = char.unicodeScalars.first.map { TerminalView.eastAsianWidth($0) } ?? 1

            let attrs: [CFString: Any] = [
                kCTFontAttributeName: font as Any,
                kCTForegroundColorAttributeName: fgColor.cgColor,
            ]
            let attrStr = CFAttributedStringCreate(nil, charStr as CFString, attrs as CFDictionary)!
            let line = CTLineCreateWithAttributedString(attrStr)
            ctx.textPosition = CGPoint(x: col * cellWidth, y: cursorY + baselineOffset)
            CTLineDraw(line, ctx)

            col += CGFloat(charWidth)
        }

        // 下線階層化:
        //  - 全体: 点線（変換途中）
        //  - selectedRange: 実線・太め（編集中の節）
        let underlineY = cursorY + 1
        let accentColor = NSColor.controlAccentColor.withAlphaComponent(0.7).cgColor
        ctx.setStrokeColor(accentColor)

        ctx.setLineWidth(1.0)
        ctx.setLineDash(phase: 0, lengths: [2.0, 2.0])
        ctx.move(to: CGPoint(x: baseX, y: underlineY))
        ctx.addLine(to: CGPoint(x: baseX + totalPixelWidth, y: underlineY))
        ctx.strokePath()

        if selWidthCols > 0 {
            ctx.setLineDash(phase: 0, lengths: [])
            ctx.setLineWidth(2.0)
            ctx.move(to: CGPoint(x: selStartX, y: underlineY))
            ctx.addLine(to: CGPoint(x: selStartX + selPixelWidth, y: underlineY))
            ctx.strokePath()
        }

        ctx.restoreGState()
        return totalWidth
    }

    /// markedString 内の UTF-16 範囲を「先頭からのセル幅オフセット + セル幅」に変換
    /// IME の selectedRange を grapheme クラスタ単位で位置算出するため
    @MainActor
    private func selectedRangeWidthInCells(text: String, range: NSRange) -> (startCol: Int, widthCols: Int) {
        guard range.location != NSNotFound, range.location <= (text as NSString).length else {
            return (0, 0)
        }
        let nsText = text as NSString
        let safeLength = min(range.length, nsText.length - range.location)
        let beforeRange = nsText.substring(to: range.location)
        let selRange = nsText.substring(with: NSRange(location: range.location, length: safeLength))
        return (TerminalView.displayWidth(of: beforeRange), TerminalView.displayWidth(of: selRange))
    }

    /// East Asian Width（簡易版）— 主要 CJK ブロックと絵文字を 2 セル扱い
    /// 厳密な Unicode UAX#11 ではなく、IME 描画位置算出用の実用範囲
    static func eastAsianWidth(_ scalar: Unicode.Scalar) -> Int {
        let v = scalar.value
        switch v {
        case 0x1100...0x115F,    // Hangul Jamo
             0x2E80...0x303E,    // CJK Radicals, Kangxi, CJK Symbols
             0x3041...0x33FF,    // Hiragana, Katakana, CJK Strokes, Bopomofo
             0x3400...0x4DBF,    // CJK Extension A
             0x4E00...0x9FFF,    // CJK Unified Ideographs
             0xA000...0xA4CF,    // Yi Syllables
             0xAC00...0xD7A3,    // Hangul Syllables
             0xF900...0xFAFF,    // CJK Compatibility Ideographs
             0xFE30...0xFE4F,    // CJK Compatibility Forms
             0xFF00...0xFF60,    // Fullwidth Forms
             0xFFE0...0xFFE6,    // Fullwidth Symbols
             0x20000...0x2FFFD,  // CJK Extension B-F
             0x30000...0x3FFFD,  // CJK Extension G
             0x1F300...0x1F64F,  // Emoji Misc Symbols
             0x1F680...0x1F6FF,  // Transport and Map
             0x1F900...0x1F9FF:  // Supplemental Symbols and Pictographs
            return 2
        default:
            return 1
        }
    }

    /// 文字列の表示セル幅を grapheme cluster 単位で合計
    static func displayWidth(of string: String) -> Int {
        var width = 0
        for char in string {
            if let first = char.unicodeScalars.first {
                width += eastAsianWidth(first)
            } else {
                width += 1
            }
        }
        return width
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

    // MARK: - スクリーンショット

    /// ターミナルのレンダリング結果を PNG ファイルに保存
    /// - Parameter path: 保存先パス（nil なら ~/Desktop/vp-screenshot-{timestamp}.png）
    /// - Returns: 保存したファイルパス（失敗時は nil）
    @discardableResult
    func captureScreenshot(to path: String? = nil) -> String? {
        let targetPath: String
        if let path {
            targetPath = path
        } else {
            let ts = Int(Date().timeIntervalSince1970)
            targetPath = NSHomeDirectory() + "/Desktop/vp-screenshot-\(ts).png"
        }

        // ビューのレンダリングをビットマップにキャプチャ
        guard let rep = bitmapImageRepForCachingDisplay(in: bounds) else {
            logger.debug("[VP] Screenshot failed: could not create bitmap rep")
            return nil
        }
        cacheDisplay(in: bounds, to: rep)

        guard let pngData = rep.representation(using: .png, properties: [:]) else {
            logger.debug("[VP] Screenshot failed: PNG encoding failed")
            return nil
        }

        do {
            try pngData.write(to: URL(fileURLWithPath: targetPath))
            logger.debug("Screenshot saved: \(targetPath)")
            return targetPath
        } catch {
            logger.error("Screenshot failed: \(error.localizedDescription)")
            return nil
        }
    }

    // MARK: - PTY

    /// PTY を起動
    /// - Parameters:
    ///   - cwd: 作業ディレクトリ（nil ならデフォルト）
    ///   - command: 実行コマンド（nil ならデフォルトシェル）
    func startPty(cwd: String? = nil, command: String? = nil) {
        guard bridgeInitialized else { return }

        // 再起動用にコマンドを保持 + 復旧カウントリセット
        lastPtyCommand = command
        lastPtyCwd = cwd
        ptyRestartCount = 0
        lastPtyExit = nil

        let ptyRows = gridRows

        let result: Int32
        if let cmd = command {
            // コマンド指定あり → vp_bridge_pty_start_command_session
            result = cmd.withCString { cmdPtr in
                if let cwdPath = cwd {
                    return cwdPath.withCString { cwdPtr in
                        vp_bridge_pty_start_command_session(self.sessionId, cwdPtr, cmdPtr, self.gridCols, ptyRows)
                    }
                } else {
                    return vp_bridge_pty_start_command_session(self.sessionId, nil, cmdPtr, self.gridCols, ptyRows)
                }
            }
        } else {
            // コマンド指定なし → 従来通り
            if let cwdPath = cwd {
                result = cwdPath.withCString { ptr in
                    vp_bridge_pty_start_session(self.sessionId, ptr, self.gridCols, ptyRows)
                }
            } else {
                result = vp_bridge_pty_start_session(self.sessionId, nil, self.gridCols, ptyRows)
            }
        }

        if result == 0 {
            needsDisplay = true
        }
    }

    /// PTY 終了を検知して自動復旧（クールダウン付き）
    ///
    /// 最大3回まで再起動、5秒以内の連続終了はカウントアップ。
    /// 無限ループ防止のため上限で停止。
    func restartPtyIfNeeded() {
        let now = Date()

        // クールダウン: 前回の終了から5秒以内は待つ
        if let lastExit = lastPtyExit, now.timeIntervalSince(lastExit) < Self.restartCooldown {
            return
        }

        // 再起動上限チェック
        guard ptyRestartCount < Self.maxRestartCount else {
            logger.warning("PTY 再起動上限に到達 (\(self.ptyRestartCount)回)")
            return
        }

        lastPtyExit = now
        ptyRestartCount += 1

        logger.info("PTY 終了検知 → 自動復旧 (\(self.ptyRestartCount)/\(Self.maxRestartCount))")
        startPty(cwd: lastPtyCwd, command: lastPtyCommand)
    }

    /// PTY を停止
    func stopPty() {
        guard bridgeInitialized else { return }
        vp_bridge_pty_stop_session(sessionId)
    }

    // MARK: - クリップボード（NSResponder copy:/paste: 対応）

    /// メニュー Edit → Copy (Cmd+C) から呼ばれる — テキストコピー専用
    /// 選択なしの Ctrl+C (SIGINT) は performKeyEquivalent で直接処理する
    @objc func copy(_ sender: Any?) {
        guard normalizedSelection() != nil else { return }
        copySelectionToClipboard()
        selectionStart = nil
        selectionEnd = nil
        needsDisplay = true
    }

    /// メニュー Edit → Paste (Cmd+V) から呼ばれる
    @objc func paste(_ sender: Any?) {
        guard vp_bridge_pty_is_running_session(sessionId) else { return }
        pasteFromClipboard()
    }

    /// クリップボードからテキスト/画像を PTY にペースト
    private func pasteFromClipboard() {
        let pb = NSPasteboard.general
        let useTmux = self.sendMouseEvents
        logger.info("pasteFromClipboard: types=\(pb.types?.map(\.rawValue) ?? []) string=\(pb.string(forType: .string) ?? "nil") useTmux=\(useTmux)")

        if let text = pb.string(forType: .string), !text.isEmpty {
            // tmux 内ターミナル: tmux paste-buffer 経由で送信
            // 素シェル (The Hand): PTY 直接書き込み（bracketed paste 対応）
            if useTmux {
                // tmux のペーストバッファ経由で送信
                // PTY 直接書き込みでは tmux がデータを消費してしまうため
                logger.info("pasteFromClipboard: text=\(text.prefix(50)) len=\(text.count) via tmux paste-buffer")
                DispatchQueue.global(qos: .userInitiated).async {
                    let tmuxBin = "/opt/homebrew/bin/tmux"
                    let bufferName = "vp-paste-\(UUID().uuidString.prefix(8))"
                    let loadProcess = Process()
                    loadProcess.executableURL = URL(fileURLWithPath: tmuxBin)
                    loadProcess.arguments = ["load-buffer", "-b", bufferName, "-"]
                    let pipe = Pipe()
                    loadProcess.standardInput = pipe
                    do {
                        try loadProcess.run()
                        if let data = text.data(using: .utf8) {
                            pipe.fileHandleForWriting.write(data)
                        }
                        pipe.fileHandleForWriting.closeFile()
                        loadProcess.waitUntilExit()

                        let pasteProcess = Process()
                        pasteProcess.executableURL = URL(fileURLWithPath: tmuxBin)
                        pasteProcess.arguments = ["paste-buffer", "-b", bufferName, "-d", "-p"]
                        try pasteProcess.run()
                        pasteProcess.waitUntilExit()
                        logger.info("tmux paste-buffer: done (buffer: \(bufferName))")
                    } catch {
                        logger.error("tmux paste failed: \(error)")
                    }
                }
            } else {
                // 素シェル (The Hand): PTY 直接書き込み + bracketed paste
                logger.info("pasteFromClipboard: text=\(text.prefix(50)) len=\(text.count) via PTY direct write")
                let bracketStart = "\u{1B}[200~"
                let bracketEnd = "\u{1B}[201~"
                let payload = bracketStart + text + bracketEnd
                if let data = payload.data(using: .utf8) {
                    data.withUnsafeBytes { ptr in
                        _ = vp_bridge_pty_write_session(
                            sessionId,
                            ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                            UInt32(ptr.count)
                        )
                    }
                }
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
        let firstResp = window?.firstResponder === self
        // first responder でないペインはショートカットを処理しない（メニューに委譲）
        guard firstResp else { return false }
        guard event.modifierFlags.contains(.command),
              let ch = event.charactersIgnoringModifiers else {
            return super.performKeyEquivalent(with: event)
        }

        switch ch {
        case "v":
            // Cmd+V: 直接ペースト（paste(nil) は NSResponder チェーンで迷子になることがあるため直接呼ぶ）
            logger.info("performKeyEquivalent: Cmd+V detected, calling pasteFromClipboard()")
            pasteFromClipboard()
            return true
        case "c":
            // Cmd+C: 選択あり → メニューの copy: に委譲（コピー）
            //         選択なし → Ctrl+C (SIGINT) を直接送信
            // メニューの "Copy" はテキストコピー専用の意味論を維持する
            if normalizedSelection() != nil {
                return false // メニュー経由で copy(_:) が呼ばれる
            } else if vp_bridge_pty_is_running_session(sessionId) {
                let ctrlC: [UInt8] = [0x03]
                ctrlC.withUnsafeBufferPointer { ptr in
                    _ = vp_bridge_pty_write_session(sessionId, ptr.baseAddress!, 1)
                }
                return true
            }
            // セッション未起動 + 選択なし → システムに委譲
            return super.performKeyEquivalent(with: event)
        case "S":
            // Cmd+Shift+S: スクリーンショット
            if let path = captureScreenshot() {
                // クリップボードにパスをコピー
                let pb = NSPasteboard.general
                pb.clearContents()
                pb.setString(path, forType: .string)
            }
            return true
        default:
            return super.performKeyEquivalent(with: event)
        }
    }

    override func keyDown(with event: NSEvent) {
        // Split Navigator アクティブ時: ナビキーをインターセプト
        if splitNavigatorActive {
            if let navKey = splitNavigatorKeyFromEvent(event) {
                NotificationCenter.default.post(
                    name: .splitNavigatorKey,
                    object: nil,
                    userInfo: ["key": navKey]
                )
                return // PTY には送らない
            }
        }

        guard vp_bridge_pty_is_running_session(sessionId) else {
            super.keyDown(with: event)
            return
        }

        let imeMarked = hasMarkedText()

        // Cmd ショートカットは performKeyEquivalent で処理済み
        // ここに到達する Cmd イベントは未処理のもの（Cmd+`, Cmd+Q 等）
        if event.modifierFlags.contains(.command) {
            super.keyDown(with: event)
            return
        }

        // IME 変換中はすべて IME に委譲
        if imeMarked {
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
        case 48:                                      // Tab / Shift+Tab
            if modifiers.contains(.shift) {
                return [0x1B, 0x5B, 0x5A]             // Shift+Tab → CSI Z (Backtab)
            }
            return [0x09]
        case 51:  return [0x7F]                    // Delete (Backspace)
        case 53:  return [0x1B]                    // Escape
        case 117: return [0x1B, 0x5B, 0x33, 0x7E] // Forward Delete
        case 123: return [0x1B, 0x5B, 0x44]        // ←
        case 124: return [0x1B, 0x5B, 0x43]        // →
        case 125: return [0x1B, 0x5B, 0x42]        // ↓
        case 126: return [0x1B, 0x5B, 0x41]        // ↑
        case 115: return [0x1B, 0x5B, 0x48]        // Home
        case 119: return [0x1B, 0x5B, 0x46]        // End
        // PageUp/PageDown は addLocalMonitorForEvents で SGR マウスイベントとして送信
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

        // Cmd キーの押下/解放でカーソルを更新（URL ホバー表示の切り替え）
        if let window = self.window {
            let mouseLocation = window.mouseLocationOutsideOfEventStream
            let pos = gridPosition(from: mouseLocation)
            if event.modifierFlags.contains(.command),
               urlAtPosition(col: pos.col, row: pos.row) != nil {
                NSCursor.pointingHand.set()
            } else {
                NSCursor.iBeam.set()
            }
        }
    }

    // MARK: - URL 検出（Cmd+Click でブラウザ起動）

    /// URL 検出用の正規表現（https:// または http:// で始まる URL）
    private static let urlRegex: NSRegularExpression? = {
        // Rust 側 (tui_cmd.rs) と統一: `)` はパターンに含め、末尾除去で処理
        try? NSRegularExpression(pattern: "https?://[^\\s<>\"'）」\\]]+", options: [])
    }()

    /// トラッキングエリアを設定（mouseMoved イベントを受信するため）
    private func setupTrackingArea() {
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        mouseTrackingArea = area
    }

    /// ビューサイズ変更時にトラッキングエリアを再構築
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let old = mouseTrackingArea {
            removeTrackingArea(old)
        }
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        mouseTrackingArea = area
    }

    /// 指定行のテキストを cellBuffer から抽出
    private func extractRowText(row: Int) -> String {
        let cols = Int(gridCols)
        var text = ""
        var skipNext = false
        for col in 0..<cols {
            if skipNext { skipNext = false; continue }
            let idx = row * cols + col
            guard idx < cellBuffer.count else { continue }
            let cell = cellBuffer[idx]
            let ch = cellString(from: cell)
            // WIDE_CHAR (bit 6) の場合、次のセル（スペーサー）をスキップ
            if (cell.flags & (1 << 6)) != 0 {
                skipNext = true
            }
            text += ch.isEmpty ? " " : ch
        }
        return text
    }

    /// 行テキスト中の指定カラム位置にある URL を返す
    ///
    /// セルグリッドのカラム位置とテキストのカラム位置を照合し、
    /// Cmd+Click 位置が URL 範囲内かを判定する。
    private func urlAtPosition(col: Int, row: Int) -> URL? {
        guard let regex = Self.urlRegex else { return nil }
        let text = extractRowText(row: row)
        let nsText = text as NSString
        let matches = regex.matches(in: text, options: [], range: NSRange(location: 0, length: nsText.length))

        // ワイド文字を考慮してテキストインデックス → カラム位置のマッピングを構築
        let cols = Int(gridCols)
        var textIdx = 0
        var colToTextIdx: [Int: Int] = [:]
        var skipNext = false
        for c in 0..<cols {
            if skipNext { skipNext = false; continue }
            let idx = row * cols + c
            guard idx < cellBuffer.count else { continue }
            let cell = cellBuffer[idx]
            let ch = cellString(from: cell)
            let charLen = ch.isEmpty ? 1 : (ch as NSString).length
            colToTextIdx[c] = textIdx
            if (cell.flags & (1 << 6)) != 0 {
                // ワイド文字: 次カラムも同じテキスト位置
                colToTextIdx[c + 1] = textIdx
                skipNext = true
            }
            textIdx += charLen
        }

        guard let clickTextIdx = colToTextIdx[col] else { return nil }

        for match in matches {
            if clickTextIdx >= match.range.location
                && clickTextIdx < match.range.location + match.range.length {
                let urlString = nsText.substring(with: match.range)
                // 末尾の句読点・括弧を除去（URL の一部でない可能性が高い）
                let trimmed = urlString.replacingOccurrences(
                    of: "[.,;:!?)）】」』》〉\\]]+$",
                    with: "",
                    options: .regularExpression
                )
                return URL(string: trimmed)
            }
        }
        return nil
    }

    /// Cmd+ホバー時にカーソルをポインティングハンドに変更（行キャッシュで負荷軽減）
    override func mouseMoved(with event: NSEvent) {
        let pos = gridPosition(from: event.locationInWindow)
        guard event.modifierFlags.contains(.command) else {
            NSCursor.iBeam.set()
            lastHoveredUrl = nil
            return
        }
        // 行またはカラムが変わった場合のみ URL を再検出
        if pos.row != lastHoveredRow || pos.col != lastHoveredCol {
            lastHoveredRow = pos.row
            lastHoveredCol = pos.col
            lastHoveredUrl = urlAtPosition(col: pos.col, row: pos.row)
        }
        if lastHoveredUrl != nil {
            NSCursor.pointingHand.set()
        } else {
            NSCursor.iBeam.set()
        }
    }

    // MARK: - テキスト選択

    private var selectionStart: (col: Int, row: Int)?
    private var selectionEnd: (col: Int, row: Int)?
    private var isDragging = false

    private func gridPosition(from point: NSPoint) -> (col: Int, row: Int) {
        let local = convert(point, from: nil)
        var col = max(0, min(Int(gridCols) - 1, Int(local.x / cellWidth)))
        let row = max(0, min(Int(gridRows) - 1, Int((bounds.height - local.y) / cellHeight)))
        // スペーサーセル（wide 文字の右半分）へのクリックは wide 文字本体にスナップ
        let cols = Int(gridCols)
        if col > 0 {
            let prevIdx = row * cols + (col - 1)
            if prevIdx < cellBuffer.count && (cellBuffer[prevIdx].flags & (1 << 6)) != 0 {
                col -= 1
            }
        }
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

        // VP Pane フォーカス通知（クリックしたペインを focusedPaneId に更新）
        if let paneId = paneId {
            NotificationCenter.default.post(
                name: .vpPaneFocused,
                object: nil,
                userInfo: ["paneId": paneId]
            )
        }

        // Cmd+Click: URL をブラウザで開く
        if event.modifierFlags.contains(.command) {
            let pos = gridPosition(from: event.locationInWindow)
            if let url = urlAtPosition(col: pos.col, row: pos.row) {
                NSWorkspace.shared.open(url)
                return
            }
        }

        let pos = gridPosition(from: event.locationInWindow)

        // SGR マウスイベントを PTY に送信（tmux ペインフォーカス切替等）
        // 素シェル（The Hand 等）ではマウスイベントを送らない（制御文字がそのまま表示されるため）
        if sendMouseEvents && vp_bridge_pty_is_running_session(sessionId) {
            let col = pos.col + 1  // 1-based
            let row = pos.row + 1
            let seq = "\u{1B}[<0;\(col);\(row)M"
            if let data = seq.data(using: .ascii) {
                data.withUnsafeBytes { ptr in
                    _ = vp_bridge_pty_write_session(
                        sessionId,
                        ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                        UInt32(ptr.count))
                }
            }
        }

        selectionStart = pos
        selectionEnd = pos
        isDragging = true
        needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) {
        guard isDragging else { return }
        let pos = gridPosition(from: event.locationInWindow)
        selectionEnd = pos
        needsDisplay = true

        // SGR マウスドラッグイベント（tmux のマウス選択等）
        if sendMouseEvents && vp_bridge_pty_is_running_session(sessionId) {
            let col = pos.col + 1
            let row = pos.row + 1
            let seq = "\u{1B}[<32;\(col);\(row)M"  // 32 = button1 + motion
            if let data = seq.data(using: .ascii) {
                data.withUnsafeBytes { ptr in
                    _ = vp_bridge_pty_write_session(
                        sessionId,
                        ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                        UInt32(ptr.count))
                }
            }
        }
    }

    override func mouseUp(with event: NSEvent) {
        guard isDragging else { return }
        isDragging = false
        let pos = gridPosition(from: event.locationInWindow)
        selectionEnd = pos

        // SGR マウスリリースイベント
        if sendMouseEvents && vp_bridge_pty_is_running_session(sessionId) {
            let col = pos.col + 1
            let row = pos.row + 1
            let seq = "\u{1B}[<0;\(col);\(row)m"  // 小文字 m = release
            if let data = seq.data(using: .ascii) {
                data.withUnsafeBytes { ptr in
                    _ = vp_bridge_pty_write_session(
                        sessionId,
                        ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                        UInt32(ptr.count))
                }
            }
        }

        if selectionStart?.col == selectionEnd?.col && selectionStart?.row == selectionEnd?.row {
            selectionStart = nil
            selectionEnd = nil
        }
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
            var skipNext = false
            for col in colStart...colEnd {
                if skipNext { skipNext = false; continue }
                let idx = row * cols + col
                guard idx < cellBuffer.count else { continue }
                let cell = cellBuffer[idx]
                let ch = cellString(from: cell)
                // WIDE_CHAR (bit 6) の場合、次のセル（スペーサー）をスキップ
                if (cell.flags & (1 << 6)) != 0 {
                    skipNext = true
                }
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

        // SGR マウスホイールイベント: \e[<button;col;rowM
        // button 64 = scroll up, 65 = scroll down
        let pos = gridPosition(from: event.locationInWindow)
        let col = pos.col + 1  // 1-based
        let row = pos.row + 1  // 1-based
        let button = lines > 0 ? 64 : 65
        let count = abs(lines)

        for _ in 0..<min(count, 10) {
            let seq = "\u{1B}[<\(button);\(col);\(row)M"
            if let data = seq.data(using: .ascii) {
                data.withUnsafeBytes { ptr in
                    _ = vp_bridge_pty_write_session(
                        sessionId,
                        ptr.baseAddress!.assumingMemoryBound(to: UInt8.self),
                        UInt32(ptr.count))
                }
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
            logger.info("[IME] insertText: unknown type \(type(of: string))")
            return
        }

        markedString = NSMutableAttributedString()
        _markedRange = NSRange(location: NSNotFound, length: 0)

        guard vp_bridge_pty_is_running_session(sessionId) else {
            logger.info("[IME] insertText: PTY not running, dropped")
            return
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
        // markedString.length は UTF-16 単位なので全角と合わない → displayWidth で grapheme 単位に
        let markedWidth = TerminalView.displayWidth(of: markedString.string)
        let cursorCol = CGFloat(cursor.x) + CGFloat(markedWidth)
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

// MARK: - Split Navigator キーマッピング

extension TerminalView {
    /// Split Navigator 用キーイベント → 文字列キー名への変換
    ///
    /// ナビゲーターが消費するキーのみ変換、それ以外は nil（PTY に通す）
    func splitNavigatorKeyFromEvent(_ event: NSEvent) -> String? {
        // IME 変換中はパススルー（候補選択の矢印キー等を横取りしない）
        if hasMarkedText() {
            return nil
        }

        // Modifier キー付きは無視（Cmd+D 等はメニュー経由で処理済み）
        if event.modifierFlags.intersection([.command, .control, .option]).isEmpty == false {
            return nil
        }

        switch event.keyCode {
        case 123: return "left"   // ←
        case 124: return "right"  // →
        case 126: return "up"     // ↑（未使用だが将来用）
        case 125: return "down"   // ↓（未使用だが将来用）
        case 36:  return "enter"  // Return
        case 53:  return "escape" // Escape
        default: break
        }

        // 数字キー 1〜4
        if let chars = event.charactersIgnoringModifiers,
           chars.count == 1,
           let ch = chars.first,
           ch >= "1" && ch <= "4" {
            return String(ch)
        }

        return nil
    }
}
