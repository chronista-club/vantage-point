# Multiplexer 設計

> 複数の Paisley Park と Terminal を統合管理するオーケストレーション層

## 概要

Multiplexerは、複数のPaisley ParkとTerminalを束ねて、
並列タスク実行・進捗管理・結果集約を行うオーケストレーション層。

### コンセプト

```
┌─────────────────────────────────────────────────────────────┐
│                      Multiplexer                             │
│  「複数のPaisley Parkを束ね、並列タスクを統率する」          │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ Paisley Park│  │ Paisley Park│  │ Paisley Park│         │
│  │  (proj-A)   │  │  (proj-B)   │  │  (proj-C)   │         │
│  │ ┌───┬───┐   │  │ ┌───┐       │  │ ┌───┬───┐   │         │
│  │ │T1 │T2 │   │  │ │T3 │       │  │ │T4 │T5 │   │         │
│  │ └───┴───┘   │  │ └───┘       │  │ └───┴───┘   │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
│                                                              │
│  ◆ タスク単位の並列実行                                      │
│  ◆ プロジェクト横断操作                                      │
│  ◆ 自由なグループ編成                                        │
│  ◆ MCP経由でAI Agentから操作可能                            │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

## アーキテクチャ

### コンポーネント構成

```
┌─────────────────────────────────────────────────────────────┐
│                      The World                               │
│  ┌─────────────────────────────────────────────────────┐    │
│  │                   Multiplexer                        │    │
│  │  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐    │    │
│  │  │   Group     │ │    Task     │ │  Progress   │    │    │
│  │  │  Manager    │ │  Scheduler  │ │   Tracker   │    │    │
│  │  └─────────────┘ └─────────────┘ └─────────────┘    │    │
│  │  ┌─────────────┐ ┌─────────────┐                    │    │
│  │  │  Terminal   │ │   Result    │                    │    │
│  │  │    Pool     │ │ Aggregator  │                    │    │
│  │  └─────────────┘ └─────────────┘                    │    │
│  └─────────────────────────────────────────────────────┘    │
│                          │                                   │
│                          ▼                                   │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              Paisley Park Registry                   │    │
│  │  [park-A] [park-B] [park-C] ...                     │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## 操作モデル

### 1. タスク単位の並列実行

同一タスクを複数のPaisley Parkで並列実行:

```
Task: "Run tests across all projects"
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
   [park-A]    [park-B]    [park-C]
   cargo test  npm test   pytest
        │           │           │
        └───────────┼───────────┘
                    ▼
            Result Aggregation
```

### 2. プロジェクト横断操作

複数プロジェクトを対象にした一括操作:

```
Operation: "Update dependencies"
                    │
    ┌───────────────┼───────────────┐
    ▼               ▼               ▼
[proj-A]        [proj-B]        [proj-C]
cargo update    npm update      pip install -U
```

### 3. 自由なグループ編成

ユーザー定義のグループでPaisley Parkを束ねる:

```kdl
group "frontend" {
    park "proj-web"
    park "proj-mobile"
}

group "backend" {
    park "proj-api"
    park "proj-worker"
}
```

## データモデル

### Group

```rust
struct Group {
    id: String,
    name: String,
    parks: Vec<ParkId>,
    created_at: u64,
}
```

### Task

```rust
struct Task {
    id: String,
    name: String,
    command: TaskCommand,
    target: TaskTarget,
    status: TaskStatus,
    created_at: u64,
    started_at: Option<u64>,
    completed_at: Option<u64>,
}

enum TaskCommand {
    Shell(String),           // シェルコマンド
    Script(PathBuf),         // スクリプトファイル
    Prompt(String),          // AIプロンプト
}

enum TaskTarget {
    AllParks,                // 全Paisley Park
    Group(GroupId),          // 特定グループ
    Parks(Vec<ParkId>),      // 個別指定
}

enum TaskStatus {
    Pending,
    Running { progress: Vec<ParkProgress> },
    Completed { results: Vec<ParkResult> },
    Failed { error: String },
    Cancelled,
}
```

### Progress

```rust
struct ParkProgress {
    park_id: String,
    status: ParkTaskStatus,
    output: String,          // 途中出力
    progress_pct: Option<u8>,
}

enum ParkTaskStatus {
    Waiting,
    Running,
    Completed,
    Failed,
}
```

### Result

```rust
struct ParkResult {
    park_id: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u64,
}

struct AggregatedResult {
    task_id: String,
    total: usize,
    succeeded: usize,
    failed: usize,
    results: Vec<ParkResult>,
}
```

## MCP Tools

Multiplexerは以下のMCPツールを提供:

### グループ管理

```json
{
    "name": "multiplexer_create_group",
    "description": "Paisley Parkのグループを作成",
    "inputSchema": {
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "park_ids": { "type": "array", "items": { "type": "string" } }
        }
    }
}
```

```json
{
    "name": "multiplexer_list_groups",
    "description": "グループ一覧を取得"
}
```

### タスク実行

```json
{
    "name": "multiplexer_dispatch",
    "description": "タスクを複数Paisley Parkに配信",
    "inputSchema": {
        "type": "object",
        "properties": {
            "command": { "type": "string" },
            "target": {
                "oneOf": [
                    { "type": "string", "enum": ["all"] },
                    { "type": "object", "properties": { "group": { "type": "string" } } },
                    { "type": "object", "properties": { "parks": { "type": "array" } } }
                ]
            }
        }
    }
}
```

```json
{
    "name": "multiplexer_status",
    "description": "実行中タスクの進捗を取得",
    "inputSchema": {
        "type": "object",
        "properties": {
            "task_id": { "type": "string" }
        }
    }
}
```

### 結果取得

```json
{
    "name": "multiplexer_result",
    "description": "完了タスクの結果を取得",
    "inputSchema": {
        "type": "object",
        "properties": {
            "task_id": { "type": "string" }
        }
    }
}
```

## View統合

### 進捗表示

Multiplexerの進捗はViewPointに自動表示:

```
┌─────────────────────────────────────────────────────────┐
│  Task: "cargo test" across 3 projects                    │
├─────────────────────────────────────────────────────────┤
│  [████████████████████] proj-A    ✓ Completed (2.3s)    │
│  [████████░░░░░░░░░░░░] proj-B    ⋯ Running (45%)       │
│  [░░░░░░░░░░░░░░░░░░░░] proj-C    ⏳ Waiting            │
├─────────────────────────────────────────────────────────┤
│  Total: 1/3 completed                                    │
└─────────────────────────────────────────────────────────┘
```

### 結果表示

```
┌─────────────────────────────────────────────────────────┐
│  Task: "cargo test" - Completed                          │
├─────────────────────────────────────────────────────────┤
│  ✓ proj-A: 45 tests passed (2.3s)                       │
│  ✓ proj-B: 32 tests passed (1.8s)                       │
│  ✗ proj-C: 2 tests failed (0.5s)                        │
│    └── test_user_auth: assertion failed                 │
│    └── test_db_connection: timeout                      │
├─────────────────────────────────────────────────────────┤
│  Summary: 2/3 succeeded, 77/79 tests passed             │
└─────────────────────────────────────────────────────────┘
```

## Terminal Pool

### 概要

各Paisley Parkが管理するTerminalをプール化:

```
┌─────────────────────────────────────────────────────────┐
│                    Terminal Pool                         │
├─────────────────────────────────────────────────────────┤
│  park-A:                                                 │
│    [T1: idle] [T2: running "cargo build"]               │
│                                                          │
│  park-B:                                                 │
│    [T3: running "npm test"]                             │
│                                                          │
│  park-C:                                                 │
│    [T4: idle] [T5: idle]                                │
└─────────────────────────────────────────────────────────┘
```

### Terminal割り当て戦略

1. **Round Robin**: 各Parkに均等配分
2. **Load Balanced**: 負荷の低いParkに優先配分
3. **Affinity**: 特定タスクは特定Parkで実行

## 将来拡張

### リモートMultiplexer

複数マシン上のPaisley Parkを統合:

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Machine A  │     │  Machine B  │     │  Machine C  │
│  [park-A]   │────▶│  [park-B]   │────▶│  [park-C]   │
└─────────────┘     └─────────────┘     └─────────────┘
        │                   │                   │
        └───────────────────┼───────────────────┘
                            ▼
                    Multiplexer Hub
```

### ワークフロー定義

KDLによるワークフロー定義:

```kdl
workflow "deploy" {
    stage "build" parallel=true {
        task "cargo build --release" target="backend"
        task "npm run build" target="frontend"
    }
    stage "test" {
        task "cargo test" target="backend"
        task "npm test" target="frontend"
    }
    stage "deploy" {
        task "deploy.sh" target="all"
    }
}
```

## 関連ドキュメント

- [spec/07-the-world.md](../spec/07-the-world.md)
- [spec/08-paisley-park.md](../spec/08-paisley-park.md)
- [design/09-world-park-protocol.md](./09-world-park-protocol.md)
