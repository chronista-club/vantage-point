# VP-SPEC-002: Capability / MIDI 仕様

> **Status**: Active
> **Created**: 2025-12-16
> **Updated**: 2026-07-27

---

## Overview

Process が保持する「能力（Capability / Stand）」システムと、MIDI コントローラー連携の仕様。

---

## Capability システム

### 段階的アーキテクチャ

```
Phase 1: トレイト型（現在）— 内部能力を Capability トレイトで整理
Phase 2: プロトコル型（完了）— 能力間 / agent 間通信を wiremsg（wire accumulation）ベースに
Phase 3: プラグイン型（将来）— WASM で能力を動的ロード
```

> **Phase 2 補足**: agent 間メッセージング基盤は数次の再設計を経ている。 当初は in-memory `TopicRouter` ベース → VP-169（doc 19）で `WhitesnakeStore`（SurrealDB embedded primary）→ 最終的に 2026-05 の **wiremsg 再設計（R1〜R6、 PR #406〜#420）** で per-agent cursor の wire accumulation モデルに統一された。 旧 msgbox 実装（`MsgboxStore` / `WhitesnakeStore` / `msgs` table / `MsgboxRegistry`）は全廃済。 現行の能力間 / agent 間メッセージングは wiremsg（`wire_send` / `wire_recv` / `wire_thread`）。 `TopicRouter` 自体は Canvas / pane content の broadcast 配信用途で引き続き存在する（`process/topic_router.rs`）。

### REQ-CAP-001: Capability トレイト

**実装**: `crates/vantage-point/src/capability/core.rs`

- [x] 能力の識別子（name, version）を提供できる
- [x] 能力の初期化・終了処理を定義できる
- [x] イベントの購読・発火ができる（EventBus）
- [x] 非同期処理に対応している

### REQ-CAP-002: CapabilityRegistry

**実装**: `crates/vantage-point/src/capability/registry.rs`

- [x] 能力を名前で登録・検索できる
- [x] 能力の有効/無効を切り替えられる

### REQ-CAP-003: EventBus

**実装**: `crates/vantage-point/src/capability/eventbus.rs`

- [x] 型安全なイベント定義・購読・発火
- [x] broadcast による複数購読者配信
- [x] 非同期対応

### REQ-CAP-004: wiremsg（wire accumulation）

**実装**: `crates/vantage-point/src/capability/wiremsg_store.rs`（store、 TheWorld 上で稼働）+ `process/routes/wire.rs`（TheWorld handlers）+ `process/world_wire.rs`（SP→TheWorld client）、 CLI は `commands/wire.rs`

> **改訂 (2026-05-21)**: 本要件はもともと「msgbox v2（WhitesnakeStore）」 として VP-169 epic（doc 19）の `MsgboxStore` / `WhitesnakeStore` / `msgs` table を指していたが、 2026-05 の **wiremsg 再設計（R1〜R6、 PR #406〜#420）** で msgbox substrate が全廃され、 per-agent cursor の **wire accumulation** モデルに置き換わった。 旧 msgbox 実装（`MsgboxStore` / `WhitesnakeStore` / `msgs` / `msgbox` table / `MsgboxRegistry` / `vp mailbox`）は撤去済。 doc 19 / doc 16-18 は msgbox 設計の historical reference。
>
> **改訂 (2026-06-11、 R2-a)**: wire store を **TheWorld（`db/world/`）に中央化**（設計 memory `mem_1CbvcJj4ppU3QKH9d7xMpT`）。 TheWorld が唯一の writer となり、 SP の wire ハンドラは「アドレス正規化 → TheWorld へ HTTP relay」の proxy に。 これに伴い per-SP store と cross-process forward（`wire_remote`、 旧 R3）は概念ごと撤去（B1/B2 バグの根治）。 local_seq は TheWorld 採番でマシン大域単調。

wiremsg は agent 間メッセージングの substrate。 message は中央 store（TheWorld）の wire に追記され、 受信側は自分の cursor を進めて未読を取得する。

- [x] wire accumulation — message を wire に追記、 per-agent 単一 cursor で未読取得
- [x] threading — `wire_send` の `reply_to` で thread 化、 `wire_thread` で ancestor-chain 取得
- [x] 中央 store — TheWorld が唯一の writer、 SP は proxy（R2-a。 旧 R3 の cross-process forward は撤去）
- [x] ack 台帳 — `wire_ack`（per-message、 cursor 非破壊。 R2-a、 決定 D3）
- [x] delivery loop — 未 ack の `body.category = "command"` を受信者の tmux session に nudge + 再掲示（10min 間隔・max 3 回）。 TheWorld 常駐の `DeliveryActor`（R2-b、 チャネル C。 Phase A 後に native channels へ移行予定）
- [x] activity poll — `claude agents --json` を pulse ごとに poll し、 lane cwd で CC 状態を照合して policy table を精密化: idle / waiting → 即 nudge、 busy → 待つ（idle 遷移で配信）、 session 不在 → pending 保持。 poll 不能時は R2-b の degraded 挙動（Running → nudge）に自動 fallback（R3-a / Phase A、 設計 D4 の LaneActivity 供給）
- [x] session 指名 resume — SessionStart hook が自 session id を lane 単位で記録（`lane::cc_session`、 `vp_state_dir()/cc_sessions/`）し、 echoes の conductor spawn が `claude --resume '<保存 id>'` で同一 session を deterministic に再開（R3-b。 `--continue` の Agent View dashboard 罠を構造的に回避）。 `LaneInfo.cc_session_id` で可視化（lazy read）、 R3-c の `--bg` session 管理に流用予定
- [x] hook 注入 — echoes spawn が `--settings` で SessionStart / UserPromptSubmit hook を注入し、 `vp wire hook-check` が会話境界で未読を additionalContext 通知（R2-c、 チャネル B。 fail-open、 dotfile 非依存 — 決定 D2）
- [x] MCP tool — `wire_send` / `wire_recv` / `wire_inbox` / `wire_thread` / `wire_ack`
- [x] CLI — `vp wire send|recv|inbox|thread|ack|watch`（MCP との取得 primitives parity、 R2-a）
- [x] address モデル — `<actor>@<project>[/<performer>]`（[doc 14](../design/14-wire-address-v3.md)、 canonical = qualified 一本。 bare `"agent"` は SP 入口で正規化、 TheWorld は reject）

---

## MIDI / device 連携

> **改訂 (2026-07-27)**: 旧 `MidiCapability`（REQ-CAP-010）と LPD8 単体定義（REQ-CAP-011）は
> 撤去済 — single-device monitor は消費者不在のまま enumeration 先頭 device を無条件 grab する
> 害だけが残っていた（fleet dogfood で発覚）。現行の device 連携は **Bastet 🧲（World scope の
> multi-device registry）+ Justice 🌫️（Lane scope の双方向 I/O）**。設計 SSOT =
> `design/23-bastet-justice-stand-wiring.md`、実装 = `crates/vantage-point/src/bastet.rs` /
> `justice.rs`。CLI は `vp midi lpd8 write|switch` / `vp midi monitor|ports`。

### REQ-CAP-020: Canvas / TUI 連携

- [ ] MIDI イベントが TopicRouter 経由で配信される（realtime broadcast 用途のため TopicRouter のまま）
- [ ] TUI / Canvas で MIDI 状態を表示できる

> **Note**: 能力間 / agent 間の **メッセージング** は 2026-05 の wiremsg 再設計（R1〜R6）で wire accumulation に統一済（REQ-CAP-004 参照）。 一方、 MIDI / Canvas 状態の realtime broadcast 配信は引き続き `TopicRouter` を用いる（永続化不要・低レイテンシ要求のため）。 「wiremsg = agent 間 messaging」 と「realtime broadcast = TopicRouter」 を混同しないこと。

### REQ-CAP-021: Claude Agent 連携

- [ ] MIDI 入力で PTY にテキスト送信
- [ ] MIDI 入力でチャットキャンセル
- [ ] LED で Agent 状態表示

---

## References

- `archive/02-capability-evolution.md` (VP-DESIGN-002) — 旧進化システム設計（ACT 進化系は 2026-07-27 撤去、 historical reference）
- `design/14-wire-address-v3.md` — wire address モデル（Phase 2 プロトコル型 = wiremsg の address 仕様）
- `design/19-msgbox-whitesnake-primary.md` (VP-169) — 旧 msgbox v2 / WhitesnakeStore 設計（wiremsg 再設計で全廃、 historical reference）
- `crates/vantage-point/src/capability/` — 実装
