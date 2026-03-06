# Agent Protocol Unification Design

> AG-UI + ACP準拠 + 独自拡張の統一プロトコル設計

## 概要

Vantage PointはAI Agent通信の2つの主要プロトコルに準拠しつつ、独自の拡張を可能にする。

### プロトコル比較

| 観点 | AG-UI | ACP | Vantage Point |
|------|-------|-----|---------------|
| **設計思想** | UI更新に特化 | Editor統合に特化 | 両方 + MIDI/マルチモーダル |
| **メッセージ形式** | Tagged enum | JSON-RPC 2.0 | 両方をサポート |
| **トランスポート** | WebSocket | stdio (+ HTTP Draft) | WebSocket + stdio |
| **ツール実行** | Start/End | pending→in_progress→completed | 両方 + カスタムステータス |
| **Permission** | approve/deny | allow_once/always, reject_once/always | 拡張オプション |

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│                    Vantage Point Protocol Layer             │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   AG-UI     │  │    ACP      │  │  Vantage Extension  │ │
│  │   Events    │  │  JSON-RPC   │  │  (MIDI, Custom)     │ │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘ │
│         │                │                     │            │
│         └────────────────┼─────────────────────┘            │
│                          ▼                                   │
│              ┌───────────────────────┐                      │
│              │   Protocol Router     │                      │
│              │   (CapabilityEvent)   │                      │
│              └───────────┬───────────┘                      │
│                          │                                   │
├──────────────────────────┼──────────────────────────────────┤
│                          ▼                                   │
│              ┌───────────────────────┐                      │
│              │      EventBus         │                      │
│              │  (REQ-CAP-003)        │                      │
│              └───────────────────────┘                      │
└─────────────────────────────────────────────────────────────┘
```

## メッセージ構造

### 統一イベント型

```rust
/// 統一プロトコルメッセージ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol")]
pub enum ProtocolMessage {
    /// AG-UI形式のイベント
    AgUi(AgUiEvent),

    /// ACP形式のJSON-RPCメッセージ
    Acp(AcpMessage),

    /// Vantage Point独自拡張
    Vantage(VantageEvent),
}

/// ACP JSON-RPC メッセージ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AcpMessage {
    Request(AcpRequest),
    Response(AcpResponse),
    Notification(AcpNotification),
}

/// Vantage Point 独自イベント
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VantageEvent {
    /// MIDI入力イベント
    MidiInput {
        channel: u8,
        event_type: MidiEventType,
        data: Vec<u8>,
    },

    /// Capability状態変更
    CapabilityStateChanged {
        capability_id: String,
        state: CapabilityState,
    },

    /// Synergy発動
    SynergyActivated {
        synergy_id: String,
        capabilities: Vec<String>,
    },
}
```

### ACP準拠メソッド

```rust
/// ACP Methods (Request-Response)
pub enum AcpMethod {
    // エージェント → クライアント
    Initialize,           // バージョン/capability交換
    Authenticate,         // 認証（オプション）
    SessionNew,           // 新規セッション作成
    SessionLoad,          // 既存セッション読み込み
    SessionPrompt,        // プロンプト送信
    SessionCancel,        // キャンセル
    SessionRequestPermission, // 権限要求

    // クライアント → エージェント
    FileRead,
    FileWrite,
    TerminalExecute,
    // ...
}

/// ACP Notifications (One-way)
pub enum AcpNotification {
    SessionUpdate,        // セッション状態更新
    ToolCallUpdate,       // ツール実行状態更新
}
```

### ツール実行ステータス（ACP準拠）

```rust
/// ACP Tool Call Status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,      // 実行待ち or 承認待ち
    InProgress,   // 実行中
    Completed,    // 完了
    Failed,       // 失敗
}

/// Tool Call Kind (ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}
```

### Permission オプション（ACP準拠 + 拡張）

```rust
/// Permission Option (ACP + Vantage Extension)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    // ACP標準
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,

    // Vantage拡張
    AllowWithEdit,     // 編集して許可
    Delegate,          // 自動判断に委任
    RequireConfirm,    // 毎回確認
}
```

### Stop Reason（ACP準拠）

```rust
/// ACP Stop Reason
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,           // 正常終了
    MaxTokens,         // トークン上限
    MaxTurnRequests,   // リクエスト上限
    Refusal,           // 拒否
    Cancelled,         // キャンセル
}
```

## 実装計画

### Phase 1: プロトコル型定義

1. `protocol/mod.rs` - 統一プロトコルモジュール
2. `protocol/agui.rs` - AG-UI型（既存を移動・拡張）
3. `protocol/acp.rs` - ACP型定義
4. `protocol/vantage.rs` - 独自拡張型

### Phase 2: Protocol Capability

1. `ProtocolCapability` - Capabilityトレイト実装
2. EventBusとの連携
3. AG-UI ↔ ACP 変換ロジック

### Phase 3: Transport統合

1. WebSocket（既存）にACP対応追加
2. stdio transport追加（MCPモード用）

## 要件ID

- REQ-PROTO-001: AG-UI準拠
- REQ-PROTO-002: ACP準拠
- REQ-PROTO-003: Vantage拡張
- REQ-PROTO-004: EventBus連携
- REQ-PROTO-005: Transport抽象化

## 関連ドキュメント

- [AG-UI Protocol](https://docs.ag-ui.com)
- [Agent Client Protocol](https://agentclientprotocol.com)
- [05-capability.md](../spec/05-capability.md)
