# 34. wire × Echoes Act II — 構造化配送(channel E)と wire 可視化

- **日付**: 2026-07-12(hearing 収束 = 同日の mako × conductor 対話、実装前)
- **branch**: `mako/wire-act2-delivery`
- **status**: 設計確定(Step 0 spike 項目あり、PR 分割確定)
- **前提 doc**: [guide/messaging.md](../guide/messaging.md)(wire 全体像、#729)/ [32-echoes-act2-gui.md](32-echoes-act2-gui.md)(Act II エンジン)/ [33-console-unification.md](33-console-unification.md)(engine 排他スロット)

## 0. TL;DR

Act II(chat)lane への wire 配送は現状 **2 経路とも壊れている**(§2、コードで確定)。修正と同時に、「VP 自身がエンジンの stdin とイベント流を所有する」という Act II の構造を活かし、配送を PTY text 注入(channel C)から**エンジンへの構造化注入(channel E)**へ引き上げる。あわせて wire を GUI に可視化する — **V1(inbox/thread カード、配送非依存)**と **V2(会話統合バブル)**の 2 層。

## 1. Why

- **Act I の配送は text 注入ハック**: delivery_actor が PtySlot に `"📨 wire: ..."` という文字列を打ち、claude が画面の文字を読んで自発的に `wire_recv` を呼ぶのを期待する構造。busy 判定は外側からの `claude agents --json` poll 推定。端末状態依存・10 分 ×3 再掲示・console を汚す。
- **Act II では VP がエンジンを所有**: `EchoesAgentHost` は stdin(JSONL 注入点)と stdout(turn 状態が読めるイベント流)の両方を持つ。wire を「文字で気付かせる」のではなく「ターンとして配送する」ことができ、busy 判定も自前のイベントで内在化できる。
- **そして現状は壊れている**: chat lane 宛の command wire は届かない(§2)。channel E は改善であると同時にバグ修正。

## 2. 現状の確定事実(2026-07-12 コード採取)

1. **delivery_actor は console_mode を見ない**: `pick_nudge_target`(`delivery_actor.rs:134`)は `LaneState::Running` のみで判定。chat lane も nudge 対象になる。
2. **Ready 判定 → nudge は必ず失敗**: `lane_nudge` → `deliver_nudge`(`lanes_state.rs:1015`)→ `write_to_lane`(`lanes_state.rs:781`)が chat lane では `Err("Lane has no PtySlot")`。送出失敗は台帳を進めない設計(`delivery_actor.rs:479`)のため、**30s ごとに無限再試行**して message は届かない(warn ログが積もる)。
3. **Offline 判定 → channel D は session 競合**: bg dispatch(`delivery_actor.rs:398-441`)が `claude -p --resume <cc_session_id>` を detached 起動するが、chat lane の `cc_session_id` は**常駐 EchoesAgentHost が保持中の session そのもの**。同一 session への二重 `--resume` で transcript の fork / interleave が起きうる。**ただし conductor 限定**: registry snapshot への `cc_session_id` lazy populate は `routes/lanes.rs:136` で conductor のみ(syscall 抑制のための消費者限定)。performer chat lane では `None` → `--resume` なしの fresh headless となり、競合の代わりに**文脈喪失**という別の故障になる。
4. **朗報: plumbing は最小で済む**: `LaneInfo.console_mode` は既に SP→World registry へ届いている(`lanes_state.rs:317`、serde default = `Tui` で wire 後方互換)。delivery_actor は field を読むだけで分岐できる。
5. **channel B(hook-check)は Act II でも有効**: SessionStart hook 群は headless でも走る(doc 32 §10.2 の `hook_started` 実測)。未読告知の床は流用可能。
6. **未確定(Step 0 spike)**: `claude agents --json` が headless -p プロセスをどう報告するか(= 現状 chat lane が Ready/Offline どちらに落ちるかの分水嶺だが、channel E 実装後はどちらでも無関係になる)。

## 3. 設計 — channel E(chat lane への構造化配送)

```
delivery_actor(TheWorld)
  console_mode == Tui  → 従来どおり channel C(lane_nudge → PtySlot)/ D(bg dispatch)
  console_mode == Chat → channel E:
      forward_to_sp_control(path_key, "echoes_nudge", {lane, text})
        └─ SP: ensure_chat_engine(lazy spawn。echoes_submit と同経路、unison_server.rs:526)
             └─ host.submit(text)  … nudge 文言をターンとして注入
```

- **nudge 文言は channel C と同一**(wire_recv → 処理 → wire_ack の導線)。headless engine も MCP server を settings から解決するため `mcp__vantage-point__wire_recv` は使える(channel D と同じ前提、実証済の経路)。
- **chat lane を channel C/D から除外** = PTY 無限リトライと session 競合を構造的に排除。
- **台帳は channel C と共有**((message_id, agent) → 回数/時刻。`RENUDGE_AFTER`/`MAX_NUDGES` も同値から開始)。
- **busy の扱い(PR1)**: まず即 submit。turn 実行中の stdin user message を claude が queue するかは Step 0 spike ①で確定し、queue しない/壊れるなら PR3(turn 境界 queue)を PR1 に前倒しする。
- **method 名 `echoes_nudge` は仮**(実装時に unison_server の dispatch 語彙と揃えて確定)。

## 4. 設計 — wire 可視化(2 層)

### V1 — inbox/thread カード(読み取り側、配送非依存)

- vp-app が World の "wire" channel の read-only method(`wire/thread` / `wire/unread-count` 等)を叩き、ChatView(または Console 脇)に **inbox / thread カード + ack ボタン**(`wire/ack`)を描く。
- lane の agent address は LaneInfo から導出(`agent@<project>` / `agent@<project>/<name>`、messaging.md §4 の規約)。
- 配送改修と完全に独立なので**早く出せる**。「wire が見える」体験の初出。

### V2 — 会話統合バブル + ack 追跡

- channel E で注入した wire を、通常の user message と区別して**専用バブル(wire カード)**で描く。
- **origin タグ spike(Step 0 ②)**: stream-json user message に追加 field(例 `origin: {kind: "wire", message_id}`)を載せて transcript jsonl に残るか実測。残れば replay も同じタグで復元できる(transcript replay の origin 基盤に相乗り)。残らなければ host 側の in-memory 対応帳 + EchoesEvent タグ付けで代替(replay 側は妥協)。
- **ack 半自動化**: auto-ack は**しない**(ack は「処理した」の意思表示で、claude が明示するのが正 — messaging.md §1.6 の台帳意味論を守る)。注入 turn の TurnCompleted を観測して「処理済みらしいが未 ack」を GUI で可視化するに留める。

## 5. PR 分割(直列、各 PR 単独で nightly に載る)

| PR | 内容 | exit criteria |
|---|---|---|
| **PR1** | channel E 最小核: delivery_actor の console_mode 分岐 + SP `echoes_nudge` dispatch + chat lane の C/D 除外(**実質バグ修正**) | chat lane 宛 command wire が ChatView の会話にターンとして届き、wire_recv → ack まで回る。PTY エラーの無限リトライが消える |
| **PR2** | 可視化 V1: inbox/thread カード + ack ボタン | GUI から未読確認と ack ができ、sidebar の flow_state(AwaitingUser 等)と整合する |
| **PR3** | turn 境界 queue: host に turn 状態(submit〜TurnCompleted)+ pending queue | turn 実行中に届いた wire が turn 完了後に注入される(spike ①の結果次第で PR1 に前倒し) |
| **PR4** | 可視化 V2: wire 専用バブル(origin タグ)+ 未 ack 可視化 | 注入 wire が user 発話と視覚的に区別され、replay 後も維持される |

- 実装運用は doc 32 §8 と同じ(team-b レビュー → `--base nightly` PR → auto-merge、GitNexus impact/detect_changes、pre-MVP 原則)。
- **Epic 末尾**: `guide/messaging.md`(配送チャネル表に E を追加、§1.7 更新)と `AGENTS.md` の整合 sweep を doc-only PR で。→ **messaging.md 分は `msg-doc-sweep` lane で消化済**(channel C/D/E 分岐を §1.7、Wire Inbox V1 を §4 に反映)。`AGENTS.md` は channel E が agent から透過的(chat lane も wire を turn で受け wire_recv → wire_ack する経路は不変)なため変更不要と確認。

## 6. Step 0 spike リスト(PR1 着手時に実測、結果は本 doc 付録へ)

1. **turn 実行中の stdin user message**: queue される / 破棄される / 割り込む のどれか(隔離環境: `env -u VP_LANE` + cwd=/tmp、doc 32 Step 0 と同作法)。
2. **stream-json user message の追加 field**: transcript jsonl に保存されるか(V2 の origin タグ方式の分水嶺)。
3. **`claude agents --json` と headless -p**: 現状把握のみ(channel E 後は無関係)。
4. **chat lane 宛 wire の実機観察**: §2 の故障 2 経路の log シグネチャ採取(regression テストの根拠)。
5. **performer chat lane の resume 経路確認**: channel E の SP 側(`ensure_chat_engine`)が resume_session_id を SP local の state file から解決するなら registry の conductor 限定 populate(§2-3)に依存しない — これを確認し、依存する場合は populate の performer 拡張を PR1 scope に含めるか判断。

## 7. 未決事項

- `echoes_nudge` の正式名 / nudge 文言の Act II 最適化(GUI では「inbox カードを見よ」の方が自然になる可能性 — V1 との合流点)。
- V1 の接続形: vp-app が既存 "wire" channel を直接 open するか、"lanes" 同様の World 集約 channel を新設するか。
- `needs_user` の GUI relay: 現行規約(conductor が relay して ack)と V1 の ack ボタンの整合。
- **Act II 対話 UI 語彙との合流**: AskUserQuestion 不能問題の調査(別 lane、2026-07-12 dispatch)が「GUI ダイアログ ⇄ エンジン往復」の語彙を定義する場合、V2 の wire バブルと同じ EchoesEvent 拡張面に乗る可能性が高い。実装前に突き合わせる。
