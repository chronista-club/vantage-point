# Guide: dev-flow primitives (`flow_handoff` / `flow_progress`)

> **Status**: MVP + 6-state FSM (2026-05-28 初版 5-state、 2026-07-11 `awaiting_user` 追加で 6-state、 `mako/flow-tools`)
> **Scope**: Main × Sub × Memory orchestration の core 操作を CLI + MCP 両方から 1 call で。

dev-flow (= Main が複数 Sub に並列 task を渡し、 進捗を集約する開発手順) の頻出操作を atomic primitive 化した。

> messaging 全体（wire store / category / ack 台帳 / federation / flow_state の sidebar 投影）の見取り図は [`messaging.md`](./messaging.md)。 本 doc は dev-flow primitive（`flow_handoff` / `flow_progress`）の tool 詳細に絞る。

| 操作 | 旧 (= 多 step) | 新 (= 1 step) |
|------|---------------|---------------|
| P4: handoff | `add_sub` + `wire_send` + tmux send-keys (= 3 step) | `flow_handoff` |
| P5: parallel 追跡 | `list_lanes` + `wire_recv` + `tmux_capture` (= 別々) | `flow_progress` |

---

## 1. `flow_handoff` — atomic 手渡し

新規 Sub を作成 → 初手 task spec を wire_send → tmux nudge で worker を起動、 を 1 call で。 失敗時は sub を rollback (= dirty state を残さない)。

### MCP tool

```jsonc
mcp__vantage-point__flow_handoff {
  "name": "feat-api",                 // 必須: sub slug
  "task_spec": "# mission\n...",      // 必須: worker への markdown 仕様
  "branch": "mako/feat-api",          // 省略時 `<git-user>/<slug>` を auto-derive
  "agent": "claude",                  // default "claude" (= Claude CLI)
  "mode": "hitl",                     // "hitl" (default、 nudge 後応答期待) / "auto"
  "nudge": true                       // default true、 false で tmux send-keys を skip
}
// →
{
  "sub_address": "agent@vantage-point/feat-api",
  "lane_address": "vantage-point/sub/feat-api",
  "wire_msg_id": "019e...",
  "sub_dir": "/.../.vp/lanes/feat-api",
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

`wire_send` 失敗時のみ sub を削除して error 返却 (= dirty state を残さない)。
`nudge` 失敗は best-effort 扱い、 handoff 全体は成功で返る (= wire は届いており worker は自走可)。

---

## 2. `flow_progress` — 並列追跡集約 view

現 repo の全 lane (main + subs) の **git status + 未読 wire 数 + 6-state FSM (= control surrender model)** を 1 view で。 read-only (= cursor 不触り)、 何度 call しても side-effect なし。

### MCP tool

```jsonc
mcp__vantage-point__flow_progress {}
// →
{
  "repo": "vantage-point",
  "root": {
    "address": "agent@vantage-point",
    "unread_wire_count": 2,
    "unread_by_thread": { "019e...": 1, "019f...": 1 }
  },
  "subs": [{
    "name": "feat-api",
    "address": "agent@vantage-point/feat-api",
    "state": "Running",                          // = repo の Lane state (生死)
    "agent": "claude",
    "cwd": "/.../.vp/lanes/feat-api",
    "sub_status": {
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

    // 6-state FSM (= 後述 §3、 cursor 不触りで derive)
    "flow_state": "hitl_pending",                 // idle | working | hitl_pending | awaiting_user | completed | stuck
    "control_surrender": false,                   // main が control 手放したか
    "state_reason": "sub posted question, awaiting main reply",
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
Repo: vantage-point
  Main unread wire: 2

SUB                STATE      MODE                 AHEAD  BEHIND   DIRTY  UNREAD BRANCH
feat-api                 Running    🤝 hitl-pending          3       0       2       0 mako/feat-api
chore-deps               Running    🤖 auto-running          1       0       0       3 mako/chore-deps
flow-tools               Running    ✅ completed             8       2       0       0 mako/flow-tools
```

emoji label の意味:

| label | flow_state | 意味 |
|---|---|---|
| `⏸ idle` | `idle` | wire activity 一切なし (= 新規 sub) |
| `🤖 auto-running` | `working` | main が control 手放し中、 sub 自走 |
| `🤝 hitl-pending` | `hitl_pending` | sub が question 投げて main reply 待ち |
| `🙋 needs-you` | `awaiting_user` | sub が needs_user 投げて **ユーザ本人**の回答待ち (未 ack) |
| `✅ completed` | `completed` | sub が complete 報告済 |
| `⚠ stuck` | `stuck` | main 指示後 dirty 残り commit 無し |

---

## 3. 6-state FSM (= control surrender model)

各 sub の `flow_state` は **3 つの input から derive** される (= store なし、 pure derivation):

1. **最新 wire activity** (= `wire_latest_msg(agent_addr)` の direction + `body.kind`)
2. **sub_status** (= `dirty_count` / `last_commit` から `dirty` / `has_commit` を抽出)
3. **未 ack の needs_user wire** (= `wire/needs-user-pending`、 ack 台帳ベースの述語)

cascade match (= Rust の match 表現):

```rust
if pending_needs_user => AwaitingUser  // 未 ack needs_user は cascade より優先 (ack 台帳が SSOT)
match (latest_msg, dirty, has_commit) {
    (None, _, _) => Idle,                                                       // wire 無し
    (Some(m), _, _) if m.from == main && m.kind == "task"       => Working,      // 初手 handoff
    (Some(m), _, _) if m.from == sub && m.kind == "question"   => HitlPending,  // main reply 待ち
    (Some(m), _, _) if m.from == sub && m.kind == "complete"   => Completed,    // 完了報告
    (Some(m), true, false) if m.from == main                    => Stuck,        // dirty 残り commit 無し
    _                                                            => Working,     // fallback
}
```

`control_surrender` は次の条件で `true`:

```
state ∈ {Working, Completed} && (last_msg.from == sub || last_msg is None)
```

### wire kind taxonomy

| kind | direction | 意味 |
|---|---|---|
| `task` | main → sub | 初手 handoff spec |
| `question` | sub → main | 質問 / decision 依頼 (= main が捌ける相談) |
| `needs_user` | sub → main | **ユーザ本人**の意見が要る相談 (ack まで `awaiting_user`) |
| `ack` | sub → main | 受領 / progress |
| `decision` | sub → main | 自己判断表明 |
| `approve` / `modify` / `clarify` | main → sub | reply |
| `complete` | sub → main | 完了報告 |
| `request` | sub → main | action 依頼 (= dogfood 等) |

wmsg を送るときは `body.kind` に上記のいずれかを入れる (= FSM derive が正しく走るため)。 規約は **convention であり enforcement ではない** — 不明な kind は fallback で `Working` に倒れる。

### needs_user 規約 (= awaiting_user の入力、 2026-07-11)

sub が「main では捌けない、 **ユーザ本人**の意見が要る」相談を投げる時の規約:

- `body.kind = "needs_user"` + `body.category = "command"` で main 宛に `wire_send`
  (command なので ack されるまで delivery loop が re-nudge する)。
- 受信側 (main) は **ユーザの回答を sub に relay してから** `wire_ack` する。
  ack した瞬間に `awaiting_user` が解消される (= ack 台帳が SSOT)。
- 未 ack の needs_user が存在する間は、 sub が追加の wire (ack / decision 等) を
  送っても `awaiting_user` のまま (= latest cascade より優先)。
- `question` との使い分け: main が自分で判断して返せる相談は `question` (= `hitl_pending`)、
  ユーザの好み・意思決定が要る相談だけ `needs_user`。 sidebar の needs-you 表示
  (magenta diamond) は `awaiting_user` にのみ接続される — 乱発すると盤面が常時光って
  signal が死ぬので、 本当にユーザが要る時だけ使う。

### sidebar への投影 (= LaneInfo.flow_state、 2026-07-11)

daemon が vp-app へ lane snapshot を送る直前に、 sub の `LaneInfo` へ `flow_state` を
enrich する (= `vp flow progress` と同一判定、 送信時 derive で registry / db には保存しない)。
wire send/ack の成功が関与 repo の snapshot 再 push をトリガするため、 flow_state の変化は
polling 無しで sidebar に届く。 vp-app 側は `flow_state` を state 言語 (working / idle /
needs-you) の一次 source とし、 field 欠落時 (旧 daemon) は pid heuristic に fallback する。

---

## 4. composition 図 (= 内部経路)

L0 portless 完了後、 全 step は **daemon process-proxy dispatch** または **daemon "wire" channel** 経由（旧 SP HTTP 直叩きは撤去済）:

```
flow_handoff:
  lane_create  ─────────────────────→ create_sub_orchestrated (= new_sub_in)
  wire/send (daemon "wire" channel) ──→ WiremsgStore::send_root
  tmux_resolve_pane + tmux_send_keys → nudge (best-effort)
   ↑ wire_send 失敗 → lane_delete (rollback)

flow_progress:
  repo name (repo_path から導出)          → 旧 GET /api/health は撤去
  lanes_list                                    → 全 lane (sub_status 込み)
  wire/unread-count   (per lane × N)            → 未読 count (cursor 不触り)
  wire/latest-msg     (per sub × M)       → 最新 wmsg (FSM derive 入力)
```

`flow_*` は既存 primitive (`add_sub` / `wire_send` / `tmux_send_keys` / `list_lanes`) の上に乗る薄い composition tool。 これら primitive は L0 portless で全て daemon process-proxy dispatch / daemon "wire" channel に移行済（旧 SP HTTP 直叩きは撤去）、 単発で叩く path も dispatch 経由で引き続き有効。

supporting method (= cursor 不触り、 read-only、 daemon "wire" channel):

- `wire/unread-count` — `{agent}` → `{total, by_thread}`
- `wire/latest-msg` — `{agent}` → `{message}` (= 最新 1 件 or null)
- `wire/needs-user-pending` — `{agent}` → `{message}` (= agent **発**の未 ack needs_user 最新 1 件 or null)

---

## 5. scope 外 (= 別 PR)

- `flow_status` / `flow_advance` / `flow_council` / `flow_trail` = 次段 primitive、 今は MVP 2 tool のみ
- pane_persist (canvas 永続化) = 別 sub 担当 (`pp-content-persist`)
- 詳細 mcp/cli pair audit = 別 sub 担当 (`mcp-cli-audit`)
- 真の state transition tracking (= 各遷移の timestamp/reason を独立して保存) = metadata table が必要、 MVP は latest wmsg の `created_at` を proxy として返す
