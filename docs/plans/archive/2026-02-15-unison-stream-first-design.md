# Unison Protocol Stream-First API設計

**日付**: 2026-02-15
**ステータス**: 承認済み
**creo-memories ID**: 019c5f2b-d32e

## 概要

Unison Protocolに双方向ストリーミングを追加する設計。
QUICストリームをAPIの唯一のプリミティブとし、HoL Blocking分析・データ特性・役割に基づいてStreamを分離する。

## コア哲学

> **Everything is a Stream.**
> RPC、Pub/Sub、Pushはすべて、Streamの属性パターンとして表現される。

## Stream分離の3つの根拠

### 1. HoL Blocking分析

```
❌ 単一Stream（WebSocket的）:
  query応答 500ms ──▶ イベント通知 ──▶ 緊急アラート
  queryが詰まると → イベントもアラートも待つ

✅ 独立Stream（QUIC）:
  Stream A: query ──■（遅延）
  Stream B: events ──▶        ← queryに影響されない
  Stream C: urgent ──▶        ← queryに影響されない
```

**原則**: 互いの遅延が影響してはいけないデータは、別Streamに分離する。

### 2. データ特性による分類

| データ | Drop可否 | 順序 | レイテンシ | 頻度 | → 配置先 |
|--------|---------|------|-----------|------|---------|
| 購読管理 | NG | 必要 | 低 | 低 | Control Plane |
| メモリイベント | NG | カテゴリ内で必要 | 中 | 高 | Data Plane |
| RPC応答 | NG | リクエスト内で必要 | 中 | 中 | Per-Request Stream |
| CC間メッセージ | NG | スレッド内で必要 | 中 | 低 | Messaging Plane |
| 緊急通知 | NG | 不要 | 最優先 | 低 | Urgent Plane |
| ハートビート | OK | 不要 | 低 | 固定 | QUIC Datagram |
| テレメトリ | OK | 不要 | 低 | 高 | QUIC Datagram |

### 3. 役割による分離

```
┌─── QUIC Connection ──────────────────────────────────────┐
│                                                           │
│  Control Plane ─── 購読管理（軽量・低頻度）                │
│      ↕ HoL分離                                           │
│  Data Plane ────── イベント配信（高頻度・Drop NG）          │
│      ↕ HoL分離                                           │
│  Query ─────────── RPC（重い処理・per-request分離）        │
│      ↕ HoL分離                                           │
│  Messaging ─────── CC間対話（独立会話・Drop NG）           │
│      ↕ HoL分離                                           │
│  Urgent ────────── 緊急通知（最優先・何にもブロックされない）│
│      ↕ HoL分離                                           │
│  Datagrams ─────── heartbeat/telemetry（Drop OK）         │
│                                                           │
└───────────────────────────────────────────────────────────┘
```

## QUICへのマッピング

```
QUIC Connection
│
├─ System Streams（変更なし）
│  ├─ Stream 1: Control / Handshake
│  ├─ Stream 2: Heartbeat
│  └─ Stream 3: Node Info
│
├─ Client-initiated Streams
│  ├─ [persistent] control   → 購読管理、認証
│  ├─ [transient]  query     → 単発RPC（per-request分離）
│  ├─ [persistent] messaging → CC間対話
│  └─ [persistent] subscribe → イベント購読条件送信
│
├─ Server-initiated Streams
│  ├─ [persistent] events    → イベント配信
│  ├─ [transient]  urgent    → 緊急通知
│  └─ [persistent] messaging → CC間対話（サーバーからも開ける）
│
└─ QUIC Datagrams（信頼性なし）
   ├─ heartbeat
   └─ telemetry
```

## KDLスキーマ仕様

### Channel定義

```
channel "name" from="client|server|either" lifetime="transient|persistent" {
    send "MessageType" { field "name" type="type" required=true|false }
    recv "MessageType" { field "name" type="type" }
    error "ErrorType" { field "name" type="type" }
}
```

| 属性 | 値 | 意味 |
|------|-----|------|
| `from` | `client` | クライアントがStreamを開く |
| | `server` | サーバーがStreamを開く |
| | `either` | どちらからでも開ける |
| `lifetime` | `transient` | メッセージ交換後に自動クローズ |
| | `persistent` | 明示的にクローズするまで継続 |

### パターンマトリクス

| パターン | from | lifetime | send/recv | 生成API |
|---------|------|----------|-----------|---------|
| RPC | client | transient | 1:1 | `async fn() -> Response` |
| Subscription | client | persistent | 1:N | `async fn() -> EventStream` |
| Push | server | transient | 1:0 | `async fn push(client)` |
| Channel | either | persistent | N:N | `async fn() -> (Tx, Rx)` |

## 具体例: creo-sync プロトコル

```kdl
protocol "creo-sync" version="1.0.0" {
    namespace "club.chronista.sync"

    // === Control Plane ===
    // 購読管理: 軽量・低頻度、他をブロックしてはいけない
    channel "control" from="client" lifetime="persistent" {
        send "Subscribe" {
            field "category" type="string"
            field "tags" type="array"
        }
        send "Unsubscribe" {
            field "channel_ref" type="string"
        }
        recv "Ack" {
            field "status" type="string"
            field "channel_ref" type="string"
        }
    }

    // === Data Plane ===
    // イベント配信: 高頻度、Drop NG、queryに巻き込まれてはいけない
    channel "events" from="server" lifetime="persistent" {
        send "MemoryEvent" {
            field "event_type" type="string"
            field "memory_id" type="string"
            field "category" type="string"
            field "from" type="string"
            field "timestamp" type="timestamp"
        }
    }

    // クエリ: 重い処理、per-requestで分離、eventsをブロックしない
    channel "query" from="client" lifetime="transient" {
        send "Query" {
            field "method" type="string"
            field "params" type="json"
        }
        recv "Result" {
            field "data" type="json"
        }
        error "QueryError" {
            field "code" type="string"
            field "message" type="string"
        }
    }

    // === Messaging Plane ===
    // CC間メッセージ: 独立した会話、データ配信と分離
    channel "messaging" from="either" lifetime="persistent" {
        send "CCMessage" {
            field "from" type="string"
            field "to" type="string"
            field "content" type="string"
            field "thread" type="string"
        }
        recv "CCMessage"
    }

    // === Urgent Plane ===
    // 緊急通知: 最優先、何があっても即座に届く、Drop NG
    channel "urgent" from="server" lifetime="transient" {
        send "Alert" {
            field "level" type="string"
            field "title" type="string"
            field "body" type="string"
        }
    }
}
```

## 生成されるRust API

```rust
// 全Channelを束ねた接続
struct CreoSyncConnection {
    control: BidirectionalChannel<Subscribe, Ack>,
    events: ReceiveChannel<MemoryEvent>,
    query: RequestChannel<Query, QueryResult>,
    messaging: BidirectionalChannel<CCMessage, CCMessage>,
    urgent: ReceiveChannel<Alert>,
}

// 全Channel並行処理（HoL blocking なし）
tokio::select! {
    event = conn.events.recv() => { /* イベント処理 */ }
    msg = conn.messaging.recv() => { /* メッセージ処理 */ }
    alert = conn.urgent.recv() => { /* 緊急通知処理 */ }
}
```

## エラーハンドリング

- QUICの`RESET_STREAM`にエラーコードをマッピング
- `error`ブロックでチャネルごとにエラー型を定義
- transientチャネルはエラー時に自動クローズ
- persistentチャネルはエラーをイベントとして通知、接続は維持

## 次のステップ

1. **Phase 1**: Unison Protocolに双方向ストリーミング実装（`channel`キーワード、KDLパーサー、コード生成）
2. **Phase 2**: creo-syncプロトコル定義（KDL）とテスト
3. **Phase 3**: VP StandにUnisonクライアント組み込み
4. **Phase 4**: creo-memoriesとの統合（WebSocket → Unison移行）
