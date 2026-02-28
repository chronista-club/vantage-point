# MARU × VP クロスプロジェクト協業フロー 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** MARU側CCとVP側CCがcreo-memories経由でデータ共有・議論ログを蓄積できるようにする

**Architecture:** creo-memories MCPを両プロジェクトに追加し、CLAUDE.mdに協業規約を記載。Atlas IDを初期登録して運用開始。

**Tech Stack:** creo-memories MCP (SSE), CLAUDE.md, .mcp.json

---

### Task 1: VP側 .mcp.json にcreo-memories MCPを追加

**Files:**
- Modify: `.mcp.json`

**Step 1: creo-memories MCPサーバーを追加**

`.mcp.json` の `mcpServers` に以下を追加:

```json
"creo-memories": {
  "type": "http",
  "url": "https://mcp.creo-memories.in/"
}
```

**Step 2: コミット**

```bash
git add .mcp.json
git commit -m "chore: creo-memories MCPをプロジェクトに追加"
```

---

### Task 2: VP側 CLAUDE.md にクロスプロジェクト協業規約を追記

**Files:**
- Modify: `CLAUDE.md`

**Step 1: CLAUDE.mdの末尾にクロスプロジェクト協業セクションを追加**

```markdown
## クロスプロジェクト協業（MARU × VP）

本プロジェクトはMARU（ESP32-S3物理コントローラ）と連携開発を行っている。
creo-memoriesを共有データベースとして、CC間でデータ共有・議論を行う。

### セッション開始時

- `recall("cross-project discussion")` で未読の議論を確認する

### 記録規約

- 他プロジェクトに関わる設計決定時: `remember()` で `category: "cross-project"` に記録
- メタデータに `from: "vp"` を必ず付与
- 議論は `type: proposal / question / answer / decision` で分類
- タグ例: `wire-protocol`, `atlas`, `decision`, `discussion`

### 参照

- 設計ドキュメント: [docs/plans/2026-02-15-cross-project-collab-design.md](docs/plans/2026-02-15-cross-project-collab-design.md)
```

**Step 2: コミット**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.mdにクロスプロジェクト協業規約を追加"
```

---

### Task 3: MARU側 .mcp.json を作成

**Files:**
- Create: `/Users/makoto/repos/maru/maru-firmware/.mcp.json`

**Step 1: creo-memories MCPサーバーを設定**

```json
{
  "mcpServers": {
    "creo-memories": {
      "type": "sse",
      "url": "https://mcp.creo-memories.in/sse"
    }
  }
}
```

**Step 2: コミット**

```bash
cd /Users/makoto/repos/maru/maru-firmware
git add .mcp.json
git commit -m "chore: creo-memories MCPを追加（VP連携用）"
```

---

### Task 4: MARU側 CLAUDE.md を作成してクロスプロジェクト規約を記載

**Files:**
- Create: `/Users/makoto/repos/maru/maru-firmware/CLAUDE.md`

**Step 1: CLAUDE.mdを作成**

MARUプロジェクトの基本情報 + クロスプロジェクト協業規約を含む。
（内容はMARU側CCのコンテキストを参照して適切に記述）

**Step 2: コミット**

```bash
cd /Users/makoto/repos/maru/maru-firmware
git add CLAUDE.md
git commit -m "docs: CLAUDE.md作成（VP連携の協業規約を含む）"
```

---

### Task 5: creo-memoriesにAtlas IDを初期登録

**前提:** Task 1完了後、CCセッションを再起動してcreo-memories MCPが接続された状態で実行

**Step 1: Atlas IDをcreo-memoriesに保存**

```
mcp__creo-memories__remember({
  content: "# Atlas ID 一覧\n\n| ノード | ID |\n|--------|-----|\n| MARU (root) | 019c5ee3-b0d8-7577-9517-37f0091589ec |\n| Wire Protocol | 019c5ee3-bf93-76ef-988f-e32327c25c11 |\n| Volume Mode | 019c5ee3-c1d9-7fac-ab2e-c56599a78174 |\n| Agent (VP) | 019c5ee3-c7f6-740e-aee4-8615406cfa1d |",
  category: "cross-project",
  tags: ["atlas", "node-config", "maru", "vp"],
  contentType: "markdown"
})
```

**Step 2: 協業フロー設計の要約も登録**

```
mcp__creo-memories__remember({
  content: "# MARU × VP 協業フロー\n\ncreo-memoriesベースの非同期議論ストリーム。\ncategory: cross-project でデータ共有。\nmetadata.from で発信元を識別。\nmetadata.type で proposal/question/answer/decision を分類。\n\n設計詳細: vantage-point/docs/plans/2026-02-15-cross-project-collab-design.md",
  category: "cross-project",
  tags: ["collab-flow", "setup", "maru", "vp"],
  contentType: "markdown",
  metadata: {
    from: "vp",
    type: "decision"
  }
})
```

**Step 3: recallで登録確認**

```
mcp__creo-memories__recall({ query: "Atlas ID MARU VP", limit: 5 })
```

---

### Task 6: VP側の変更をpush

**Step 1: push**

```bash
git push origin main
```

---

### Task 7: 動作確認

**Step 1: VP側CCセッションを再起動**

creo-memories MCPが接続されることを確認。

**Step 2: MARU側CCセッションでrecall**

MARU側CCで `recall("cross-project")` を実行し、VP側が登録したAtlas IDと協業フロー設計が取得できることを確認。
