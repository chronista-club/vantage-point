# REQ-PROMPT: ユーザープロンプトシステム

> CCからユーザーへの双方向リクエストシステム

## 概要

Stand内で動作するClaude Code（CC）がユーザーに対して質問・確認・選択を要求し、
ユーザーの応答を受け取って処理を継続するシステム。

## 要件

### REQ-PROMPT-001: プロンプトタイプ

CCからユーザーへのリクエストは以下のタイプをサポート:

| タイプ | 説明 | 例 |
|--------|------|-----|
| `confirm` | Yes/No確認 | "このファイルを削除しますか？" |
| `input` | テキスト入力 | "APIキーを入力してください" |
| `select` | 単一選択 | "使用するフレームワークを選んでください" |
| `multi_select` | 複数選択 | "有効にする機能を選んでください" |

### REQ-PROMPT-002: 表示先

プロンプトは以下のUIで表示:

- **WebUI**: Stand起動時のWebViewで表示
- **VantagePoint.app**: メニューバーアプリで通知/ダイアログ表示
- **両方同時**: どちらからでも応答可能（先に応答した方が有効）

### REQ-PROMPT-003: プロトコル準拠

既存プロトコルとの互換性:

- **AG-UI**: `PermissionRequest` / `PermissionResponse` を拡張
- **ACP**: `session/request_permission` メソッドを使用

### REQ-PROMPT-004: タイムアウト

- デフォルトタイムアウト: 300秒（5分）
- タイムアウト時はCC側でキャンセル扱い
- UIにカウントダウン表示

### REQ-PROMPT-005: 応答形式

ユーザー応答は以下の情報を含む:

```typescript
interface UserPromptResponse {
  request_id: string;
  // 応答タイプ
  outcome: "approved" | "rejected" | "cancelled" | "timeout";
  // テキスト応答（inputタイプ、またはコメント）
  message?: string;
  // 選択された選択肢ID（select/multi_select）
  selected_options?: string[];
}
```

## データ構造

### UserPrompt（CC → ユーザー）

```typescript
interface UserPrompt {
  // 一意のリクエストID
  request_id: string;
  // 実行中のrun_id
  run_id: string;
  // プロンプトタイプ
  prompt_type: "confirm" | "input" | "select" | "multi_select";
  // タイトル
  title: string;
  // 詳細説明
  description?: string;
  // 選択肢（select/multi_selectの場合）
  options?: PromptOption[];
  // デフォルト値（inputの場合）
  default_value?: string;
  // タイムアウト秒数
  timeout_seconds: number;
  // タイムスタンプ
  timestamp: number;
}

interface PromptOption {
  id: string;
  label: string;
  description?: string;
}
```

## フロー

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   CC Agent   │     │    Stand     │     │  WebUI/App   │
└──────┬───────┘     └──────┬───────┘     └──────┬───────┘
       │                    │                    │
       │ UserPrompt         │                    │
       │───────────────────>│                    │
       │                    │ USER_PROMPT event  │
       │                    │───────────────────>│
       │                    │                    │
       │                    │         ユーザー操作│
       │                    │<───────────────────│
       │                    │ UserPromptResponse │
       │ Response           │                    │
       │<───────────────────│                    │
       │                    │                    │
       │ 処理継続           │                    │
       ▼                    ▼                    ▼
```

## AG-UI拡張イベント

### USER_PROMPT（新規）

```json
{
  "type": "USER_PROMPT",
  "run_id": "run-123",
  "request_id": "prompt-456",
  "prompt_type": "select",
  "title": "使用するデータベースを選択",
  "description": "プロジェクトで使用するデータベースを選んでください",
  "options": [
    { "id": "postgres", "label": "PostgreSQL", "description": "リレーショナルDB" },
    { "id": "mongodb", "label": "MongoDB", "description": "ドキュメントDB" },
    { "id": "sqlite", "label": "SQLite", "description": "軽量ファイルDB" }
  ],
  "timeout_seconds": 300,
  "timestamp": 1702900000000
}
```

### USER_PROMPT_RESPONSE（新規）

```json
{
  "type": "USER_PROMPT_RESPONSE",
  "run_id": "run-123",
  "request_id": "prompt-456",
  "outcome": "approved",
  "selected_options": ["postgres"],
  "message": "PostgreSQLで進めてください",
  "timestamp": 1702900010000
}
```

## 実装箇所

| レイヤー | ファイル | 内容 |
|---------|----------|------|
| Protocol | `agui.rs` | `UserPrompt`, `UserPromptResponse` イベント追加 |
| Stand | `server.rs` | WebSocket経由でイベント送受信 |
| WebUI | `index.html` | プロンプトダイアログUI |
| Mac App | `PromptService.swift` | 通知/ダイアログ表示 |

## 関連要件

- REQ-AGUI-021: Human-in-the-Loop Events
- REQ-PROTO-002: ACP準拠
- REQ-PROTO-003: Vantage拡張
