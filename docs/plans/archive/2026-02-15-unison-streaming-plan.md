# Unison Protocol Stream-First 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Unison Protocolに双方向ストリーミングとIdentity Channelを追加し、`channel`キーワードによるStream-First APIを実現する

**Architecture:** KDLパーサーに`channel`キーワードを追加、コード生成でChannel型を出力、QUICネットワーク層にpersistent stream管理とIdentity Channelを実装する。既存のRPC（`method`）は引き続きサポート。

**Tech Stack:** Rust, quinn (QUIC), knuffel (KDL), tokio, rkyv

---

### Task 1: KDLパーサーに `channel` キーワードを追加

**Files:**
- Modify: `crates/unison-protocol/src/parser/schema.rs`
- Test: `crates/unison-protocol/tests/test_kdl.rs`

**Step 1: テスト用KDLスキーマを書く**

`crates/unison-protocol/tests/test_kdl.rs` に追加:

```rust
#[test]
fn test_channel_parsing() {
    let schema = r#"
        protocol "test-streaming" version="1.0.0" {
            namespace "test.streaming"

            channel "events" from="server" lifetime="persistent" {
                send "Event" {
                    field "event_type" type="string" required=true
                    field "payload" type="json"
                }
            }

            channel "control" from="client" lifetime="persistent" {
                send "Subscribe" {
                    field "category" type="string"
                }
                recv "Ack" {
                    field "status" type="string"
                }
            }

            channel "query" from="client" lifetime="transient" {
                send "Request" {
                    field "method" type="string" required=true
                    field "params" type="json"
                }
                recv "Response" {
                    field "data" type="json"
                }
                error "QueryError" {
                    field "code" type="string"
                    field "message" type="string"
                }
            }

            channel "chat" from="either" lifetime="persistent" {
                send "Message" {
                    field "text" type="string" required=true
                    field "from" type="string"
                }
                recv "Message"
            }
        }
    "#;

    let parser = SchemaParser::new();
    let result = parser.parse(schema).unwrap();
    let protocol = &result.protocols[0];

    // channelが4つパースされること
    assert_eq!(protocol.channels.len(), 4);

    // events channel
    let events = &protocol.channels[0];
    assert_eq!(events.name, "events");
    assert_eq!(events.from, ChannelFrom::Server);
    assert_eq!(events.lifetime, ChannelLifetime::Persistent);
    assert!(events.send.is_some());
    assert!(events.recv.is_none());

    // control channel
    let control = &protocol.channels[1];
    assert_eq!(control.from, ChannelFrom::Client);
    assert!(control.send.is_some());
    assert!(control.recv.is_some());

    // query channel - with error
    let query = &protocol.channels[2];
    assert_eq!(query.lifetime, ChannelLifetime::Transient);
    assert!(query.error.is_some());

    // chat channel
    let chat = &protocol.channels[3];
    assert_eq!(chat.from, ChannelFrom::Either);
}
```

**Step 2: テスト実行 → 失敗を確認**

```bash
cd /Users/makoto/repos/unison
cargo test test_channel_parsing -- --nocapture
```
Expected: コンパイルエラー（`Channel`, `ChannelFrom`, `ChannelLifetime` が未定義）

**Step 3: `schema.rs` に Channel 型を追加**

`crates/unison-protocol/src/parser/schema.rs` に以下を追加:

```rust
/// Channel開始者
#[derive(Debug, Clone, PartialEq, knuffel::DecodeScalar)]
pub enum ChannelFrom {
    #[knuffel(rename = "client")]
    Client,
    #[knuffel(rename = "server")]
    Server,
    #[knuffel(rename = "either")]
    Either,
}

/// Channelの寿命
#[derive(Debug, Clone, PartialEq, knuffel::DecodeScalar)]
pub enum ChannelLifetime {
    #[knuffel(rename = "transient")]
    Transient,
    #[knuffel(rename = "persistent")]
    Persistent,
}

/// Channel定義（Stream-First APIのプリミティブ）
#[derive(Debug, Clone, knuffel::Decode)]
pub struct Channel {
    /// チャネル名
    #[knuffel(argument)]
    pub name: String,

    /// 誰がStreamを開くか
    #[knuffel(property)]
    pub from: ChannelFrom,

    /// Streamの寿命
    #[knuffel(property)]
    pub lifetime: ChannelLifetime,

    /// 送信メッセージ型（opener視点）
    #[knuffel(children(name = "send"))]
    pub send: Option<MethodMessage>,

    /// 受信メッセージ型（opener視点）
    #[knuffel(children(name = "recv"))]
    pub recv: Option<MethodMessage>,

    /// エラー型
    #[knuffel(children(name = "error"))]
    pub error: Option<MethodMessage>,
}
```

`Protocol` structに `channels` フィールドを追加:

```rust
// Protocol struct内（既存のservicesの後に追加）
#[knuffel(children(name = "channel"))]
pub channels: Vec<Channel>,
```

**Step 4: テスト実行 → パスを確認**

```bash
cargo test test_channel_parsing -- --nocapture
```
Expected: PASS

**Step 5: コミット**

```bash
git add crates/unison-protocol/src/parser/schema.rs crates/unison-protocol/tests/test_kdl.rs
git commit -m "feat: KDLパーサーにchannelキーワードを追加"
```

---

### Task 2: Identity Channel のメッセージ型を定義

**Files:**
- Create: `crates/unison-protocol/src/network/identity.rs`
- Modify: `crates/unison-protocol/src/network/mod.rs`
- Test: `crates/unison-protocol/tests/test_identity.rs`

**Step 1: テストを書く**

`crates/unison-protocol/tests/test_identity.rs`:

```rust
use unison_protocol::network::identity::*;

#[test]
fn test_identity_serialization() {
    let identity = ServerIdentity {
        name: "creo-memories".to_string(),
        version: "1.0.0".to_string(),
        namespace: "club.chronista.sync".to_string(),
        channels: vec![
            ChannelInfo {
                name: "events".to_string(),
                direction: ChannelDirection::ServerToClient,
                lifetime: "persistent".to_string(),
                status: ChannelStatus::Available,
            },
            ChannelInfo {
                name: "query".to_string(),
                direction: ChannelDirection::Bidirectional,
                lifetime: "transient".to_string(),
                status: ChannelStatus::Available,
            },
        ],
        metadata: serde_json::json!({
            "project": "creo-memories",
            "role": "memory-store"
        }),
    };

    // JSON往復
    let json = serde_json::to_string(&identity).unwrap();
    let deserialized: ServerIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.name, "creo-memories");
    assert_eq!(deserialized.channels.len(), 2);
    assert_eq!(deserialized.channels[0].status, ChannelStatus::Available);
}

#[test]
fn test_channel_update() {
    let update = ChannelUpdate::Added(ChannelInfo {
        name: "alerts".to_string(),
        direction: ChannelDirection::ServerToClient,
        lifetime: "transient".to_string(),
        status: ChannelStatus::Available,
    });

    let json = serde_json::to_string(&update).unwrap();
    let deserialized: ChannelUpdate = serde_json::from_str(&json).unwrap();
    match deserialized {
        ChannelUpdate::Added(info) => assert_eq!(info.name, "alerts"),
        _ => panic!("Expected Added variant"),
    }
}
```

**Step 2: テスト実行 → 失敗確認**

```bash
cargo test test_identity -- --nocapture
```
Expected: コンパイルエラー

**Step 3: Identity型を実装**

`crates/unison-protocol/src/network/identity.rs`:

```rust
//! Identity Channel: QUICエンドポイントのリアルタイム自己紹介
//!
//! 各サーバーは接続時にServerIdentityを送信し、
//! チャネルの追加・削除・状態変更をリアルタイムに通知する。

use serde::{Deserialize, Serialize};

/// サーバーの自己紹介情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerIdentity {
    /// サーバー名
    pub name: String,
    /// プロトコルバージョン
    pub version: String,
    /// 名前空間
    pub namespace: String,
    /// 利用可能なチャネル一覧
    pub channels: Vec<ChannelInfo>,
    /// 任意のメタデータ
    pub metadata: serde_json::Value,
}

/// チャネルの情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// チャネル名
    pub name: String,
    /// データの流れる方向
    pub direction: ChannelDirection,
    /// 寿命（transient / persistent）
    pub lifetime: String,
    /// 現在の状態
    pub status: ChannelStatus,
}

/// チャネルの方向
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelDirection {
    ServerToClient,
    ClientToServer,
    Bidirectional,
}

/// チャネルの状態
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    Available,
    Busy,
    Unavailable,
}

/// チャネルのリアルタイム更新
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "channel")]
pub enum ChannelUpdate {
    /// 新しいチャネルが追加された
    Added(ChannelInfo),
    /// チャネルが削除された
    Removed(String),
    /// チャネルの状態が変わった
    StatusChanged { name: String, status: ChannelStatus },
}
```

`crates/unison-protocol/src/network/mod.rs` に追加:

```rust
pub mod identity;
```

**Step 4: テスト実行 → パス確認**

```bash
cargo test test_identity -- --nocapture
```
Expected: PASS

**Step 5: コミット**

```bash
git add crates/unison-protocol/src/network/identity.rs crates/unison-protocol/src/network/mod.rs crates/unison-protocol/tests/test_identity.rs
git commit -m "feat: Identity Channel のメッセージ型を追加"
```

---

### Task 3: Persistent Stream 管理を実装

**Files:**
- Create: `crates/unison-protocol/src/network/channel.rs`
- Modify: `crates/unison-protocol/src/network/mod.rs`
- Test: `crates/unison-protocol/tests/test_channel_stream.rs`

**Step 1: テストを書く**

`crates/unison-protocol/tests/test_channel_stream.rs`:

```rust
use tokio::sync::mpsc;

#[tokio::test]
async fn test_stream_sender_receiver() {
    let (tx, mut rx) = mpsc::channel::<String>(32);

    // StreamSenderラップ
    let sender = unison_protocol::network::channel::StreamSender::new(tx);
    sender.send("hello".to_string()).await.unwrap();
    sender.send("world".to_string()).await.unwrap();

    // StreamReceiverラップ
    let msg1 = rx.recv().await.unwrap();
    assert_eq!(msg1, "hello");
    let msg2 = rx.recv().await.unwrap();
    assert_eq!(msg2, "world");
}

#[tokio::test]
async fn test_bidirectional_channel() {
    use unison_protocol::network::channel::BidirectionalChannel;

    let (client_tx, server_rx) = mpsc::channel::<String>(32);
    let (server_tx, client_rx) = mpsc::channel::<String>(32);

    let client = BidirectionalChannel {
        sender: unison_protocol::network::channel::StreamSender::new(client_tx),
        receiver: unison_protocol::network::channel::StreamReceiver::new(client_rx),
    };

    let server = BidirectionalChannel {
        sender: unison_protocol::network::channel::StreamSender::new(server_tx),
        receiver: unison_protocol::network::channel::StreamReceiver::new(server_rx),
    };

    // クライアント → サーバー
    client.sender.send("ping".to_string()).await.unwrap();
    let msg = server.receiver.recv().await.unwrap();
    assert_eq!(msg, Some("ping".to_string()));

    // サーバー → クライアント
    server.sender.send("pong".to_string()).await.unwrap();
    let msg = client.receiver.recv().await.unwrap();
    assert_eq!(msg, Some("pong".to_string()));
}
```

**Step 2: テスト実行 → 失敗確認**

```bash
cargo test test_stream_sender_receiver test_bidirectional_channel -- --nocapture
```

**Step 3: Channel型を実装**

`crates/unison-protocol/src/network/channel.rs`:

```rust
//! Channel: Stream-First APIの通信プリミティブ
//!
//! 各ChannelはQUICストリームにマッピングされ、
//! 独立したHoL Blocking境界を形成する。

use tokio::sync::mpsc;

/// 送信側ハンドル
pub struct StreamSender<T> {
    tx: mpsc::Sender<T>,
}

impl<T> StreamSender<T> {
    pub fn new(tx: mpsc::Sender<T>) -> Self {
        Self { tx }
    }

    pub async fn send(&self, msg: T) -> Result<(), mpsc::error::SendError<T>> {
        self.tx.send(msg).await
    }

    /// チャネルが閉じているか
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// 受信側ハンドル
pub struct StreamReceiver<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> StreamReceiver<T> {
    pub fn new(rx: mpsc::Receiver<T>) -> Self {
        Self { rx }
    }

    pub async fn recv(&mut self) -> Option<T> {
        self.rx.recv().await
    }
}

/// 双方向チャネル
pub struct BidirectionalChannel<S, R> {
    pub sender: StreamSender<S>,
    pub receiver: StreamReceiver<R>,
}

/// 受信専用チャネル（Push/Event用）
pub struct ReceiveChannel<T> {
    pub receiver: StreamReceiver<T>,
}

/// リクエスト-レスポンスチャネル（transient RPC用）
pub struct RequestChannel<Req, Res> {
    _req: std::marker::PhantomData<Req>,
    _res: std::marker::PhantomData<Res>,
    tx: mpsc::Sender<(Req, tokio::sync::oneshot::Sender<Res>)>,
}
```

`crates/unison-protocol/src/network/mod.rs` に追加:

```rust
pub mod channel;
```

**Step 4: テスト実行 → パス確認**

```bash
cargo test test_stream_sender test_bidirectional -- --nocapture
```

**Step 5: コミット**

```bash
git add crates/unison-protocol/src/network/channel.rs crates/unison-protocol/src/network/mod.rs crates/unison-protocol/tests/test_channel_stream.rs
git commit -m "feat: Channel型（StreamSender/Receiver/Bidirectional）を追加"
```

---

### Task 4: コード生成にChannel対応を追加

**Files:**
- Modify: `crates/unison-protocol/src/codegen/rust.rs`
- Test: `crates/unison-protocol/tests/test_codegen_channel.rs`

**Step 1: テストを書く**

`crates/unison-protocol/tests/test_codegen_channel.rs`:

```rust
use unison_protocol::codegen::CodeGenerator;
use unison_protocol::codegen::rust::RustGenerator;
use unison_protocol::parser::SchemaParser;

#[test]
fn test_channel_codegen() {
    let schema = r#"
        protocol "test-sync" version="1.0.0" {
            namespace "test.sync"

            channel "events" from="server" lifetime="persistent" {
                send "Event" {
                    field "event_type" type="string" required=true
                    field "data" type="json"
                }
            }

            channel "query" from="client" lifetime="transient" {
                send "QueryRequest" {
                    field "method" type="string" required=true
                }
                recv "QueryResponse" {
                    field "result" type="json"
                }
            }
        }
    "#;

    let parser = SchemaParser::new();
    let parsed = parser.parse(schema).unwrap();
    let generator = RustGenerator::new();
    let code = generator.generate(&parsed).unwrap();

    // メッセージ構造体が生成されること
    assert!(code.contains("pub struct Event"));
    assert!(code.contains("pub event_type: String"));
    assert!(code.contains("pub struct QueryRequest"));
    assert!(code.contains("pub struct QueryResponse"));

    // Connection型にchannelフィールドが生成されること
    assert!(code.contains("events"));
    assert!(code.contains("query"));
    assert!(code.contains("ReceiveChannel"));
    assert!(code.contains("RequestChannel") || code.contains("BidirectionalChannel"));
}
```

**Step 2: テスト実行 → 失敗確認**

```bash
cargo test test_channel_codegen -- --nocapture
```

**Step 3: `rust.rs`にChannel生成を追加**

`crates/unison-protocol/src/codegen/rust.rs` の `generate()` メソッドに追加:

```rust
// generate() 内、既存のgenerate_service()の後に追加
for protocol in &schema.protocols {
    for channel in &protocol.channels {
        output.push_str(&self.generate_channel_messages(channel)?);
    }
    if !protocol.channels.is_empty() {
        output.push_str(&self.generate_connection_struct(protocol)?);
    }
}
```

新メソッド追加:

```rust
/// Channelのメッセージ型を生成
fn generate_channel_messages(&self, channel: &Channel) -> Result<String> {
    let mut output = String::new();
    if let Some(ref send) = channel.send {
        output.push_str(&self.generate_message_struct(send)?);
    }
    if let Some(ref recv) = channel.recv {
        output.push_str(&self.generate_message_struct(recv)?);
    }
    if let Some(ref error) = channel.error {
        output.push_str(&self.generate_message_struct(error)?);
    }
    Ok(output)
}

/// Connection構造体を生成（全Channelを束ねる）
fn generate_connection_struct(&self, protocol: &Protocol) -> Result<String> {
    let mut fields = String::new();
    for channel in &protocol.channels {
        let field_name = channel.name.to_case(Case::Snake);
        let field_type = self.channel_field_type(channel);
        fields.push_str(&format!("    pub {}: {},\n", field_name, field_type));
    }
    let name = format!("{}Connection", protocol.name.to_case(Case::Pascal));
    Ok(format!("pub struct {} {{\n{}}}\n\n", name, fields))
}

/// Channel定義からフィールド型を決定
fn channel_field_type(&self, channel: &Channel) -> String {
    let has_send = channel.send.is_some();
    let has_recv = channel.recv.is_some();
    match (has_send, has_recv, &channel.lifetime) {
        // server→client persistent: 受信専用
        (true, false, _) if channel.from == ChannelFrom::Server => {
            let send_type = channel.send.as_ref().unwrap().name.to_case(Case::Pascal);
            format!("ReceiveChannel<{}>", send_type)
        }
        // client→server transient: リクエスト-レスポンス
        (true, true, ChannelLifetime::Transient) => {
            let send_type = channel.send.as_ref().unwrap().name.to_case(Case::Pascal);
            let recv_type = channel.recv.as_ref().unwrap().name.to_case(Case::Pascal);
            format!("RequestChannel<{}, {}>", send_type, recv_type)
        }
        // 双方向
        (true, true, ChannelLifetime::Persistent) => {
            let send_type = channel.send.as_ref().unwrap().name.to_case(Case::Pascal);
            let recv_type = channel.recv.as_ref().unwrap().name.to_case(Case::Pascal);
            format!("BidirectionalChannel<{}, {}>", send_type, recv_type)
        }
        _ => "()".to_string(),
    }
}
```

**Step 4: テスト実行 → パス確認**

```bash
cargo test test_channel_codegen -- --nocapture
```

**Step 5: コミット**

```bash
git add crates/unison-protocol/src/codegen/rust.rs crates/unison-protocol/tests/test_codegen_channel.rs
git commit -m "feat: コード生成にChannel対応を追加"
```

---

### Task 5: Identity Channel をQUIC層に組み込む

**Files:**
- Modify: `crates/unison-protocol/src/network/quic.rs`
- Modify: `crates/unison-protocol/src/network/identity.rs`
- Test: `crates/unison-protocol/tests/test_identity_quic.rs`

**Step 1: テストを書く**

`crates/unison-protocol/tests/test_identity_quic.rs`:

```rust
use unison_protocol::network::identity::*;

#[tokio::test]
async fn test_identity_channel_flow() {
    // サーバーがIdentityを構築
    let identity = ServerIdentity::new("test-server", "1.0.0", "test.ns");
    assert_eq!(identity.name, "test-server");
    assert!(identity.channels.is_empty());

    // チャネルを追加
    let mut identity = identity;
    identity.add_channel(ChannelInfo {
        name: "events".to_string(),
        direction: ChannelDirection::ServerToClient,
        lifetime: "persistent".to_string(),
        status: ChannelStatus::Available,
    });
    assert_eq!(identity.channels.len(), 1);

    // ProtocolMessageに変換
    let msg = identity.to_protocol_message();
    assert_eq!(msg.method, "__identity");

    // ProtocolMessageから復元
    let restored = ServerIdentity::from_protocol_message(&msg).unwrap();
    assert_eq!(restored.name, "test-server");
    assert_eq!(restored.channels.len(), 1);
}
```

**Step 2: テスト実行 → 失敗確認**

```bash
cargo test test_identity_channel_flow -- --nocapture
```

**Step 3: Identity型にヘルパーメソッドを追加**

`crates/unison-protocol/src/network/identity.rs` に追加:

```rust
impl ServerIdentity {
    /// 新しいIdentityを作成
    pub fn new(name: &str, version: &str, namespace: &str) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
            namespace: namespace.to_string(),
            channels: Vec::new(),
            metadata: serde_json::Value::Null,
        }
    }

    /// チャネル情報を追加
    pub fn add_channel(&mut self, channel: ChannelInfo) {
        self.channels.push(channel);
    }

    /// ProtocolMessageに変換（System Stream 3で送信用）
    pub fn to_protocol_message(&self) -> ProtocolMessage {
        ProtocolMessage {
            id: 0,
            method: "__identity".to_string(),
            msg_type: MessageType::StreamSend,
            payload: serde_json::to_string(self).unwrap(),
        }
    }

    /// ProtocolMessageから復元
    pub fn from_protocol_message(msg: &ProtocolMessage) -> Result<Self, serde_json::Error> {
        serde_json::from_str(&msg.payload)
    }
}
```

**Step 4: テスト実行 → パス確認**

```bash
cargo test test_identity_channel_flow -- --nocapture
```

**Step 5: コミット**

```bash
git add crates/unison-protocol/src/network/identity.rs crates/unison-protocol/tests/test_identity_quic.rs
git commit -m "feat: Identity ChannelのProtocolMessage変換を追加"
```

---

### Task 6: creo-sync スキーマ定義と統合テスト

**Files:**
- Create: `crates/unison-protocol/schemas/creo_sync.kdl`
- Test: `crates/unison-protocol/tests/test_creo_sync.rs`

**Step 1: creo-syncスキーマを作成**

`crates/unison-protocol/schemas/creo_sync.kdl`:

```kdl
protocol "creo-sync" version="1.0.0" {
    namespace "club.chronista.sync"

    // === Control Plane ===
    channel "control" from="client" lifetime="persistent" {
        send "Subscribe" {
            field "category" type="string"
            field "tags" type="array"
        }
        recv "Ack" {
            field "status" type="string"
            field "channel_ref" type="string"
        }
    }

    // === Data Plane ===
    channel "events" from="server" lifetime="persistent" {
        send "MemoryEvent" {
            field "event_type" type="string" required=true
            field "memory_id" type="string" required=true
            field "category" type="string"
            field "from" type="string"
            field "timestamp" type="timestamp"
        }
    }

    channel "query" from="client" lifetime="transient" {
        send "Query" {
            field "method" type="string" required=true
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
    channel "messaging" from="either" lifetime="persistent" {
        send "CCMessage" {
            field "from" type="string" required=true
            field "to" type="string"
            field "content" type="string" required=true
            field "thread" type="string"
        }
        recv "CCMessage"
    }

    // === Urgent Plane ===
    channel "urgent" from="server" lifetime="transient" {
        send "Alert" {
            field "level" type="string" required=true
            field "title" type="string" required=true
            field "body" type="string"
        }
    }
}
```

**Step 2: 統合テストを書く**

`crates/unison-protocol/tests/test_creo_sync.rs`:

```rust
use unison_protocol::parser::SchemaParser;
use unison_protocol::codegen::CodeGenerator;
use unison_protocol::codegen::rust::RustGenerator;

#[test]
fn test_creo_sync_parse_and_generate() {
    let schema = std::fs::read_to_string("schemas/creo_sync.kdl").unwrap();
    let parser = SchemaParser::new();
    let parsed = parser.parse(&schema).unwrap();

    let protocol = &parsed.protocols[0];
    assert_eq!(protocol.name, "creo-sync");
    assert_eq!(protocol.channels.len(), 5); // control, events, query, messaging, urgent

    // コード生成
    let generator = RustGenerator::new();
    let code = generator.generate(&parsed).unwrap();

    // 全メッセージ型が生成されること
    assert!(code.contains("pub struct Subscribe"));
    assert!(code.contains("pub struct MemoryEvent"));
    assert!(code.contains("pub struct Query"));
    assert!(code.contains("pub struct CcMessage") || code.contains("pub struct CCMessage"));
    assert!(code.contains("pub struct Alert"));

    // Connection型が生成されること
    assert!(code.contains("CreoSyncConnection"));
}
```

**Step 3: テスト実行 → パス確認**

```bash
cargo test test_creo_sync -- --nocapture
```

**Step 4: コミット**

```bash
git add crates/unison-protocol/schemas/creo_sync.kdl crates/unison-protocol/tests/test_creo_sync.rs
git commit -m "feat: creo-syncスキーマ定義と統合テスト"
```

---

### Task 7: 全テスト実行と最終確認

**Step 1: 全テスト実行**

```bash
cd /Users/makoto/repos/unison
cargo test --workspace
```
Expected: ALL PASS

**Step 2: clippy**

```bash
cargo clippy --workspace --all-targets
```
Expected: 警告なし

**Step 3: fmt**

```bash
cargo fmt --all -- --check
```
Expected: フォーマット済み

**Step 4: コミット（必要な修正があれば）**

```bash
git add -A
git commit -m "chore: lint修正と最終調整"
```

---
