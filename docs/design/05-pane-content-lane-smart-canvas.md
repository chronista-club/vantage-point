# Pane / Content / Lane / Smart Canvas 統合設計 (2026-04-21)

VP の UI アーキテクチャを 4 層に分離、Smart Canvas (Paisley Park 🧭 の進化形) を中核 content type として確立。
fleetflow `mem_1CaGRNrt5VbAmrUh8sWbYw` (Commander + Plugin SDK) と並ぶ本体側デザインメモ。

---

## 1. 4 層モデル

```
Project      (vantage-point)
└── Lane     (lead / worker-X)            ← Stone Free 🧵 管理
    └── Pane (矩形 layout、split tree)
        └── Content (stand-owned 中身)
```

| 層 | 概念 | 主な操作 |
|----|------|---------|
| 1 Project | repo 単位 | register / remove / peek |
| 2 Lane | 実行 context (Lead or Worker) | create / destroy / hibernate / resume |
| 3 Pane | 矩形 layout | split / resize / focus / close |
| 4 Content | Stand-owned object | attach / detach / migrate / mirror |

---

## 2. Content type カタログ

| Stand | Content type | 説明 |
|-------|--------------|------|
| Heaven's Door 📖 | `heavens-door` (mode: chat / tui) | Claude CLI (GUI chat 版追加、session 共有で切替) |
| The Hand ✋ | `bare-shell` | 素 PTY (CC なし) |
| Paisley Park 🧭 | `smart-canvas` | Items + pin + memory 統合 workspace |
| Gold Experience 🌿 | `ruby-repl` | Ruby VM 対話環境 (将来) |
| Hermit Purple 🍇 | `midi-control` | MIDI UI (将来) |
| Whitesnake 🐍 | `db-viewer` | SurrealDB query view (将来) |

---

## 3. Smart Canvas

### Public / Internal naming
- **Public**: Smart Canvas
- **Stand**: Paisley Park 🧭
- **Tech layer**: `canvas_*` MCP tools, `smart-canvas` Content type

### Item-based design

Canvas は items array を保持、各 item:
- type: markdown / memory / url / image / mermaid / log / data
- content (type-specific)
- pinned (bool)
- tags
- expires_at (optional TTL)
- layout (x/y/w/h optional)

### "最適表示のアシスト" philosophy
- Layout engine が signals (pin / tags / viewport / focus) から optimal layout を提案
- `canvas_suggest_layout(pane_id)` で reasoning 付き proposal
- Manual / Auto / Focus の 3 動作モード
- MCP / GUI 対称 API

---

## 4. HD 2 モード (Chat / TUI)

ユーザー好みで選択、session_id 共有で無縫切替:

| Feature | TUI | Chat GUI |
|---------|-----|----------|
| 対象 | power user | light user |
| 見た目 | ratatui + ANSI | message bubbles |
| 入力 | raw keyboard | text input + attachments |
| 出力 | stream raw | parsed blocks (markdown) |
| tool use 表示 | inline text | expandable card |

- System default = Chat (初見 friendly)
- Per-project / per-user override
- Runtime toggle anytime

---

## 5. Lane Lifecycle

```
States: Creating → Active ⇄ Idle → Hibernated → Destroyed
```

- **Lead Lane**: Project 作成時に自動、Project 削除時のみ destroy
- **Worker Lane**: `vp ws new` で create、`vp ws rm` で destroy
- **Hibernated ≠ Destroyed** — "消してないが今使ってない" 状態
- **Resume**: Lead eager / Worker lazy

---

## 6. Pane / Content 分離

### 設計原則
Pane は **content_ref のみ保持**、Content は独立オブジェクト:
- Content は 0..N Pane に attach 可能 (mirror)
- Pane close しても Content は生存 (orphan)
- Content は addressable (ID 持ち)

### Migration rules

| Content type | Pane 内 | Lane 内 | Project 間 |
|-------------|---------|---------|-----------|
| Smart Canvas | ✅ | ✅ | ⚠ tags reset |
| HD | ✅ 同 Lane | ⚠ worker_dir 調整 | ❌ codebase 違い |
| The Hand | ✅ | ⚠ cwd 再設定 | ⚠ |

### Orphan 管理
- Unpinned: 24h TTL
- Pinned: 永続、orphan list で再 attach 可能
- 7 日 soft delete、その後 hard delete

---

## 7. Mirror View

同 Content を複数 Pane で表示。**Content state = 共有、View state = Pane 独立**:

- Content: items / session / PTY buffer / layout (mirror 同期)
- View: scroll / zoom / focus (per-Pane 独立)

### 特殊: HD Cross-mode Mirror
Pane A = Chat mode、Pane B = TUI mode、同 session を両方で見る。

### 制限
- Max 4 Pane per Content
- Lane 跨ぎ OK / Project 跨ぎ ❌
- 衝突は last-write-wins (CRDT は将来)

---

## 8. msg address 粒度 (3 tier)

| Tier | 例 | 用途 |
|------|-----|------|
| 1 Lane-level | `agent@project` / `worker-X@project` / `notify@project` | 恒久 actor |
| 2 Content-level | `canvas-{id}@project` (opt-in alias) | 特定 Content に送る |
| 3 Item-level | — addressable しない | tool 経由 (`canvas_move(item_id)`) |

### Canonical + Alias の 2 層

- Canonical: `{stand}.{lane}@{project}` (例: `hd.lead@vp`)
- Alias: `agent@vp` → `hd.lead@vp`、`worker-VP-10@vp` → `hd.worker-VP-10@vp`
- alias は永久互換、canonical は拡張性

### Linear 命名統一
Issue ID = worker name = branch suffix = actor address 部分:
```
Issue "VP-10" → worker "VP-10" → branch "mako/vp-10-..." → address "worker-VP-10@vp"
```

---

## 9. Session Persistence

### 保存先 3 層
- **Whitesnake 🐍** (SurrealDB): Lane / Pane / Content / items / mirror 関係
- **Config files**: projects, preferences, project preset
- **External**: claude jsonl (session), ccws clones, tmux sessions

### Eager / Lazy
- Project list, Lead Lane: eager (起動即)
- Lead Pane tree + Contents: eager
- Worker Lane: **lazy** (sidebar click で activate)
- Orphan Contents: lazy
- HD messages (> last 50): lazy scroll

### Tombstone
- soft-delete 7 日、undo 可能
- hard-delete は確定

### 将来: Snapshot (git-like)
```
vp snapshot create "before-refactor"
vp snapshot restore "before-refactor"
vp snapshot diff "before-refactor" current
```

---

## 10. Lane 間独立性

### Default isolation
- Lead ↔ Worker の default: **msg のみ**
- Content の直接操作は不可

### 3 アクセスモード
1. **Message** (default): `msg_send` / `msg_recv`
2. **Peek** (read-only): `lane_peek(lane_id)` → snapshot
3. **Share / Mirror** (read-write): `content_attach(from_worker, to_lead_pane)`

### Parent-child 関係
- Lead = 親、Worker = 子
- Lead は Worker destroy 可、逆は不可
- Worker 同士は対等 (sibling、対等 msg/peek/share)

### UI
Sidebar で各 Worker Lane に `[peek]` / `[mirror to lead]` / `[destroy]` 操作。

---

## 11. Phase ロードマップ（proposed）

| Phase | 内容 | 優先 |
|-------|------|------|
| **P0** | Data model (Whitesnake schema) + API spec | High |
| **P1** | Backend: Lane / Pane / Content CRUD + Msgbox registry 拡張 | High |
| **P2** | Smart Canvas backend (items store + event bus) | High |
| **P3** | Smart Canvas frontend (Masonry grid + Card component) | High |
| **P4** | Migration / Mirror implementation | Med |
| **P5** | HD Chat mode UI (SwiftUI) | High |
| **P6** | Memory 統合 (双方向 sync、search UI) | Med |
| **P7** | Canvas brain (layout suggest, adaptive) | Low |
| **P8** | Cross-Lane peek / share UX | Med |
| **P9** | Snapshot (git-like state history) | Low |

---

## 12. 既存資産との対応

| 既存 | 本設計での位置付け |
|------|-------------------|
| `show` MCP tool | shim (Phase 4 まで)、後 deprecated |
| pane_id parameter | Canvas v1 の構造に依存、v2 で廃止 |
| `clear` / `close_pane` | `canvas_clear` / pane_close に整理 |
| VPPaneContainer (Swift) | 存続、content_ref ベースに refactor |
| `vp_mdast_wasm` | 継続使用 (markdown item renderer) |
| Liquid Glass 計画 | Smart Canvas と統合 (カード UI が Liquid Glass 素材) |

---

## 13. 関連

- **Commander / Bad Company 🪖** (fleetflow atlas `mem_1CaGRNrt5VbAmrUh8sWbYw`): cloud 側 orchestrator、Smart Canvas は Field Report の表示先として機能
- **Stone Free Cloud (VP-71)**: sf.vantage-point.app = msg mesh の cloud 延長、Smart Canvas の cross-device 共有に寄与
- **Creo ID (VP-63)**: Canvas の cross-device / cross-user sync 前提
- **既存 memo**: `mem_1CaGSVAXWkrkPnrUmABQQd` (zero-local-config UX 原則)

---

## 14. 未確定事項

- Smart Canvas backend の tech stack: 既存 vantage-point Rust に追加 vs 新 crate
- Item data の size limit / compression
- Collaborative editing (Mirror で複数 user が同時編集) の CRDT 採用時期
- Pane / Content API の Swift side (vp-bridge) での露出方式

これらは実装着手時に個別 Issue 化。

---

## 15. Requiem Architecture Evolution (事後追記 2026-04-21)

本 doc 起草後の深掘り議論で、4 層モデルに **event-sourced reactive Stand Ensemble** としての昇華が起きた。全 Stand を actor 化、mailbox / event bus / state を 1 つの event stream に統一。

**核心原則**: "Everything is events"
- PP = Information Router (routing を event で publish する特化 Stand)
- Whitesnake = event log + materialized view
- CreoUI = schema owner (VP は render client)
- Causation chain built-in (全 event に `causation: Option<EventId>`)

### 決定済み (D-1 ~ D-12)

| # | 論点 | 決定 |
|---|------|------|
| D-1 | CreoUI delegation | Yes, schema owner = creo-memories |
| D-2 | Smart Canvas ↔ PP | Option D: PP = Information Router Orchestrator |
| D-3 | PP naming | Paisley Park keep / public "Information Router" |
| D-4 | PP 動作モード | Hybrid (MVP Passive → v2 Active → v3 AI-driven) |
| D-5 | The Hand routing | Surface + Permission Gate |
| D-6 | Requiem 命名 | Selective (PP/Whitesnake/HD のみ) |
| D-7 | Causation UI | B+C (Dev Panel 常駐 + on-demand "why?") |
| D-8 | Event topic schema | Hybrid canonical (Unison match) + alias |
| D-9 | User event 粒度 | Medium + opt-in Fine |
| D-10 | State projection | MVP 6 + Eager + 1y compact |
| D-11 | Migration 順序 | B (SC 先行 + Whitesnake 並行) |
| D-12 | CreoUI schema 戦略 | C (Co-design) |

### Linear Epic
- [VP-72 Requiem Architecture Epic](https://linear.app/chronista/issue/VP-72)
- 子 issues: VP-73 (R0) / VP-74 (R1) / VP-75 (R2) / VP-76 (R3)

### 関連 creo-memories
- VP 4 層 Core: `mem_1CaGtbmxgE7UKcQNCyauTT`
- Stand Ensemble / Requiem: `mem_1CaGvxreWpPRsMrfmddMai`
- Final Summary: `mem_1CaGxnzEsjyyvnqaaVSFBH`
- CreoUI handoff: `mem_1CaFLjx1ATHBeDDkW9sY8B` (nexus から)

### 実装 Phase
v0.15 (R0-R3) → v0.16 (PP/HD Requiem) → v0.17 (TH + User event) → v0.18 (Snapshot + Cross-device)
