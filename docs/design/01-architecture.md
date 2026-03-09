# VP-DESIGN-001: アーキテクチャ設計

> **Status**: Active
> **Created**: 2025-12-16
> **Updated**: 2026-03-10
> **Version**: 0.8.2
> **Implements**: VP-SPEC-001 (REQ1〜REQ7)

---

## Overview

VP のシステムは 2 層構造: **TheWorld**（グローバルデーモン）と **Process**（プロジェクト単位のサーバー）。
各 Process は Stand（Capability）を保持し、TUI・Canvas・MCP を通じてユーザーと AI に開発体験を提供する。

---

## System Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                   TheWorld 👑 (port 32000)                    │
│                   Process Manager / 常駐デーモン               │
│  ┌────────────────────────────────────────────────────────┐   │
│  │  ProcessManagerCapability (InMemory HashMap)           │   │
│  │  → Process 登録・発見・ライフサイクル管理 (REQ6.1, REQ6.4)   │   │
│  └────────────────────────────────────────────────────────┘   │
└──────────────────────┬───────────────────────────────────────┘
                       │ HTTP API (register/unregister/list)
     ┌─────────────────┼─────────────────┐
     ▼                 ▼                 ▼
┌──────────┐    ┌──────────┐    ┌──────────┐
│ Process 0│    │ Process 1│    │ Process N│
│ port     │    │ port     │    │ port     │
│ 33000    │    │ 33001    │    │ 33000+N  │
└──────────┘    └──────────┘    └──────────┘
```

---

## Process 内部構成

各 Process（⭐ Star Platinum）はプロジェクトの開発サーバー本体。
Stand（Capability）を束ね、複数の通信レイヤーで外部と接続する。

```
Process (Star Platinum ⭐)
├── AppState
│   ├── Hub (broadcast::channel, buffer=10000)
│   ├── TopicRouter (MQTT v5 スタイル Topic ルーティング)
│   ├── SessionManager (セッション永続化)
│   ├── PtyManager (マルチセッション PTY 管理)
│   ├── FileWatcherManager (ファイル監視)
│   ├── TmuxHandle (tmux 統合)
│   └── terminal_token (UUID v4 認証)
│
├── Capabilities (ProcessCapabilities)
│   ├── AgentCapability      📖 Heaven's Door (REQ2)
│   ├── ProtocolCapability   🧭 Paisley Park (REQ3)
│   ├── MidiCapability       🍇 Hermit Purple (REQ5)
│   └── BonjourCapability    (mDNS 発見)
│
├── Communication Layers
│   ├── HTTP (Axum)          REST API + 静的ファイル
│   ├── WebSocket            Canvas リアルタイム通信
│   └── QUIC (Unison)        MCP / TUI 高速チャネル
│
└── Code Execution
    └── ProcessRunner        🌿 Gold Experience (REQ4)
```

---

## Communication Layers

### D1: 通信プロトコル一覧

| レイヤー | プロトコル | 方向 | 用途 | 関連要件 |
|---------|-----------|------|------|---------|
| MCP ↔ Process | Unison QUIC | Persistent | 高速コマンド送受信 | REQ2.4, REQ3.5, REQ5.3 |
| MCP ↔ Process | HTTP Fallback | On-demand | Ruby VM, permission, capture | REQ4.1, REQ4.2 |
| TUI ↔ Process | Unison QUIC | Persistent | Canvas コンテンツ購読 | REQ3.1, REQ3.2 |
| Client ↔ Process | HTTP REST | Request-Response | コントロール API | REQ6.2 |
| Client ↔ Process | WebSocket | Pub-Sub | Canvas 更新 | REQ3.1 |
| Process ↔ TheWorld | HTTP REST | Heartbeat | 自己登録・発見 | REQ6.1, REQ6.4 |

### D2: メッセージフロー（Canvas Show の例）

```
Claude Code (MCP Tool: show)
  │
  ▼ Unison QUIC "process" channel
Process Server (unison_server.rs)
  │
  ▼ AppState.hub.broadcast(ProcessMessage::Show)
Hub (broadcast::channel)
  │
  ▼ TopicRouter bridge
TopicRouter
  ├── Topic: "process/paisley-park/command/show/{pane_id}"
  ├── → RetainedStore に保存 (command カテゴリ)
  └── → 全 subscriber にブロードキャスト
  │
  ▼ Canvas (WebSocket subscriber)      ▼ TUI (QUIC subscriber)
  ブラウザ WebView に描画              CanvasState を更新 → ratatui 再描画
```

---

## Unison v2: TopicRouter

Hub（broadcast::channel）の上に乗る Topic ベースのメッセージルーティング層。

### D3: Topic 命名規則

```
{scope}/{capability}/{category}/{detail}
```

| セグメント | 値の例 | 説明 |
|-----------|--------|------|
| scope | `process` | 常に `process` |
| capability | `paisley-park`, `heavens-door`, `star-platinum`, `terminal`, `debug` | Stand 対応 |
| category | `command`, `state`, `event`, `data`, `log`, `trace` | メッセージ種別 |
| detail | pane_id, session_id 等 | メッセージ固有 |

### D4: Retained カテゴリ

| カテゴリ | 保持 | 用途 |
|---------|------|------|
| `state` | Yes | システム状態（TerminalReady, SessionList） |
| `command` | Yes | Canvas コマンド（show, clear, split） |
| `event` | No | 一過性イベント |

新規 subscriber は購読開始時に Retained メッセージを即座に受信する（MQTT v5 の Retained Message 相当）。

---

## Process Discovery

### D5: 発見フロー (REQ6.1, REQ6.4)

```
discovery::list()
  │
  ├── 1st: TheWorld API (port 32000) → /api/world/processes
  │        Success → ProcessInfo[] を返却
  │
  └── 2nd: HTTP スキャン (port 33000〜33010)
           各ポートの /api/health を順次チェック
           → terminal_token, project_dir を取得
```

**設計判断**: running.json（ファイルキャッシュ）を廃止し、TheWorld のインメモリ HashMap を唯一の真実源とする。ファイルは嘘をつく。

### D6: ProcessInfo

```rust
pub struct ProcessInfo {
    pub port: u16,
    pub pid: u32,
    pub project_dir: String,
    pub terminal_token: String,  // UUID v4 (REQ7 セキュリティ)
}
```

---

## Port Assignment (REQ6.3)

```
TheWorld:   32000 (HTTP + QUIC)
Process 0:  33000 (HTTP + QUIC)
Process 1:  33001 (HTTP + QUIC)
  ...
Process N:  33000+N (HTTP + QUIC)

上限: 33010（11 プロジェクト同時稼働）
QUIC オフセット: 0（TCP/UDP は同一ポート番号で共存可能）
```

---

## TUI Architecture (REQ1, REQ2)

### D7: 状態遷移

```
起動
  ▼
セッション選択画面 (REQ1.2)
  │ ├── 前回続行 (--continue)
  │ ├── 新規セッション
  │ └── 過去セッション一覧 (JSONL 解析)
  ▼
PTY ターミナル画面
  ├── Header: ⭐ プロジェクト名 | 🧭 Canvas 状態 | 📖 AI 状態
  ├── Body:   alacritty_terminal GridSnapshot → ratatui Widget
  └── Footer: アクションショートカット一覧
```

### D8: PTY 統合

| コンポーネント | 役割 |
|---------------|------|
| `PtyManager` | マルチセッション PTY 管理 (HashMap) |
| `alacritty_terminal` | VT パーサー → GridSnapshot |
| `TerminalView` | GridSnapshot → ratatui Buffer 描画 |
| `key_to_pty_bytes()` | crossterm KeyEvent → PTY バイト変換 |

AI 状態検出: PTY 出力アクティビティを監視（800ms 無出力 → 入力待ち）。

---

## HTTP API Routes

### D9: ルート一覧

**Canvas コントロール (REQ3)**:
- `POST /api/show` — コンテンツ表示
- `POST /api/toggle-pane` — サイドバー表示切替
- `POST /api/split-pane` — ペイン分割
- `POST /api/close-pane` — ペイン閉じる
- `POST /api/watch-file` / `unwatch-file` — ファイル監視
- `POST /api/canvas/open` / `close` / `capture` — Canvas ウィンドウ制御

**コード実行 (REQ4)**:
- `POST /api/ruby/eval` / `run` / `stop` — Ruby 実行
- `POST /api/process/run` / `eval` / `stop` / `inject` — ProcessRunner
- `GET /api/ruby/list`, `GET /api/process/list` — 一覧

**Permission / Prompt**:
- `POST /api/permission` — Claude CLI 権限リクエスト
- `POST /api/prompt` — ユーザープロンプトリクエスト

**TheWorld API (REQ6)**:
- `GET /api/world/processes` — プロセス一覧
- `POST /api/world/processes/{name}/start` / `stop` — ライフサイクル
- `POST /api/world/processes/register` / `unregister` — 自己登録

**システム**:
- `GET /api/health` — ヘルスチェック（terminal_token 返却）
- `POST /api/shutdown` — グレースフルシャットダウン

---

## MCP Server (REQ5.3)

`vp mcp` で stdio MCP サーバーとして起動。Claude Code のツールとして Process を操作する。

### D10: MCP ツール → 通信パス

| ツール | プロトコル | チャネル | 説明 |
|--------|-----------|---------|------|
| `show` | QUIC | process | Canvas にコンテンツ表示 |
| `clear` | QUIC | process | ペインクリア |
| `split_pane` | QUIC | process | ペイン分割 |
| `close_pane` | QUIC | process | ペイン閉じる |
| `toggle_pane` | QUIC | process | サイドバー切替 |
| `watch_file` | QUIC | process | ファイル監視開始 |
| `permission` | HTTP | fallback | 権限リクエスト |
| `restart` | HTTP | fallback | Process 再起動 |
| `eval_ruby` | HTTP | fallback | Ruby 評価 |
| `run_ruby` | HTTP | fallback | Ruby デーモン起動 |

QUIC 接続は lazy + 1 回リトライ。失敗時は HTTP フォールバック。タイムアウト 5 秒。

---

## Technology Stack

| レイヤー | 技術 | 用途 |
|---------|------|------|
| Runtime | Tokio | 非同期処理 |
| HTTP | Axum | REST API / WebSocket / 静的ファイル |
| QUIC | quinn + Unison | MCP / TUI 高速通信 |
| CLI | Clap | コマンドライン |
| WebView | wry + tao | ネイティブ Canvas (REQ3.2, REQ7.2) |
| Terminal | alacritty_terminal | VT パーサー |
| TUI | ratatui + crossterm | ターミナル UI |
| MIDI | midir | MIDI 入力 (REQ5.1) |
| Agent | Claude CLI (PTY) | AI 処理 (REQ2.1) |

---

## Network Binding

全サーバーは IPv6 loopback `[::1]` にバインド（localhost 限定）。

| プロトコル | バインドアドレス | 用途 |
|-----------|-----------------|------|
| HTTP (Axum) | `[::1]:{port}` | REST API / WebSocket / 静的ファイル |
| QUIC (Unison) | `[::1]:{port}` | MCP / TUI → Process 通信 |

---

## Key Design Decisions

| ID | 決定 | 理由 |
|----|------|------|
| D-01 | running.json 廃止、TheWorld インメモリ管理 | ファイルキャッシュは実態と乖離する（REQ6.4） |
| D-02 | QUIC primary + HTTP fallback の二重通信 | 速度と信頼性の両立 |
| D-03 | TopicRouter + RetainedStore | MQTT v5 相当の Topic ルーティング + 初回同期 |
| D-04 | QUIC ポートオフセット = 0 | TCP/UDP は同一ポートで共存可能、ポート消費を半減 |
| D-05 | terminal_token UUID v4 | 軽量認証でターミナルチャネルを保護 |
| D-06 | Hub buffer = 10000 | 高速 PTY 出力による ANSI シーケンス分断を防止 |
| D-07 | PTY 出力アクティビティで AI 状態検出 | DECTCEM は Claude CLI が常時隠すため使用不可 |

---

## Configuration

**場所**: `~/.config/vantage/config.toml`

```toml
default_port = 33000

[[projects]]
name = "vantage-point"
path = "/Users/makoto/repos/vantage-point"
```

---

## References

- `spec/01-core-concept.md` (VP-SPEC-001) — 要件定義
- `spec/02-capability.md` (VP-SPEC-002) — Capability / MIDI 仕様
- `crates/vantage-point/src/stands.rs` — Stand 命名定義
- `crates/vantage-point/src/discovery.rs` — プロセス発見
- `crates/vantage-point/src/process/topic_router.rs` — TopicRouter
- `crates/vantage-point/src/tui/app.rs` — TUI メインループ

---

*更新日: 2026-03-10*
