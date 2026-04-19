# v0.13.0 Dogfooding チェックリスト

> v0.13.0 の日本語 UX 完全対応 (P1+P2+P4+P5+P-S1〜S5) を実機で検証するためのチェックリスト。
> **launch (2026-04-21) 前の最終動作確認**として使う。

## Pre-flight（環境準備）

- [ ] `vp --version` → `0.13.0`
- [ ] `defaults read /Applications/VantagePoint.app/Contents/Info.plist CFBundleShortVersionString` → `0.13.0`
- [ ] `/Applications/VantagePoint.app/Contents/MacOS/VantagePoint` のビルド時刻が最新
- [ ] 稼働 SP をすべて `vp restart` で v0.13.0 バイナリに切替済
- [ ] VantagePoint.app を再起動 (起動中なら Quit → 再起動)

---

## Category A: 罫線・フォント (P1, P4, P5)

### 罫線途切れ修正 (P1)

- [ ] CC の入力ボックス `╭────╮│  │╰────╯` の水平線が**連続した 1 本線**で表示
- [ ] 日本語混在コンテンツを入力中でも罫線が崩れない（以前は 1 つ置きに飛んでいた）
- [ ] `tree` コマンドで `├── └── │` が正しく連続
- [ ] `lsd -l` の罫線表示が正常
- [ ] `cargo build` の progress bar `█ ▌ ░` が正しく 1 セル幅で描画

### フォント (P4)

- [ ] 日本語「あいうえお」が**豆腐（□）で表示されず**正常描画
- [ ] CJK 多用コンテンツ（数千行の日本語ログ）でスクロール性能が良好
- [ ] コンソール出力に「fallback font」系のログが大量に出ていない

### 全角 underline (P5)

- [ ] `echo -e "\e[4m全角下線\e[0m"` → 「全角下線」全幅に下線
- [ ] CC の選択肢リスト等で日本語 hover underline が全幅

### まる数字・記号

- [ ] まる数字 `①②③④⑤` は全角幅で正しく表示
- [ ] `★ ◯ ●` 等は全角扱いでセル境界に揃う
- [ ] General Punctuation `" " ' ' – …` は半角扱い

---

## Category B: IME 日本語入力 (P2, P2+)

### Inline 描画

- [ ] 「がんばれ」と打ち始めた時、**確定前でも画面に文字が表示される**
- [ ] 変換中の文字に半透明 accent 色背景 + 点線下線
- [ ] selectedRange（IME が編集中の節）が**別色（濃いめ accent）でハイライト**
- [ ] 節を切り替える（Shift+←/→）と selectedRange ハイライトが移動

### IME 候補ウィンドウ位置

- [ ] 半角文字カーソル位置で候補が正しくカーソル直下
- [ ] 全角文字の後（例 `あい|`）で候補が**全角幅を考慮した位置**に出る
- [ ] スクロール後も候補位置がズレない

### Cursor 連動

- [ ] 変換中はカーソルがマーク文字の**末尾に移動**
- [ ] 確定（Enter）後、カーソルが確定テキストの末尾に戻る

### Stuck 解消 (P-S1)

- [ ] 「がんば」と打って未確定 → 別ペインに Cmd+P で切替 → 戻る → **新しい keyDown が IME に食われない**
- [ ] Cmd+Tab で別アプリ → 戻った時、**最初のキー入力が直接 PTY に流れる**（IME に stuck しない）
- [ ] ウィンドウを別 Space に移動してから戻る → IME state が正常

---

## Category C: 選択 (P-S2, P-S4)

### ダブルクリック単語選択

- [ ] ファイルパス `/Users/makoto/repos/vantage-point/README.md` をダブルクリック → 全体が 1 単語として選択
- [ ] URL `https://example.com/path?query=1` → 全体選択
- [ ] 関数名 `some_function_name` → underscore 含む全体選択
- [ ] 日本語の連続漢字「東京都渋谷区」→ 連続 CJK として 1 単語選択

### トリプルクリック行選択

- [ ] 任意の行をトリプルクリック → 行全体が選択ハイライト（gridCols 全幅）

### Shift+クリック拡張

- [ ] ドラッグで選択 → Shift+クリック別位置 → 選択範囲が**新しい位置まで拡張**（開始点は維持）

### Cmd+A 全選択

- [ ] Edit メニュー → Select All が**有効（enabled）**になっている
- [ ] Cmd+A → 画面全体が選択ハイライト

---

## Category D: コピペ (P-S3)

### NFC 正規化

- [ ] Finder でファイル名「がんばれ.txt」をコピー（NFD） → VP にペースト → PTY に正しく NFC「がんばれ」として届く
- [ ] `ls -la` や `find` で NFD ファイル名が化けない

### 改行正規化

- [ ] VS Code で Windows 改行（\r\n）を含むコピー → ペースト → **空行が挟まらない**
- [ ] macOS Numbers / Excel のセル範囲コピー → ペースト → 正常

### 制御文字フィルタ

- [ ] 悪意のある `\x00 \x07 \x1b[2;H` 等を含むテキスト（テスト用）をペースト → **制御文字が除去され安全**
- [ ] タブ文字 `\t` は維持される
- [ ] `\x1b[201~`（bracketed paste 終了シーケンス）を含むテキスト → 除去されてターミナル状態異常なし

### サイズ上限

- [ ] 2MB 以上のテキストをペースト → 1MB で切り詰め、**アプリフリーズなし**

---

## Category E: フォーカス (P-S5)

### 取り合い race 解消

- [ ] ペイン分割直後（Cmd+D）、**意図したペインがフォーカスを持つ**（ 0.5 秒後にフォーカスが奪われない）
- [ ] 複数ウィンドウを高速で切替えてもフォーカスがちらつかない

### カーソル視覚

- [ ] **フォーカスペインのカーソル**: 白塗り四角
- [ ] **非フォーカスペインのカーソル**: 中空（線のみ）
- [ ] フォーカス移動時、両方のカーソル状態が即座に切り替わる

### paste 誤配防止

- [ ] 非フォーカスペインに paste を送っても**フォーカスペインに届く**（menu Paste / Cmd+V）

---

## Known Limitations（v0.14 以降で対応）

以下は P3（FFI `CellData ch: [u8;5]→[u8;16]` 拡張）で対応予定、**現バージョンでは既知制限**:

- NFD 濁点の分離表示: Finder などが NFD で送る「が=か+゛」は**描画時に「か」だけ見える**（ペースト経由は NFC 正規化されるので OK）
- Family emoji `👨‍👩‍👧‍👦` の ZWJ 連結: 単独絵文字として表示
- VS16 (color emoji selector): monochrome 表示（`❤️` が `❤` に）

---

## 回帰チェック（既存機能が壊れていないか）

- [ ] Canvas (PP) の表示・show MCP ツール
- [ ] Claude CLI `--resume <id>` からのセッション復帰
- [ ] tmux 連携（split / send-keys）
- [ ] msg_* MCP ツール一式（send / recv / broadcast / ack / peers）
- [ ] WebView 内 JS との双方向通信
- [ ] DistributedNotification 経由の Native App バッジ更新

---

## バグ報告テンプレート

実機確認で issue を見つけた場合、以下のフォーマットで Linear / GitHub に起票:

```
## 症状
（スクリーンショット、録画、キー入力列）

## 再現手順
1.
2.
3.

## 期待動作

## 実際の動作

## 環境
- VP: 0.13.0
- macOS: (系統とバージョン)
- 日本語 IME: ことえり / Google IME / ATOK / etc.

## 仮説（あれば）
```

---

## 完了判定

全 Category のチェックが埋まれば **v0.13.0 は launch ready**。
未チェック項目がある場合、launch 前に hotfix or 既知問題として明示化。
