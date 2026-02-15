# MARU × VP クロスプロジェクト協業フロー設計

**日付**: 2026-02-15
**ステータス**: 承認済み

## 概要

MARU（ESP32-S3物理コントローラ）とVP（AI開発プラットフォーム）の2プロジェクト間で、
各Claude Codeセッションがcreo-memoriesを介してデータ共有・議論・ログ蓄積を行う仕組み。

## 目的

- 設計決定、Atlas ID、プロトコル仕様を一元管理
- CC間の非同期議論をcreo-memoriesに記録
- 議論の経緯をログとして永続化（後から振り返り可能）

## アーキテクチャ

```
┌──────────────┐         ┌──────────────────┐         ┌──────────────┐
│  MARU CC     │         │  creo-memories   │         │   VP CC      │
│  (ESP32側)   │◄───────►│  (SurrealDB)     │◄───────►│  (Rust側)    │
│              │  MCP    │                  │  MCP    │              │
│  remember()  │────────►│  category:       │◄────────│  remember()  │
│  recall()    │◄────────│  "cross-project" │────────►│  recall()    │
└──────────────┘         └──────────────────┘         └──────────────┘
       ▲                        ▲                           ▲
       │                        │                           │
       └────────────── ユーザー（三者議論） ─────────────────┘
```

## 共有データの種類

| 種類 | category | tags例 | 用途 |
|------|----------|--------|------|
| Atlas ID・ノード構成 | `cross-project` | `atlas`, `node-config` | ノードIDと役割定義 |
| プロトコル仕様 | `cross-project` | `wire-protocol`, `spec` | Wire Protocolの定義 |
| 設計決定 | `cross-project` | `decision`, `architecture` | 合意事項の記録 |
| 議論ログ | `cross-project` | `discussion`, `log` | CC間の非同期議論の経緯 |

## 議論ストリームの仕組み

### 書き込み規約

```typescript
mcp__creo-memories__remember({
  content: "議論内容（提案・質問・回答・決定）",
  category: "cross-project",
  tags: ["discussion", "wire-protocol"],  // トピックタグ
  contentType: "markdown",
  metadata: {
    from: "vp",           // or "maru"
    thread: "wire-protocol-state-update",  // スレッド識別子
    type: "proposal"      // proposal / question / answer / decision
  }
})
```

### 読み取り規約

```typescript
// セッション開始時: 未読の議論を確認
mcp__creo-memories__recall({
  query: "cross-project discussion",
  limit: 10
})

// 特定スレッドの議論を追跡
mcp__creo-memories__search({
  category: "cross-project",
  tags: ["discussion", "wire-protocol"]
})
```

### 議論フロー

```
1. [VP-CC] remember: 提案 (type: "proposal")
   → "Wire Protocolにstate_updateを追加したい"

2. [MARU-CC] recall → 提案を発見
   → remember: 回答 (type: "answer")
   → "MSG_STATE_UPDATE 0x81で実装済み"

3. [VP-CC] recall → 回答を確認
   → remember: 決定 (type: "decision")
   → "MSG_STATE_UPDATE 0x81を採用、VP側も実装する"
```

## セットアップ手順

### 1. creo-memories MCPを両プロジェクトに追加

各プロジェクトの `.mcp.json` に creo-memories MCPサーバーを追加:

```json
{
  "creo-memories": {
    "type": "sse",
    "url": "https://mcp.creo-memories.in/sse"
  }
}
```

### 2. CLAUDE.mdに規約を追加

各プロジェクトのCLAUDE.mdに以下を追記:

```markdown
## クロスプロジェクト協業

- セッション開始時: `recall("cross-project discussion")` で未読議論を確認
- 他プロジェクトに関わる設計決定時: `remember()` で `category: "cross-project"` に記録
- メタデータに `from: "vp"` (または `"maru"`) を必ず付与
- 議論は `type: proposal/question/answer/decision` で分類
```

### 3. Atlas IDの初期登録

最初にAtlas IDをcreo-memoriesに登録:

```typescript
mcp__creo-memories__remember({
  content: `# Atlas ID 一覧

  | ノード | ID |
  |--------|-----|
  | MARU (root) | 019c5ee3-b0d8-7577-9517-37f0091589ec |
  | Wire Protocol | 019c5ee3-bf93-76ef-988f-e32327c25c11 |
  | Volume Mode | 019c5ee3-c1d9-7fac-ab2e-c56599a78174 |
  | Agent (VP) | 019c5ee3-c7f6-740e-aee4-8615406cfa1d |`,
  category: "cross-project",
  tags: ["atlas", "node-config", "maru", "vp"],
  contentType: "markdown"
})
```

## ログとしての活用

creo-memoriesに記録された議論は自動的に永続化される。

- **経緯の振り返り**: `recall("なぜstate_updateはこの形式になったか")` で設計判断の背景を取得
- **新メンバーのオンボーディング**: cross-projectカテゴリを時系列で読めば経緯がわかる
- **意思決定の証跡**: `type: "decision"` でフィルタすれば合意事項だけ抽出可能
