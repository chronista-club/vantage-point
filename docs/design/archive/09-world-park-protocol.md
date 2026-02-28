# The World ↔ Paisley Park 通信プロトコル設計

> Unison Protocol を使用した The World と Paisley Park 間の通信設計

## 概要

The World（常駐コア）と Paisley Park（プロジェクトAgent）間の通信は、
Unison Protocol（QUIC + KDL）を使用して実現する。

### 通信特性

| 項目 | 仕様 |
|------|------|
| トランスポート | QUIC (HTTP/3) |
| スキーマ | KDL (KDL Document Language) |
| シリアライズ | rkyv (ゼロコピー) |
| 圧縮 | zstd Level 1 (2KB以上) |
| 暗号化 | TLS 1.3 |
| ネットワーク | IPv6 (localhost) |

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────┐
│                      The World                           │
│                    [::1]:33000                           │
│  ┌─────────────────────────────────────────────────┐    │
│  │            Unison Protocol Server                │    │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐            │    │
│  │  │Conductor│ │  Event  │ │  State  │            │    │
│  │  │ Service │ │ Service │ │ Service │            │    │
│  │  └─────────┘ └─────────┘ └─────────┘            │    │
│  └─────────────────────────────────────────────────┘    │
│         ▲                    │                           │
│         │ QUIC               │ QUIC                      │
│         │                    ▼                           │
│  ┌──────┴─────────────────────────────────────────┐     │
│  │                                                 │     │
│  │  Paisley Park A    Paisley Park B    ...       │     │
│  │  [::1]:dynamic     [::1]:dynamic               │     │
│  │                                                 │     │
│  └─────────────────────────────────────────────────┘     │
└─────────────────────────────────────────────────────────┘
```

## KDL スキーマ定義

### vantage-point.kdl

```kdl
protocol "vantage-point" version="1.0.0" {
    namespace "dev.vantagepoint.protocol"

    // ==========================================================
    // Conductor Service: Paisley Park ライフサイクル管理
    // ==========================================================
    service "ConductorService" {
        // Paisley Park 登録
        method "register" {
            request {
                field "project_id" type="string" required=true
                field "project_path" type="string" required=true
                field "port" type="u16" required=true
            }
            response {
                field "park_id" type="string" required=true
                field "session_token" type="string" required=true
            }
        }

        // Paisley Park ハートビート
        method "heartbeat" {
            request {
                field "park_id" type="string" required=true
                field "status" type="ParkStatus" required=true
            }
            response {
                field "ack" type="bool" required=true
                field "commands" type="list<Command>" optional=true
            }
        }

        // Paisley Park 終了通知
        method "unregister" {
            request {
                field "park_id" type="string" required=true
                field "reason" type="string" optional=true
            }
            response {
                field "ack" type="bool" required=true
            }
        }
    }

    // ==========================================================
    // Event Service: イベント配信
    // ==========================================================
    service "EventService" {
        // イベント購読（双方向ストリーム）
        stream "subscribe" {
            request {
                field "park_id" type="string" required=true
                field "event_types" type="list<string>" required=true
            }
            response {
                field "event" type="Event" required=true
            }
        }

        // イベント発行
        method "publish" {
            request {
                field "park_id" type="string" required=true
                field "event" type="Event" required=true
            }
            response {
                field "event_id" type="string" required=true
            }
        }
    }

    // ==========================================================
    // View Service: View操作
    // ==========================================================
    service "ViewService" {
        // コンテンツ表示
        method "show" {
            request {
                field "pane_id" type="string" required=true
                field "content" type="Content" required=true
                field "append" type="bool" default=false
            }
            response {
                field "success" type="bool" required=true
            }
        }

        // ペインクリア
        method "clear" {
            request {
                field "pane_id" type="string" required=true
            }
            response {
                field "success" type="bool" required=true
            }
        }

        // ペイントグル
        method "toggle_pane" {
            request {
                field "pane_id" type="string" required=true
                field "visible" type="bool" optional=true
            }
            response {
                field "visible" type="bool" required=true
            }
        }

        // チャットメッセージ送信
        method "chat" {
            request {
                field "message" type="ChatMessage" required=true
            }
            response {
                field "message_id" type="string" required=true
            }
        }

        // チャットストリーム（双方向）
        stream "chat_stream" {
            request {
                field "park_id" type="string" required=true
            }
            response {
                field "chunk" type="ChatChunk" required=true
            }
        }
    }

    // ==========================================================
    // Terminal Service: Terminal管理
    // ==========================================================
    service "TerminalService" {
        // Terminal生成
        method "create" {
            request {
                field "park_id" type="string" required=true
                field "name" type="string" optional=true
            }
            response {
                field "terminal_id" type="string" required=true
            }
        }

        // コマンド実行
        method "exec" {
            request {
                field "terminal_id" type="string" required=true
                field "command" type="string" required=true
            }
            response {
                field "exit_code" type="i32" required=true
                field "stdout" type="string" required=true
                field "stderr" type="string" required=true
            }
        }

        // Terminal出力ストリーム
        stream "output" {
            request {
                field "terminal_id" type="string" required=true
            }
            response {
                field "data" type="bytes" required=true
                field "stream" type="StreamType" required=true
            }
        }

        // Terminal破棄
        method "destroy" {
            request {
                field "terminal_id" type="string" required=true
            }
            response {
                field "success" type="bool" required=true
            }
        }
    }

    // ==========================================================
    // 型定義
    // ==========================================================

    enum "ParkStatus" {
        value "idle"
        value "busy"
        value "error"
    }

    enum "StreamType" {
        value "stdout"
        value "stderr"
    }

    struct "Command" {
        field "type" type="string" required=true
        field "payload" type="json" optional=true
    }

    struct "Event" {
        field "id" type="string" required=true
        field "type" type="string" required=true
        field "timestamp" type="u64" required=true
        field "source" type="string" required=true
        field "payload" type="json" required=true
    }

    struct "Content" {
        field "type" type="ContentType" required=true
        field "data" type="string" required=true
        field "mime_type" type="string" optional=true
    }

    enum "ContentType" {
        value "log"
        value "markdown"
        value "html"
        value "image_base64"
    }

    struct "ChatMessage" {
        field "role" type="ChatRole" required=true
        field "content" type="string" required=true
    }

    enum "ChatRole" {
        value "user"
        value "assistant"
        value "system"
    }

    struct "ChatChunk" {
        field "content" type="string" required=true
        field "done" type="bool" required=true
    }
}
```

## 通信フロー

### 1. Paisley Park 起動フロー

```
Paisley Park                    The World
    │                               │
    │ ──── register ─────────────→  │
    │      {project_id, port}       │
    │                               │
    │ ←──── {park_id, token} ─────  │
    │                               │
    │ ──── subscribe(events) ────→  │
    │      (bidirectional stream)   │
    │                               │
    │ ←──── event stream ─────────  │
    │                               │
```

### 2. ハートビート

```
Paisley Park                    The World
    │                               │
    │ ──── heartbeat ────────────→  │
    │      {park_id, status}        │
    │                               │  (毎10秒)
    │ ←──── {ack, commands} ──────  │
    │                               │
```

### 3. View操作

```
Paisley Park                    The World                Browser
    │                               │                        │
    │ ──── show ─────────────────→  │                        │
    │      {pane_id, content}       │                        │
    │                               │ ──── WebSocket ─────→  │
    │ ←──── {success} ────────────  │                        │
    │                               │                        │
```

### 4. MIDIイベント

```
MIDI Controller                 The World              Paisley Park
    │                               │                        │
    │ ──── MIDI Note ────────────→  │                        │
    │                               │ ──── event stream ──→  │
    │                               │      {type: "midi"}    │
    │                               │                        │
    │                               │ ←──── show ──────────  │
    │                               │      (View更新)        │
    │                               │                        │
```

## エラーハンドリング

### 接続断絶時

1. Paisley Park は自動再接続を試行（最大3回、指数バックオフ）
2. 再接続成功時は `register` から再実行
3. 3回失敗でローカルログに記録し、手動復旧待ち

### The World 再起動時

1. The World は起動時に `restart` イベントをブロードキャスト
2. 既存 Paisley Park は再登録
3. View は WebSocket 再接続後に状態復元

## セキュリティ

### ローカル通信のみ

- The World / Paisley Park 間は `[::1]` (localhost) 限定
- 外部ネットワークからのアクセス不可

### セッショントークン

- `register` 時に発行される `session_token` で認証
- トークンは The World 再起動で無効化

## 将来拡張

### Multiplexer 対応

```kdl
service "MultiplexerService" {
    method "create_group" { ... }
    method "dispatch_task" { ... }
    stream "progress" { ... }
}
```

### リモート Paisley Park

将来的にはリモートマシン上の Paisley Park との通信も検討:
- 証明書ベース認証
- mTLS
- ゼロトラストネットワーク

## 関連ドキュメント

- [spec/07-the-world.md](../spec/07-the-world.md)
- [spec/08-paisley-park.md](../spec/08-paisley-park.md)
- [Unison Protocol](https://github.com/chronista-club/unison)
