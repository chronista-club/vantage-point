# VP ネイティブアプリ化 設計書

> **Status**: Draft
> **Created**: 2026-03-12
> **Decision**: vantage-point-mac を拡張、ratatui NSView Backend 自作

## ビジョン

kitty + wry の2ウィンドウ構成を、**1つのネイティブ macOS アプリ**に統合する。
今の TUI の操作感はそのまま維持しつつ、Canvas と Agent Dashboard をネイティブに統合。

## 現状 → 目標

```
現状:
  kitty (ターミナル)          wry (WebView)
  ┌──────────────────┐      ┌──────────────────┐
  │ vp start (TUI)   │      │ Canvas           │
  │ ratatui + PTY    │      │ HTML/JS          │
  │ Claude CLI 対話   │      │ Markdown/Mermaid │
  └──────────────────┘      └──────────────────┘
        tmux で pane 管理

目標:
  VP.app (ネイティブ macOS)
  ┌─────────────────────────────────────────┐
  │ ┌─ Terminal ──────┐ ┌─ Canvas ────────┐ │
  │ │ ratatui TUI     │ │ WKWebView       │ │
  │ │ (NSView 描画)   │ │ (既存 HTML/JS)  │ │
  │ │                 │ │                 │ │
  │ │ Claude CLI 対話  │ │ Markdown/Mermaid│ │
  │ └─────────────────┘ │ Agent Dashboard │ │
  │                      └─────────────────┘ │
  ├──────────────────────────────────────────┤
  │ メニューバー (既存 NSPopover)             │
  └──────────────────────────────────────────┘
        tmux は対等ツールとして連携
```

## アーキテクチャ

### レイヤー構成

```
┌─────────────────────────────────────────────────┐
│ Swift / SwiftUI                                  │
│  ├── AppDelegate (メニューバー, 既存)             │
│  ├── MainWindow (NSWindow, 新規)                 │
│  │    ├── TerminalPane (NSView, ratatui 描画)    │
│  │    ├── CanvasPane (WKWebView)                 │
│  │    └── AgentDashboard (SwiftUI)               │
│  ├── TheWorldClient (既存)                       │
│  ├── ConfigManager (既存)                         │
│  └── UnisonClient (新規, QUIC 通信)              │
├─────────────────────────────────────────────────┤
│ FFI Layer (C ABI)                                │
│  └── vp-bridge: Cell グリッド + イベント受け渡し   │
├─────────────────────────────────────────────────┤
│ Rust                                             │
│  ├── vp-bridge (新規 crate)                      │
│  │    ├── NSViewBackend (ratatui Backend impl)   │
│  │    └── FFI exports (C ABI functions)          │
│  ├── vantage-point (既存)                        │
│  │    ├── Process / Unison QUIC server           │
│  │    ├── MCP server                             │
│  │    ├── TUI (ratatui, backend 差し替え)         │
│  │    └── tmux_actor (対等ツール)                 │
│  └── vantage-core (既存)                         │
└─────────────────────────────────────────────────┘
```

### ratatui NSView Backend

ratatui の `Backend` trait を実装する新規 crate `vp-bridge`。

```rust
// crates/vp-bridge/src/lib.rs

/// ratatui の Cell グリッドを外部に公開するための Backend
pub struct NativeBackend {
    width: u16,
    height: u16,
    buffer: Buffer,
}

impl Backend for NativeBackend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        // Cell データを内部バッファに蓄積
        for (x, y, cell) in content {
            self.buffer[(x, y)] = cell.clone();
        }
        // Swift 側にコールバックで通知
        notify_frame_ready();
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        Ok(Size::new(self.width, self.height))
    }
    // ...
}
```

#### FFI データフロー

```
Rust (ratatui)                    Swift (NSView)
     │                                 │
     │  frame_ready コールバック         │
     ├────────────────────────────────►│
     │                                 │
     │  vp_bridge_get_cell(x, y)       │
     │◄────────────────────────────────┤
     │  → CellData { ch, fg, bg, ... } │
     ├────────────────────────────────►│
     │                                 │
     │           NSView.setNeedsDisplay │
     │                                 ▼
     │                     Core Text で描画
```

#### CellData (FFI 構造体)

```rust
#[repr(C)]
pub struct CellData {
    /// UTF-8 文字列 (最大 4 バイト + null)
    pub ch: [u8; 5],
    /// 前景色 (RGBA)
    pub fg: u32,
    /// 背景色 (RGBA)
    pub bg: u32,
    /// フラグ (bold, italic, underline, inverse)
    pub flags: u8,
}
```

### Canvas Pane

既存の `web/canvas.html` + JS をそのまま WKWebView で表示。
通信は Unison QUIC → WKWebView の JS bridge (WKScriptMessageHandler)。

現在の wry WebView と同じ WebSocket 接続パターンを維持:
- Canvas JS → WebSocket → Process (Axum HTTP server)
- Process → TopicRouter → canvas チャネル → push

### tmux 連携

tmux は VP が「所持する道具」。あれば使い、なくても動く。

```
VP.app
├── tmux 検出 (is_inside_tmux / tmux ls)
├── tmux あり:
│   ├── 既存 tmux pane の可視化 (Agent Dashboard)
│   ├── tmux_agent_deploy / status / send (MCP 経由)
│   └── ccwire / ccws ワーカー管理
└── tmux なし:
    └── 全機能が VP.app 内で完結
```

## 実装フェーズ

### Phase 1: vp-bridge crate + 最小 NSView

1. `crates/vp-bridge/` に新規 crate 作成
2. `NativeBackend` (ratatui Backend trait) 実装
3. FFI 関数 export (`vp_bridge_init`, `vp_bridge_get_cell`, etc.)
4. Swift 側で NSView サブクラスを作り、Core Text で Cell 描画
5. 固定テキスト（"Hello from ratatui"）が NSView に表示されることを確認

### Phase 2: PTY 接続 + TUI 表示

1. Rust 側で PTY (alacritty_terminal) → ratatui → NativeBackend のパイプ構築
2. Swift 側でキーボード入力 → FFI → PTY write
3. Claude CLI が NSView 内で動作することを確認

### Phase 3: メインウィンドウ統合

1. vantage-point-mac に MainWindow (NSWindow) 追加
2. TerminalPane + CanvasPane (WKWebView) のレイアウト
3. メニューバーからプロジェクト選択 → メインウィンドウ表示

### Phase 4: Canvas + Agent Dashboard

1. WKWebView で既存 Canvas HTML/JS を表示
2. Unison QUIC client (Swift) で Process と通信
3. Agent Dashboard (SwiftUI) で #97 メタデータ表示

### Phase 5: Polish

1. ウィンドウ管理（リサイズ、フルスクリーン）
2. テーマ・フォント設定
3. Mac App Store 準備

## 既存資産マップ

### vantage-point-mac から継続利用

| ファイル | 用途 | 変更 |
|---------|------|------|
| TheWorldClient.swift | TheWorld API | そのまま |
| TheWorldTypes.swift | API 型定義 | そのまま |
| ConfigManager.swift | config.toml | そのまま |
| BonjourBrowser.swift | プロセス発見 | そのまま |
| UpdateService.swift | 自動アップデート | そのまま |
| UserPromptService.swift | プロンプト | そのまま |
| AppDelegate.swift | メニューバー | 拡張（メインウィンドウ追加） |
| PopoverView/ViewModel | ダッシュボード | 拡張（ウィンドウ表示導線） |
| ProcessManager.swift | Port scan | 廃止 |

### vantage-point (Rust) からの変更

| モジュール | 変更 |
|-----------|------|
| tui/ | Backend を crossterm → NativeBackend に差し替え |
| process/ | 変更なし |
| mcp.rs | 変更なし |
| tmux_actor.rs | 変更なし（#97 のまま） |
| protocol/ | 変更なし |

## リスク

| リスク | 影響 | 対策 |
|--------|------|------|
| FFI の複雑さ | Cell データ受け渡しのオーバーヘッド | 共有メモリ or バッチ転送で最適化 |
| Core Text 描画パフォーマンス | 高速スクロール時のカクつき | Metal バッキング or ダーティ領域のみ再描画 |
| WKWebView セキュリティ制限 | localhost 接続の制約 | WKURLSchemeHandler でバイパス |
| ratatui Backend API 変更 | アップデート時の追従 | trait が安定しているので低リスク |
