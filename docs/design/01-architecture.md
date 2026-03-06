# Vantage Point - アーキテクチャ設計

## 概要

Vantage PointはRust製のAI協働開発プラットフォーム。
Claude CLIをバックエンドとして、WebView UIとMIDI入力を統合した開発体験を提供する。

## システム構成

```
┌─────────────────────────────────────────────────────┐
│                    VP CLI (vp)                       │
│                    Rust Binary                       │
├─────────────────────────────────────────────────────┤
│                                                      │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │
│  │   Agent     │  │    MIDI     │  │   WebView   │ │
│  │  Service    │  │   Service   │  │   Server    │ │
│  │             │  │             │  │             │ │
│  │ Claude CLI  │  │   midir     │  │ Axum + wry  │ │
│  │ + MCP       │  │   LPD8等    │  │ WebSocket   │ │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘ │
│         │                │                │         │
│         └────────────────┼────────────────┘         │
│                          │                          │
│                   ┌──────┴──────┐                   │
│                   │  Session    │                   │
│                   │  Manager    │                   │
│                   └─────────────┘                   │
│                                                      │
└─────────────────────────────────────────────────────┘
```

## 命名体系

### Process と Stand

- **Process**: プロジェクトの開発プロセスを表す本体（HTTP + WebSocket サーバー）
- **Stand（能力）**: JoJo メタファーで、Process が保持する各 Capability の総称

```
Process（プロジェクトの開発プロセス = 本体）
  ├── Star Platinum（AI エージェント能力）
  ├── Paisley Park（Canvas 表示能力）
  ├── Heaven's Door（コード実行能力 / Ruby VM）
  └── Hermit Purple（外部コントロール能力 / MIDI 等）
```

- **TheWorld**: 常駐デーモン。全 Process のライフサイクルを管理

## サービス構成

### Agent Service

Claude CLIをプロセスとして実行し、JSON Streamで通信。

```
User Input (WebSocket)
    ↓
Agent Service
    ↓
Claude CLI (--output-format stream-json)
    ↓
Parse JSON Events
    ↓
WebSocket Broadcast
```

**機能**:
- Claude CLIの起動・管理
- MCPサーバー連携（`--mcp-config`）
- セッション管理（`--continue`, `--resume`）
- ストリーミングレスポンス解析

### MIDI Service

MIDIコントローラーからの入力をHTTP APIに変換。

```
MIDI Controller (AKAI LPD8等)
    ↓
midir (Rust MIDI Library)
    ↓
MidiEvent (Note/CC)
    ↓
HTTP POST → Agent Service
```

**マッピング例**:
- Pad 1 (Note 36): WebUI表示
- Pad 2 (Note 37): チャットキャンセル
- Pad 3 (Note 38): セッションリセット

### WebView Server

Axum HTTPサーバー + WebSocketハブ。

```
Browser/WebView
    ↓
HTTP Server (Axum)
    ├── GET /           → index.html
    ├── GET /api/health → Health Check
    └── WS  /ws         → Real-time Communication
```

**WebSocket Protocol**:
- `chat`: ユーザーメッセージ送信
- `chat_chunk`: AIレスポンス（ストリーミング）
- `cancel_chat`: 処理中断
- `list_sessions`, `switch_session`: セッション管理

## 技術スタック

| レイヤー | 技術 | 用途 |
|---------|------|------|
| Runtime | Tokio | 非同期処理 |
| HTTP | Axum | API / WebSocket |
| CLI | Clap | コマンドライン |
| WebView | wry + tao | ネイティブUI |
| MIDI | midir | MIDI入力 |
| Agent | Claude CLI | AI処理 |

## ポート管理

```
Project 0 → Port 33000
Project 1 → Port 33001
Project 2 → Port 33002
...
```

- `vp start 0` → 33000
- `vp start 1` → 33001
- `vp ps` → 33000-33010をスキャン

## 設定ファイル

**場所**: `~/.config/vantage/config.toml`

```toml
default_port = 33000

[[projects]]
name = "vantage-point"
path = "/Users/makoto/repos/vantage-point"

[[projects]]
name = "creo-memories"
path = "/Users/makoto/repos/creo-memories"
```

## プロセス構成

```
vp start 0
    │
    ├── HTTP Server (port 33000)
    ├── WebSocket Hub
    ├── Agent Process (claude CLI)
    └── [Optional] MIDI Listener
```

**システムトレイモード**:
```
vp tray
    │
    └── メニューバーアイコン
        ├── Instance 0 (33000) → Open / Stop
        ├── Instance 1 (33001) → Open / Stop
        ├── Refresh
        └── Quit
```

## 将来の拡張

| 機能 | 状態 |
|------|------|
| MCPサーバーモード | 実装済み (`vp mcp`) |
| セッション永続化 | 計画中 |
| P2P同期 (CRDT) | 将来 |
| Vision Pro対応 | 将来 |

---

*更新日: 2026-03-06*
*バージョン: 0.7.0*
