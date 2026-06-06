# Guide: dev-flow primitives (`flow_handoff` / `flow_progress`)

> **Status**: MVP + 5-state FSM (2026-05-28、 `mako/flow-tools`)
> **Scope**: Conductor × Performer × Memory orchestration の core 操作を CLI + MCP 両方から 1 call で。

dev-flow (= Conductor が複数 Performer に並列 task を渡し、 進捗を集約する開発手順) の頻出操作を atomic primitive 化した。

| 操作 | 旧 (= 多 step) | 新 (= 1 step) |
|------|---------------|---------------|
| P4: handoff | `add_performer` + `wire_send` + tmux send-keys (= 3 step) | `flow_handoff` |
| P5: parallel 追跡 | `list_lanes` + `wire_recv` + `tmux_capture` (= 別々) | `flow_progress` |

---

## 1. `flow_handoff` — atomic 手渡し

新規 Performer を作成 → 初手 task spec を wire_send → tmux nudge で worker を起動、 を 1 call で。 失敗時は performer を rollback (= dirty state を残さない)。

### MCP tool

```jsonc
mcp__vantage-point__flow_handoff {
  "name": "feat-api",                 // 必須: performer slug
  "task_spec": "# mission\n...",      // 必須: worker への markdown 仕様
  "branch": "mako/feat-api",          // 省略時 `<git-user>/<slug>` を auto-derive
  "stand": "echoes",                  // default "echoes" (= Claude CLI)
  "mode": "hitl",                     // "hitl" (default、 nudge 後応答期待) / "auto"
  "nudge": true                       // default true、 false で tmux send-keys を skip
}
// →
{
  "performer_address": "agent@vantage-point/feat-api",
  "lane_address": "vantage-point/performer/feat-api",
  "wire_msg_id": "019e...",
  "performer_dir": "/.../.vp/lanes/feat-api",
  "branch": "mako/feat-api",
  "mode": "hitl",
  "nudge": "sent"
}
```

### CLI

```bash
# stdin から task spec を渡す
echo "# mission..." | vp flow handoff feat-api --task-spec -

# ファイルから
vp flow handoff feat-api --task-spec /tmp/task.md --branch mako/feat-api

# nudge を skip (= 完全 async、 完了 wire を待つだけ)
vp flow handoff feat-api --task-spec /tmp/task.md --no-nudge

# auto mode (= 応答期待しない、 後で wire_recv で結果回収)
vp flow handoff feat-api --task-spec /tmp/task.md --mode auto
```

### rollback

`wire_send` 失敗時のみ performer を削除して error 返却 (= dirty state を残さない)。
`nudge` 失敗は best-effort 扱い、 handoff 全体は成功で返る (= wire は届いており worker は自走可)。

---

## 2. `flow_progress` — 並列追跡集約 view

現 project の全 lane (conductor + performers) の **git status + 未読 wire 数 + 5-state FSM (= control surrender model)** を 1 view で。 read-only (= cursor 不触り)、 何度 call しても side-effect なし。

### MCP tool

```jsonc
mcp__vantage-point__flow_progress {}
// →
{
  "project": "vantage-point",
  "conductor": {
    "address": "agent@vantage-point",
    "unread_wire_count": 2,
    "unread_by_thread": { "019e...": 1, "019f...": 1 }
  },
  "performers": [{
    "name": "feat-api",
    "address": "agent@vantage-point/feat-api",
    "state": "Running",                          // = SP の Lane state (生死)
    "stand": "echoes",
    "cwd": "/.../.vp/lanes/feat-api",
    "performer_status": {
      "branch": "mako/feat-api",
      "dirty_count": 2,
      "ahead": 3,
      "behind": 0,
      "has_upstream": true,
      "last_commit": "abc1234 wip impl",
      "is_merged": false
    },
    "unread_wire_count": 0,
    "unread_by_thread": {},

    // 5-state FSM (= 後述 §3、 cursor 不触りで derive)
    "flow_state": "hitl_pending",                 // idle | working | hitl_pending | completed | stuck
    "control_surrender": false,                   // conductor が control 手放したか
    "state_reason": "performer posted question, awaiting conductor reply",
    "last_state_transition_at": 1779986000000     // epoch ms (= latest wmsg created_at proxy)
  }, ...]
}
```

### CLI

```bash
# JSON 出力 (default、 機械処理用)
vp flow progress

# table 形式 (human 用)
vp flow progress --format table
```

`--format table` の出力例:

```
Project: vantage-point
  Conductor unread wire: 2

PERFORMER                STATE      MODE                 AHEAD  BEHIND   DIRTY  UNREAD BRANCH
feat-api                 Running    🤝 hitl-pending          3       0       2       0 mako/feat-api
chore-deps               Running    🤖 auto-running          1       0       0       3 mako/chore-deps
flow-tools               Running    ✅ completed             8       2       0       0 mako/flow-tools
```

emoji label の意味:

| label | flow_state | 意味 |
|---|---|---|
| `⏸ idle` | `idle` | wire activity 一切なし (= 新規 performer) |
| `🤖 auto-running` | `working` | conductor が control 手放し中、 performer 自走 |
| `🤝 hitl-pending` | `hitl_pending` | performer が question 投げて conductor reply 待ち |
| `✅ completed` | `completed` | performer が complete 報告済 |
| `⚠ stuck` | `stuck` | conductor 指示後 dirty 残り commit 無し |

---

## 3. 5-state FSM (= control surrender model)

各 performer の `flow_state` は **2 つの input から derive** される (= store なし、 pure derivation):

1. **最新 wire activity** (= `wire_latest_msg(agent_addr)` の direction + `body.kind`)
2. **performer_status** (= `dirty_count` / `last_commit` から `dirty` / `has_commit` を抽出)

cascade match (= Rust の match 表現):

```rust
match (latest_msg, dirty, has_commit) {
    (None, _, _) => Idle,                                                       // wire 無し
    (Some(m), _, _) if m.from == conductor && m.kind == "task"       => Working,      // 初手 handoff
    (Some(m), _, _) if m.from == performer && m.kind == "question"   => HitlPending,  // conductor reply 待ち
    (Some(m), _, _) if m.from == performer && m.kind == "complete"   => Completed,    // 完了報告
    (Some(m), true, false) if m.from == conductor                    => Stuck,        // dirty 残り commit 無し
    _                                                            => Working,     // fallback
}
```

`control_surrender` は次の条件で `true`:

```
state ∈ {Working, Completed} && (last_msg.from == performer || last_msg is None)
```

### wire kind taxonomy

| kind | direction | 意味 |
|---|---|---|
| `task` | conductor → performer | 初手 handoff spec |
| `question` | performer → conductor | 質問 / decision 依頼 |
| `ack` | performer → conductor | 受領 / progress |
| `decision` | performer → conductor | 自己判断表明 |
| `approve` / `modify` / `clarify` | conductor → performer | reply |
| `complete` | performer → conductor | 完了報告 |
| `request` | performer → conductor | action 依頼 (= dogfood 等) |

wmsg を送るときは `body.kind` に上記のいずれかを入れる (= FSM derive が正しく走るため)。 規約は **convention であり enforcement ではない** — 不明な kind は fallback で `Working` に倒れる。

---

## 4. composition 図 (= 内部経路)

```
flow_handoff:
  POST /api/lanes  ─────────────────→ add_performer (= new_performer_in)
  POST /api/wire/send ──────────────→ WiremsgStore::send_root
  GET  /api/tmux/resolve-pane + POST /api/tmux/send-keys → nudge (best-effort)
   ↑ wire_send 失敗 → DELETE /api/lanes (rollback)

flow_progress:
  GET  /api/health                              → project name
  GET  /api/lanes                               → 全 lane (performer_status 込み)
  POST /api/wire/unread-count   (per lane × N)  → 未読 count (cursor 不触り)
  POST /api/wire/latest-msg     (per performer × M)  → 最新 wmsg (FSM derive 入力)
```

`flow_*` は既存 primitive (`add_performer` / `wire_send` / `tmux_send_keys` / `list_lanes`) の上に乗る薄い composition tool。 既存 primitive は撤去せず後方互換、 単発で叩く path は引き続き有効。

新規 supporting endpoint (= cursor 不触り、 read-only):

- `POST /api/wire/unread-count` — `{agent}` → `{total, by_thread}`
- `POST /api/wire/latest-msg` — `{agent}` → `{message}` (= 最新 1 件 or null)

---

## 5. scope 外 (= 別 PR)

- `flow_status` / `flow_advance` / `flow_council` / `flow_trail` = 次段 primitive、 今は MVP 2 tool のみ
- pane_persist (canvas 永続化) = 別 performer 担当 (`pp-content-persist`)
- 詳細 mcp/cli pair audit = 別 performer 担当 (`mcp-cli-audit`)
- 真の state transition tracking (= 各遷移の timestamp/reason を独立して保存) = metadata table が必要、 MVP は latest wmsg の `created_at` を proxy として返す
