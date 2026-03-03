# Vantage Point Terminal Window 調査結果

## 1. ファイル構造

### Webディレクトリ
- `/web/index.html` — メインUI（ターミナル + サイドパネル）
- `/web/canvas.html` — Canvas表示用UI（デバッグ・ログビュワー用）

### Rust側コード
**Stand（WebSocketサーバー）**:
- `/crates/vantage-point/src/stand/pty.rs` — PTYセッション管理
- `/crates/vantage-point/src/stand/routes/ws.rs` — WebSocketメッセージハンドラ
- `/crates/vantage-point/src/stand/server.rs` — Axumサーバー本体

**Terminal Window（ネイティブウィンドウ）**:
- `/crates/vantage-point/src/terminal_window.rs` — macOS native window（tao + Daemon連携）
- `/crates/vantage-point/src/terminal/mod.rs` — VTエミュレーション
- `/crates/vantage-point/src/terminal/state.rs` — グリッド状態管理
- `/crates/vantage-point/src/terminal/renderer.rs` — macOS ネイティブレンダラー（CoreText）

**Daemon（PTY多重化）**:
- `/crates/vantage-point/src/daemon/server.rs` — QUIC RPC server
- `/crates/vantage-point/src/daemon/client.rs` — Terminal Window用クライアント
- `/crates/vantage-point/src/daemon/pty_slot.rs` — PTY管理スロット

**Protocol**:
- `/crates/vantage-point/src/protocol/messages.rs` — WebSocket & IPCメッセージ定義

---

## 2. WebSocket Terminal Protocol

### Browser → Stand メッセージ

| メッセージ | 形式 | 説明 |
|-----------|------|------|
| **TerminalInput** | `{"type":"terminal_input","data":"base64string"}` | PTY入力（base64エンコード） |
| **TerminalResize** | `{"type":"terminal_resize","cols":80,"rows":24}` | PTYリサイズ |

### Stand → Browser メッセージ

| メッセージ | 形式 | 説明 |
|-----------|------|------|
| **TerminalOutput** | `{"type":"terminal_output","data":"base64string"}` | PTY出力（base64エンコード） |
| **TerminalReady** | `{"type":"terminal_ready"}` | PTYセッション準備完了 |

---

## 3. Frontend Terminal 実装 (index.html)

### 利用ライブラリ
- **ghostty-web** v0.4.0 — WASM ターミナルエミュレータ
  - Terminal class + FitAddon でリサイズ対応
  - `onData()` → WebSocket送信

### キーコンポーネント

**container keydown handler** (行971-1010):
- Tab処理: WKWebView の横取り対策 → ESCシーケンス送信
- keyCode:229対策: macOS日本語IME検出
  - 遅延キュー（20ms）で compositionstart 発火を待つ
  - compositionstart → IMEセクション展開
  - 発火しなければ英字モード → 直接PTYに送信

**IME管理** (行889-944):
- 専用 `#ime-section` HTML要素でIME組成テキスト管理
- compositionend → テキストをPTYに送信
- Escape キーで IMEセクション閉鎖 → ターミナルにフォーカス戻す

**リサイズ対応** (行1019-1029):
- ResizeObserver で container 監視
- fitAddon.fit() で セル計算
- proposeDimensions() → WebSocket送信

### グローバル関数
```javascript
window.sendTerminalInput(data)  // 文字列 → base64 → WebSocket
window.sendTerminalResize(cols, rows)  // WebSocket送信
window.terminalWriteCallback = bytes => {  // 受信データをターミナルに書き込み
    ghosttyTerm.write(bytes)
}
```

---

## 4. Rust側 PTY管理 (pty.rs)

### PtySession 構造体
```rust
pub struct PtySession {
    writer: Box<dyn Write + Send>,
    pair: portable_pty::PtyPair,
}

impl PtySession {
    pub fn spawn(cwd: &str, cols: u16, rows: u16) -> Result<...>
    pub fn write(&mut self, data: &[u8]) -> Result<()>
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()>
}
```

### PTY出力リーダータスク
- `start_pty_reader_task()`: spawn_blocking でPTYを読み取り
- 4096バイトバッファ → base64エンコード
- `StandMessage::TerminalOutput` で broadcast 送信
- クライアント数 = 0 の場合も黙って続行（正常）

---

## 5. ネイティブ Terminal Window (terminal_window.rs)

### モード: Daemon経由

```
Terminal Window → DaemonBridge (mpsc channel) → Daemon (QUIC RPC) → PTY session
```

### 入力フロー
- keydown handler → DaemonInputCommand enum → mpsc → Daemon.write_input()
- base64 encode は DaemonClient 内部で自動

### 出力フロー
- Daemon.read_output() ポーリング（50msタイムアウト）
- VTバイト → TerminalState.feed_bytes()
- TerminalState → TerminalView へグリッド情報渡す
- TerminalView.request_redraw() で CoreText レンダリング

---

## 6. 主要な機能

### 入力制御
- テキスト入力（ASCIIおよび日本語IME）
- 特殊キー（Tab, Arrow, Home, End, Delete など）
- Cmd+C/V (native window) / IME統合
- クリップボード連携（macOS pbcopy/pbpaste）

### 出力制御
- VTシーケンス解析（ghostty-web / alacritty_terminal）
- リアルタイムストリーミング（base64エンコード）
- グリッド管理（セル単位でのレンダリング）
- テキスト選択・コピー（native window）

### ウィンドウ管理
- リサイズ追従（ResizeObserver/frame resize）
- マルチペイン（Daemon側で実装）
- タブ管理（native window）

---

## 7. 注目すべき実装ポイント

1. **遅延PTY起動** (ws.rs:333-339)
   - TerminalResize 受信時にのみPTY起動
   - ブラウザが正しいサイズを報告してから起動することで初期表示の不整合を防止

2. **macOS IME対策** (index.html:983-1010)
   - keyCode:229での遅延キューイング（20ms）
   - compositionstart / compositionend イベント統合

3. **base64エンコード** (複数箇所)
   - WebSocket通信は全てbase64（バイナリセーフ）
   - ブラウザ側: TextEncoder/atob/btoa
   - Rust側: base64 crate

4. **マルチクライアント対応** (pty.rs:99-105)
   - broadcast channel で複数接続者に同時配信
   - send error は黙って無視（正常動作）

5. **Daemon 多重化** (daemon/server.rs)
   - QUIC RPC で複数セッション・ペイン管理
   - read_output() ポーリング（50msタイムアウト）
