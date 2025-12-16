# Vantage Point - アーキテクチャ設計

## 概要

自律的なAgentネットワークによる分散アーキテクチャ。
SurrealDBをSingle Source of Truthとし、各デバイス・Agentが協調動作する。

## システム全体図

```mermaid
graph TB
    subgraph "Agent Network"
        A1[Agent A<br/>コード生成]
        A2[Agent B<br/>調査・分析]
        A3[Agent C<br/>レビュー]
        A1 <--> A2
        A2 <--> A3
        A1 <--> A3
    end

    subgraph "Data Layer"
        DB[(SurrealDB<br/>Single Source of Truth)]
    end

    subgraph "Clients"
        Mac[Mac<br/>Swift]
        VP[Vision Pro<br/>Swift]
        iPad[iPad/iPhone<br/>Swift]
    end

    A1 --> DB
    A2 --> DB
    A3 --> DB

    Mac <--> DB
    VP <--> DB
    iPad <--> DB

    Mac <-.->|Unison Protocol<br/>QUIC| VP
    VP <-.->|Unison Protocol<br/>QUIC| iPad
    Mac <-.->|Unison Protocol<br/>QUIC| iPad
```

## 技術スタック

```mermaid
graph TB
    subgraph "Frontend"
        Solid[SolidJS<br/>Web UI]
    end

    subgraph "Backend (Rust主軸)"
        Rust[Rust Core<br/>Unison / HTTP]
        WASM[Agent層<br/>TypeScript → WASM]
        Rust --> WASM
    end

    subgraph "Data"
        DB[(SurrealDB)]
    end

    Solid <--> Rust
    Rust <--> DB
    WASM <--> DB
```

### レイヤー別技術選定

| レイヤー | 技術 | 役割 |
|---------|------|------|
| **Frontend (Web)** | SolidJS | 全プラットフォーム向けWeb UI |
| **Frontend (Native)** | Swift | Vision Pro最適化（Phase 2） |
| **Backend (Core)** | Rust | Unison Protocol, 通信層, 高性能処理 |
| **Backend (Agent)** | TypeScript → WASM | Agent SDK, MCP連携（Rustにインプロセス実行） |
| **Data** | SurrealDB | Single Source of Truth |

### アーキテクチャの特徴

**Rust主軸 + Agent層WASM**
- TypeScriptでAgentロジックを記述
- WASMにコンパイルしてRust内でインプロセス実行
- デプロイが単一バイナリでシンプル
- Rust↔Agent間通信が高速（プロセス間通信不要）

**P2Pデバイス同期 (CRDT)**
- どのデバイスもサーバー/クライアント両方になれる
- リーダーなし、全員対等
- CRDTで競合なく同期
- SurrealDBは永続化・初期同期用（SSoT）

### P2P同期アーキテクチャ

```mermaid
graph TB
    subgraph "P2P Network (Unison/QUIC)"
        Mac[Mac<br/>CRDT]
        VP[Vision Pro<br/>CRDT]
        iPad[iPad<br/>CRDT]

        Mac <-->|P2P Sync| VP
        VP <-->|P2P Sync| iPad
        Mac <-->|P2P Sync| iPad
    end

    subgraph "Persistence"
        DB[(SurrealDB<br/>SSoT)]
    end

    Mac -->|Async Write| DB
    VP -->|Async Write| DB
    iPad -->|Async Write| DB

    NewDevice[新デバイス] -->|Initial Fetch| DB
    NewDevice -->|Catch-up| Mac
```

### CRDT選定

| ライブラリ | 採用理由 |
|-----------|---------|
| **Loro** | Rust + Swift + WASMバインディング、高性能、最新 |

**同期フロー**:
1. ローカル操作 → CRDT更新
2. P2Pでオペレーション送信（Unison Protocol）
3. 定期的にSurrealDBへスナップショット保存
4. 新デバイス参加時はDBから初期化 → P2Pで最新に追従

### 通信プロトコル

```mermaid
graph LR
    subgraph "Unison Protocol"
        UP[Unison]
        QUIC[QUIC/HTTP3]
        KDL[KDL Schema]
        UP --> QUIC
        UP --> KDL
    end
```

## Agent構成

```mermaid
graph TB
    subgraph "Agent Lifecycle"
        direction LR
        Create[生成] --> Active[稼働]
        Active --> Idle[待機]
        Idle --> Active
        Idle --> Destroy[消滅]
    end

    subgraph "Agent Types"
        Main[メインAgent<br/>ユーザー対話]
        Task1[タスクAgent<br/>コード生成]
        Task2[タスクAgent<br/>調査]
        Task3[タスクAgent<br/>レビュー]
    end

    Main -->|委任| Task1
    Main -->|委任| Task2
    Main -->|委任| Task3
    Task1 -->|報告| Main
    Task2 -->|報告| Main
    Task3 -->|報告| Main
```

## 協調モード

```mermaid
stateDiagram-v2
    [*] --> 協調
    協調 --> 委任: ユーザー指示
    委任 --> 自律: ユーザー指示
    自律 --> 委任: タスク完了/確認必要
    委任 --> 協調: ユーザー指示
    協調 --> [*]

    協調: ユーザーと一緒に進める
    委任: 任せて途中経過・結果を確認
    自律: 完全に任せる
```

## デバイス間同期

```mermaid
sequenceDiagram
    participant Mac
    participant DB as SurrealDB
    participant VP as Vision Pro
    participant iPad

    Mac->>DB: 状態更新
    DB-->>VP: Live Query通知
    DB-->>iPad: Live Query通知

    Note over Mac,iPad: デバイス移動

    VP->>DB: 状態更新
    DB-->>Mac: Live Query通知
    DB-->>iPad: Live Query通知
```

## データ配置戦略

```mermaid
graph TB
    subgraph "Phase 1: 開発初期"
        Local[(SurrealDB<br/>Mac Local)]
    end

    subgraph "Phase 2: 本番運用"
        Cloud[(SurrealDB<br/>VPS)]
        NS1[namespace: vantage]
        NS2[namespace: creo-memories]
        Cloud --> NS1
        Cloud --> NS2
    end

    Local -.->|移行| Cloud
```

## 通信フロー

```mermaid
flowchart LR
    subgraph Client [Swift Client]
        UI[UI Layer]
        UC[Unison Client]
    end

    subgraph Network [Network]
        QUIC((QUIC<br/>TLS 1.3))
    end

    subgraph Server [Agent Server]
        US[Unison Server]
        Agent[Claude Agent]
        DB[(SurrealDB)]
    end

    UI --> UC
    UC <-->|KDL Messages| QUIC
    QUIC <--> US
    US <--> Agent
    Agent <--> DB
```

## 技術選定理由

| 技術 | 選定理由 |
|------|---------|
| **Unison Protocol (Rust)** | 型安全、低レイテンシ、自前でカスタマイズ可能 |
| **Swift Native** | Vision Pro最適化、ネイティブ体験 |
| **SolidJS** | 軽量、高性能、creo-memoriesと同構成 |
| **TypeScript** | Claude Agent SDK公式対応、MCP親和性 |
| **Claude Agent SDK** | MCP対応、自律Agent構築に最適 |
| **SurrealDB** | リアルタイム同期（Live Query）、柔軟なスキーマ |

## 開発フェーズ

```mermaid
graph LR
    P1[Phase 1<br/>SolidJS Web] --> P2[Phase 2<br/>Swift Native]
```

| Phase | Frontend | 対象 | 目的 |
|-------|----------|------|------|
| **Phase 1** | SolidJS (Web) | 全プラットフォーム | コア機能・対話スタイル確立 |
| **Phase 2** | Swift Native | Vision Pro | 空間体験の最適化 |

**方針**: まずWebで全プラットフォーム対応 → 体験が固まったらVision ProをNative化

## creo-memoriesとの関係

本プロジェクトはcreo-memoriesと同じ技術構成を採用:

| 共通点 | 内容 |
|--------|------|
| Frontend (Web) | SolidJS |
| Backend | Rust + TypeScript |
| Data | SurrealDB |
| 通信 | Unison Protocol |

**差異点**:
- Phase 2でVision Pro向けSwift Nativeクライアント追加
- Agent SDK活用による自律Agent機能

---

*作成日: 2025-12-16*
*ステータス: Draft*
