# doc 39 — New / Root 切替 / Reset — Lane root agent の一級市民化

> **status**: 設計ドラフト（2026-07-17 hearing、mako + Fable 5。mako レビュー待ち）
> **supersedes**: [doc 38](./38-lane-multi-session.md) の「床には session #1 が化身する」暗黙規約 → root ポインタに一般化（#1 の特別性を撤廃）
> **発端**: 2026-07-17 session chip 凍結の解剖（PR #798）で「✨ New Session が Act I で破壊的 / Act II で追加的」という意味論の非対称が露呈。mako「意味論でいうと New と Replace がある」→「どの session が lane の root agent かが必要。session を id リストから選べて、リストに『新 ID から』があってもいい」→「wire の address が絡むから整合性は外せない」の 3 段 hearing で確定した設計。

## 0. TL;DR

> **lane に `root: SessionKey` を持たせる。root = 床に化身する session であり、同時に
> `agent@<lane>` (wire) が指す「lane の人格」の SSOT。日常操作は New（追加・非破壊）と
> Root 切替（付け替え・非破壊、特例「✨ 新 ID から」）の 2 動詞に収束し、破壊的な
> Reset lane は sidebar の奥へ退避する。**

### メンタルモデル — 「座」と「化身」（mako、dogfood 実感 2026-07-17）

> mako「wire の address って Lane が持ってて、セッションは変わるけど、address は固定で、
> その address にセッションが収まったり、別のセッションと入れ替わったり、っていう
> メンタルモデルが dogfood してていいなと思った」

**address = lane が持つ固定の「座」、session = そこに座る化身**。Act を跨いで不変の座は
**mailbox（`agent@<lane>`）**。もう一つの「器」は Act によって形態が変わる:

| | Act I | Act II |
|---|---|---|
| 器 | **床 = PTY**（doc 38 の「設備」。TUI 形態で化身、常駐） | **PTY 無し**。headless 形態 — claude は **常駐**（EchoesAgentHost、stream-json stdio で複数 turn、host.rs「lane が Act II の間は常駐する」）/ cursor・codex は **turn-scoped**（TurnHost、1 turn = 1 プロセス + resume）。「engine-less（pid=None）が正常形」（doc 33）は初回 submit 前・restart 直後の話で、turn 間で落ちる意味ではない |
| Act 切替 | 同じ会話 id が器を**乗り換える**（「セッションを引き継ぎ中…」overlay の実体 = kill + `--resume` respawn） | 同左 |
| 常駐先 | SP（Star Platinum）の子プロセス — daemon（TheWorld）は registry/routing のみで engine を抱えない | 同左 |

> **claude 常駐の裏取り（2026-07-17、過去ソース非依存で 3 系統検証）**: ①実測 — claude 2.1.205 の
> 1 プロセスに stdin から user message 2 通 → result 2 回・全イベント session_id 同一・stdin EOF で
> exit 0。②公式 docs（agent-sdk/streaming-vs-single-mode）—「long lived process」「Session stays
> alive」。③CHANGELOG — v2.1.163（終了は result でなく stdin close 起点）/ v2.1.208（turn 間も
> stdin を stateful に読む）。単発形 `claude -p "prompt"` のみ 1 result で終了する別モード。

root の定義: **どの session が「lane の器（今の Act が用意する形態）」に化身し、mailbox を
名乗るか**。root ポインタ 1 本が器と mailbox の両方を決めるので、Root 切替 = 器の張り替えと
wire 人格の交代が**原子的に**起き、器と mailbox の化身がズレる状態が構造的に存在しない。

- doc 38 の 3 層（床 = 設備 / session = 実体 / 会話 id = 属性)はそのまま。追加するのは「**今どの session が床に化身しているか**」の永続ポインタ 1 本だけ
- 方針は doc 38 と同じ: **動作確実 > UI**。backend（registry）を唯一の真実源にし、UI は薄い view

## 1. 動詞の意味論（hearing の核）

現状の ✨ New Session は Act によって別の意味に化けていた:

| | 現状 | 問題 |
|---|---|---|
| Act II lane | 新 Draft タブ追加（非破壊） | — |
| Act I lane | `lane_restart(fresh)` = 床 kill + **全 session store 破棄**（破壊的） | 「New」の語感と実体が乖離。誤爆で会話を失う。2 クリック armed で防爆しているだけ |

これを 3 動詞に分解する:

| 動詞 | 実体 | 破壊性 | UI 防爆 |
|------|------|--------|---------|
| **New** | session をリストに追加（engine は現 session を引き継ぎ）して focus。**今いる Act に出す**（§4、mako 2026-07-17）: Act I なら root をその新 session に向けて床を張り替え（= 「✨ 新 ID から」の shorthand、旧 root はタブに残存）、Act II なら新 Draft タブ。床に agent が居ない（/exit 等）などどちらでもない時は Act II | 非破壊 | 不要（1 クリック化） |
| **Root 切替** | `root` ポインタを別 session に向け替え → 床を respawn（対象 session の store で resume） | **非破壊**（旧 root の会話はリストに残る） | 進行中 wire がある時のみ警告（§3-3） |
| └ 特例「✨ 新 ID から」 | 新品 session を作って root に向ける（= 旧 Replace / fresh 相当） | 非破壊 | 同上 |
| **Reset lane** | 全 session store + registry 破棄（現 `clear_fresh_lane_state`） | 破壊的 | sidebar の奥 + 確認。日常フローから退避 |

Root 切替の picker（表示場所 = **ヘッダの cc chip クリック**、2026-07-18 dogfood 後に mako 決定。
chip は既に root session の表示器なので、クリックで dropdown を開く = 表示器と操作器の一致）:

```
┌ Root agent ──────────────────┐
│ ● cc:4dfc8d10  #1  今の床     │
│ ○ cdx:0199a2   #2  codex     │
│ ○ cc:3d91933b  #3  昨日の続き │
│ ──────────────────────────── │
│ ✨ 新 ID から（素の engine）   │
└──────────────────────────────┘
```

## 2. モデル変更 — root ポインタ

```
Lane
 ├─ sessions: [#1, #2, #3, ...]   ← 会話のリスト（doc 38。変更なし）
 ├─ focused: #m                    ← Act II でどれを見ているか（doc 38。変更なし）
 └─ root: #k                       ← ★新設: どれが lane の器（Act I=床 / Act II=headless）に化身し、agent@<lane> を名乗るか
```

- `session_registry` に `root: SessionKey` を永続（serde default = 1 で wire/file 後方互換。
  既存 registry は「root=1」として読める = N=1 の特殊ケース温存、doc 38 Phase 1 と同じ手筋）
- 床 spawn（`stand_spawner::build_stand_command` の resume type-ahead）は「lane label 固定
  （= 実質 #1）」でなく「**root session の label**」の store を読む
- doc 38 ⚠️「#1 close は Act I 床 resume を断つ」は root の一般化で構造ごと解消
  （#1 に特別性が無くなる。root が指す session は close 不可、に置き換わる）

## 3. Wire 整合の不変条件（外せない、mako 明示）

wire address は **lane 粒度**（`agent@<project>` / `agent@<project>/<performer>`）。
その配送実体 = 「lane の代表 session」であり、root はこの解決の SSOT に昇格する。

### 3-1. wire 配送は常に root に解決する

3 経路すべて:

| 経路 | 現状の読み先 | 本 doc 後 |
|------|-------------|-----------|
| channel D（delivery_actor `claude -p --resume <id>`） | `LaneInfo.cc_session_id`（lane label 固定） | **root session の store** |
| `lane_nudge`（床 type-ahead 注入） | 床（= 従来 #1 が化身） | 床（= root が化身）— 自然に整合 |
| `echoes_nudge`（chat engine 注入） | lane の engine | **root session の engine** |

`LaneInfo.cc_session_id` の「claude 専用の契約」doc も「root session の store を読む」に再定義する。

### 3-2. wire は lane 間のもの — lane 内には持ち込まない

`agent@project/lane#2` のような session 粒度 address は導入しない。**1 lane = 1 人格**
（wire 的同一性）を維持する。さらに強く: **wire は lane 間（conductor ↔ performer）の
通信手段であり、lane 内（session 間）の連携には使わない**（mako 2026-07-17「Lane間 wire は
使うけど、Lane内ではまだ使うケースが見えない」）。

- mailbox を読むのは root のみ。**非 root session は wire 的に沈黙**（inbox surface も
  nudge 注入も対象外）— session 単位の wire 機構は作らない（P1〜P3 の scope 外）
- 並列の人格が必要なら performer を増やすのが VP の答え（conductor × performer
  orchestration の既存路線）。session 間連携の需要が将来実感されたら、その時に wire 以外も
  含めて設計する（今は予約しない）

### 3-3. Root 切替 = 人格の交代

進行中の wire thread（未 ack の command 等）は lane 宛てなので**新 root が引き継ぐ**。

- 引き継ぎの実働は既存機構がそのまま効く: **hook-check がターン頭に未読 wire を surface する**
  ため、新 root の初回発話で inbox が自然に拾われる（追加実装ほぼ不要）
- 未 ack command が残っている状態での切替時のみ、picker に
  「⚠ 未処理 wire n 件を新 root が引き継ぎます」を表示（防爆はここに残る）

### 3-4. cc_session pointer 記録との整合

UserPromptSubmit hook（#795）は「実際に会話した session」の store を書く。root 切替後の
床での発話は新 root の store に記録される。PR #798 の変化 push（`lane/session-changed` →
`Diff::Update`）もそのまま root の chip 更新に乗る — 本 doc は #795/#798 の上に素直に積める。

> ⚠️ **2026-07-18 dogfood で不成立と判明**: hook の書き込み鍵は env `VP_LANE`（素の lane
> label）のままで root の session label（`conductor#2`）に追従しない — root≥2 の lane では
> 書き手と読み手のラベルが乖離し、chip 恒久空白 + 床 resume の `--continue` 劣化を起こす
> （doc 40 §1 に解剖）。root-cause fix = 会話 id の SSOT を registry に統合する doc 40 PR-1。

## 4. Act I / Act II での見え方 — New は「今いる Act に出す」

- **Act I**: 床 = root。header chip は root session の会話 id（`refresh_engine_session_id` の
  読み先を focused → **root** に変更。Act I は「lane の人格」を見る場所）
- **Act II**: タブ = sessions、表示 = focused（変更なし）。root のタブに 👑 等の badge（仮置き）
- **✨ New の出し先**（mako 2026-07-17「act i なら act i、act ii なら act ii。これ以外
  （/exit で抜けてる場合とか）なら act-ii がいい」）:

  | 状況 | 挙動 |
  |------|------|
  | Act I 表示中（床に agent あり） | 新 session 作成 + **root をそれに向ける**（床張り替え、原子的）→ Act I のまま。旧 root の会話はタブに残存 = Root 切替「✨ 新 ID から」の shorthand |
  | Act II 表示中 | 新 Draft タブ + focus（床は不変、現状どおり） |
  | どちらでもない（/exit で床の agent 不在等） | Act II の新 Draft タブに出す（張り替える相手が床に居ないため） |

  Act I の New は床の現 agent を kill する（会話は resume 可能なタブとして残るので**非破壊**）。
  床が実行中 turn の最中なら中断になる点だけ留意 — 防爆を足すかは dogfood で判断（仮: 不要）

### 4-1. Session ID の表示は「発行時点で即・engine/Act 問わず」（mako 2026-07-18 dogfood 決定）

**不変条件**: header chip は「今その lane が抱える **live session id**」を、**session id が発行された瞬間**に映す。発話（初回 UserPromptSubmit）を待たない。engine（cc / codex / cursor）も Act（I / II）も問わない共通ルール。koan =「なるべく正しい session id を、常に出す」。

- 現状の点灯は #795 の UserPromptSubmit hook（発話契機）に相乗りしており、Act I の生 TUI は
  boot 時の捕捉経路を持たない（2026-07-18 実機で確認: New root 03:18 → 記録は初回発話 03:23）
- **実現機構は doc 40 に一本化**（会話 id の SSOT を session registry に統合し、SessionStart で
  eager 報告 + F1/F2 guard を SP の policy 1 箇所に移設）。同解剖で「hook の書き込みラベルが
  root に追従しない」実バグ 2 段（chip 恒久空白 / 床 resume の `--continue` 劣化）も確定し、
  本節の遅延問題と同根で doc 40 が root-cause fix する
- **読み先は §4 のまま**（Act I = root / Act II = focused）。本節は「いつ映すか（発話 → 発行）」だけを前倒しする差分で、「何を映すか」は不変。

## 5. Phase 分割

| Phase | 内容 | 備考 |
|-------|------|------|
| **P1** | registry に `root` 追加（default 1）+ 床 spawn / wire 3 経路 / chip enrich の読み先を root に統一 | 挙動は N=1 で完全互換（中間状態を作らない） |
| **P2** | ✨ New の意味論統一(Act 不問で session 追加 + Act II 切替、armed 撤去) + Reset lane を sidebar へ退避 | 旧 tui fresh 経路は Reset に移る |
| **P2.5** | 会話 id SSOT 統合（registry 一枚岩 + 書き手漏斗 + eager 表示 + ラベル乖離バグ根治） | **doc 40 に昇格**（表示層だけの patch では §3-4 ⚠️ のバグが残るため構造ごと） |
| **P3** | Root 切替 picker（リスト + 「✨ 新 ID から」+ wire 引き継ぎ警告） | 表示場所 = ヘッダ chip click（2026-07-18 決定、§1） |
| **P4** | engine gating（床 resume 可否を実測して picker に反映）+ **respawn の stand を root session に追従させる**（現状 `restart_lane` は lane 固定 stand で spawn するため、P3 は同 engine のみ許可のガードで回避 — moody 指摘 2026-07-18） | doc 37 の engine 実測系譜 |

## 6. 既知の考慮点

- **engine 差**: 床 resume は claude（`--resume`）実証済。cursor Act I は PR #773 で console 実働
  だが resume 形は要実測。codex / agy は会話 id が無い/取れないケースがある（doc 38 §1.1）→
  root 候補にできるのは「resume 可能な session」のみ、picker で gating（P4）
- **root session の削除**: 不可（「最後の 1 本は取り除けない」の既存不変条件を「root は取り除け
  ない」に拡張。root を移してから削除）
- **#798 との関係**: 本 doc は #798（session id 変化の push 経路）と #795（pointer 記録の
  UserPromptSubmit 化）を前提に積む。root 切替時も `emit_lane_update` を撃てば chip が追従する
- **表示 eager 化と F1/F2 guard の両立**（§4-1 → doc 40 §6）: 「UserPromptSubmit のみ記録」は
  F1/F2（resume 失敗 `|| claude` fallback の幻 session が pointer 上書き）を潰す**鈍器**だった。
  doc 40 は SP の policy 1 箇所に置き換える — SessionStart は「既存 conversation の transcript が
  実在する時だけ据え置き」の精密 guard 付きで記録可（New root の fresh 発番 / resume 成功
  no-op は即記録 = chip が boot で点く）。旧鈍器を無条件 SessionStart 記録に戻すのは依然禁止
- **将来素材 — 会話の分岐**: claude には `--fork-session`（resume 時に旧 id を汚さず新 session id を
  切る）が公式にある（2026-07-17 検証で確認）。「既存会話から分岐して root にする」を作る時は
  これが土台になる（本 doc の scope 外、P4 以降の素材として記録のみ）
- **codex の turn-scoped は VP 側の現状であって CLI の限界ではない**（2026-07-18 openai/codex
  ソース確認）: `codex app-server` は常駐 + JSON-RPC の会話 API を持つ — `thread/start・resume・
  fork` / `turn/start・steer（実行中 turn への注入、claude に無い）・interrupt`
  （`app-server-protocol/src/protocol/common.rs`）。常駐化の正攻法は app-server 統合
  （follow-up 起票済 mem_1Cd5Msoj）。cursor は `--input-format` 相当が無く（出力のみ
  stream-json）、turn-scoped が CLI 側の制約 — TurnHost は cursor 用として残る

## 7. Engine 常駐統合の優先度（2026-07-18 mako 決定）

「いつでも入出力できる常駐型の方が VP のオーケストレーション（wire nudge 即注入 / interrupt
即時性 / turn 固定費 / HITL control 面）に合う」を軸に再整理した確定順:

| 優先 | engine | 方式 | 根拠・条件 |
|---|---|---|---|
| — | claude | stream-json 常駐（出荷済） | 基準器 |
| **1** | codex | **app-server 常駐** | mako「turn じゃない方で」確定。thread/turn API（steer/fork 含む）をソース確認済 |
| **2** | grok | **ACP 常駐**（新規 engine） | xai-org/grok-build ソース確認: ACP がネイティブ中核（TUI 自身が ACP client）、`xai-acp-lib` に session/new・prompt + load 機構。VP に `protocol/acp.rs` の下地あり。旧「ACP 不採用」は claude 専用統合の判断であり、grok では ACP こそが専用統合 |
| — | cursor | **撤去済み**（2026-07-18 sweep 6.5、再導入時は新規実装 — 旧実装は git history #773/#776） | Composer 2.5 は魅力（`composer-2.5` / `-fast` を CLI 実物確認）だが、cursor-agent に入力 stream が無く turn-scoped しか組めない = 常駐一枚岩の方針に反する。**CLI が入力 stream / ACP を積んだら Composer 枠として再検討**（将来素材） |
| — | agy | **撤去済み**（2026-07-18 sweep 6.5、再導入時は新規実装 — 旧実装は git history #773/#776） | 会話 id 供給なし・常駐路なし |

**帰結 — 対応 engine は常駐型のみ（claude / codex / grok）の一枚岩**:
- codex app-server と grok ACP は共に「常駐 JSON-RPC over stdio + typed protocol」— 常駐系の
  共通ホスト骨格（RpcHost 相当）を 1 度作れば両方に効く
- codex の app-server 移行が完了した時点で **TurnHost 系（turn_host / cursor_host /
  codex_host / cursor_translate / codex_translate）は全 engine から不要になり丸ごと撤去できる**
  （pre-MVP 方針: 中間状態・dead code を残さない）。cursor/agy のコード撤去はこれに束ねる

### 7-1. Local LLM の裏打ち（2026-07-18 Web 調査、全て一次資料確認済み）

常駐 3 経路すべてが local model で裏打ち可能。統合コスト順:

| route | 手段 | VP 側コスト |
|---|---|---|
| **A1** | claude engine × **LM Studio の Anthropic 互換 `/v1/messages`**（0.4.1+ ネイティブ、proxy 不要）。`ANTHROPIC_BASE_URL=http://localhost:1234` + `ANTHROPIC_AUTH_TOKEN=lmstudio` — **両ベンダー公式手順**（lmstudio.ai/blog/claudecode / code.claude.com/docs/en/llm-gateway-connect） | spawn_env に env 2 個 |
| **A2** | codex × `--oss` / `oss_provider = "lmstudio"`（公式 first-class、`/v1/responses` を叩く。repo の `lmstudio` crate が疎通確認 + model 自動 DL/load を担う） | config.toml 1 行。⚠️ app-server × oss の組合せのみ未文書化 → 統合時に実測 |
| **B** | **OpenCode**（anomalyco/opencode、活発）を AcpHost に挿す — `opencode acp`（stdio ACP）+ LM Studio/Ollama/llama.cpp の公式 provider 対応。grok 統合の副産物として新 binary 追加のみで local 常駐 engine が増える | AcpHost 完成後ほぼゼロ |
| C | VP 純正 engine（SP 内 agent loop、tool runtime が本体）。α: tool 無し chat → β: read-only tool → γ: 編集系。provider の口を modality 非依存に切る（音系/姿勢推定の種 = creo mem_1Cd7rpDkgeNDTW5nX1qcu6） | 大（長期候補） |
| — | **Bionic**（LM Studio 純正 agent GUI、2026-07-16 preview）は API/CLI/ACP surface 未公開で現状口なし — server surface が生えたら再評価の watch | — |
