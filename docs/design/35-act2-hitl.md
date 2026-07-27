> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# 35. Echoes Act II — HITL 面（control protocol client + PromptCard）

- **日付**: 2026-07-13（設計、実装前。決定 spike 実測済み）
- **branch**: `mako/hitl-doc35`
- **status**: 設計確定（決定 spike で質問経路を裁定済み、PR 分割確定）
- **前提 doc**: [32-echoes-act2-gui.md](32-echoes-act2-gui.md)（Act II エンジン・EchoesEvent 語彙）/ [33-console-unification.md](33-console-unification.md)（engine 排他スロット）/ [34-wire-act2-delivery.md](34-wire-act2-delivery.md)（§7 に本 doc との合流点が予約済み）
- **調査 SSOT**: creo-memories `mem_1Ccvpe7Gwmf5f5X43ZnVBC`（Act II 対話 UI ギャップ調査、2026-07-12）。一次資料は `mako/act2-ui-gap` lane の `.vp-scratch/act2-ui-gap/`（REPORT ×2 + spike jsonl）

## 0. TL;DR

Act II（構造化会話 GUI）は「読む」までは出来たが、**HITL（human-in-the-loop）面が「質問・permission・plan 承認・中断」の 4 つとも欠けている**。本 doc はこれを **1 本のレール = control protocol client** に束ねる。エンジン（headless claude）の stdin/stdout に既に生きている双方向 control protocol を `EchoesAgentHost` に実装し、逆方向 `can_use_tool` を GUI の **単一 PromptCard** に写像、応答を `control_response` で書き戻す。

**決定 spike（§8）で両調査の食い違いを裁定**: `--permission-prompt-tool stdio` + initialize handshake を渡すと、native **AskUserQuestion が headless の `tools[]` に現れ**（bare init では出ない）、`can_use_tool` control_request として発火し、`control_response{behavior:"allow", updatedInput:{questions, answers}}` で turn が正しく継続する（実測 ×3）。→ **質問経路 = native AskUserQuestion の横取り**（VP MCP `ask_user` の新設は不要）。同じ flag で **ExitPlanMode も tools[] に載る** ため plan 承認も同じレールに乗る。さらに **`bypassPermissions` + stdio で「tool 素通し parity ＋ 質問だけ routing」が両立**するため、既定は現状の bypass を維持したまま質問だけ GUI へ流せる。

## 1. Why — 4 つの HITL 面が全欠如、1 本のレールで束ねる

現行 `EchoesAgentHost` は **素の pipe**（`host.rs:118`）。stdin に user message JSON を書き（`host.rs:284` `submit`）、stdout を `EchoesEvent` に翻訳して broadcast するだけ（`host.rs:207`）で、**control protocol の handshake を一切行わない**。この結果、Act II には人間が engine に介入する面が構造的に無い:

| HITL 面 | Act I（TUI） | Act II（現状） | 実害 |
|---|---|---|---|
| **① 質問**（clarifying） | TUI が AskUserQuestion を native 描画 | tools から除去され不能。model は prose に劣化し次 user message を待つ（`spikeC`） | 構造化選択 UI が無い。確認したい局面で確認できず、model が勝手に進む一歩手前 |
| **② turn 中断**（Esc 相当） | Esc で停止 | **手段皆無** | 暴走・誤走ターンを止められない |
| **③ permission**（tool 承認） | bypassPermissions（Act I も素通し） | bypassPermissions（同上） | 承認したい局面で承認できない（両 Act 欠如の新機能） |
| **④ plan 承認** | plan mode 使用可 | 未対応 | plan を提示させて承認するフローが無い |

これら 4 つは**すべて control protocol で供給される**（§3 の spike で実測）: 質問と permission は逆方向 `can_use_tool`、中断は `interrupt`、plan は `set_permission_mode` + ExitPlanMode。個別機構で解くと語彙が割れるため、**control protocol client を 1 本実装して 4 面を一括で開く**。これは doc 32 §5 の「permission ダイアログを入れる時に control protocol ごと実装する」方針、doc 33 §9 の予告と一致する。

> **方針の裁定**: 調査 2 本は推奨経路が食い違った（REPORT-1 = control protocol / canUseTool、REPORT-2 = VP MCP `ask_user`）。両者とも「capability 宣言つき initialize」に未到達だったため、本 doc の決定 spike（§8）で決着させた。**結論は control protocol 経路**。VP MCP `ask_user` は AskUserQuestion の 1〜4 質問 × 2〜4 択の型を超える構造化フォーム/ウィザードが要る時の**補完**に限定し、primary にはしない（native の暴発を止められず model と戦う設計になるため）。

## 2. EchoesAgentHost への control protocol client 統合

### 2.1 現状の spawn と、追加する 1 flag

現行 spawn（`host.rs:143-163`）は Act II 駆動形 + `--permission-mode bypassPermissions`（`host.rs:155`、Act I との tool 素通し parity）。ここに **`--permission-prompt-tool stdio` を 1 つ足す**のが点火条件（§8 で実測）:

```
claude -p --input-format stream-json --output-format stream-json
  --include-partial-messages --verbose
  --permission-mode bypassPermissions        ← 既定は維持（tool は素通し）
  --permission-prompt-tool stdio             ← ★ 追加: 質問/承認を stdio control channel に routing
  [--model <model>] [--resume <cc_session id>]
```

- **`--permission-prompt-tool stdio` は claude 2.1.197 の `--help` に出ない hidden flag**だが受理・機能する（§8）。調査 §10.1 の「`--permission-prompt-tool` は削除済み」は `--help` の非表示から推論した誤りで、本 spike が訂正する（表示は消えたが flag は生存）。
- **bypassPermissions のまま stdio を足すと、通常 tool（Write/Bash）は素通しのまま AskUserQuestion だけが `can_use_tool` として routing される**（§8 run3 実測）。permission 洪水を避けつつ質問だけ拾える = 既定を現状維持できる。tool 承認（③）が欲しい時だけ `set_permission_mode("default")` で opt-in する（§2.4）。

### 2.2 handshake と双方向の control 面

spawn 直後に initialize handshake を送り、control channel を確立する（SDK と同形、`hooks` は空で可）:

```
我々→claude: {"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","hooks":null}}
claude→我々: {"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{commands,agents,models,…}}}
```

以降、control 面は 2 方向で流れる:

| 方向 | frame | stream | 用途 |
|---|---|---|---|
| **我々→claude**（能動） | `control_request{subtype:"initialize"}` | stdin | handshake（spawn 時 1 回） |
| **我々→claude**（能動） | `control_request{subtype:"interrupt"}` | stdin | turn 中断（②、`interrupt()`） |
| **我々→claude**（能動） | `control_request{subtype:"set_permission_mode", mode}` | stdin | mode 動的切替（③④） |
| **claude→我々**（受動） | `control_request{subtype:"can_use_tool", tool_name, input, tool_use_id}` | **stdout** | 質問（①）/ 承認（③）/ plan（④） |
| **claude→我々**（応答） | `control_response{subtype:"success", request_id, response:{behavior, updatedInput}}` | stdin | 上記への回答 |

**要点: 逆方向 `can_use_tool` は stdout（EchoesEvent と同じ stream）で来て、応答は stdin（control_response）で戻す。** これが「stdin は user message と control 面の 2 種になる」の実体。

### 2.3 stdin 排他 — 既存 Mutex に control_response を同居させる

stdin は既に `tokio::sync::Mutex<ChildStdin>`（`host.rs:120`）で持たれ、`submit` が lock して 1 行 write + flush する（`host.rs:286-289`）。**control frame（interrupt / set_permission_mode / control_response）も同じ Mutex を経由して書く**。各 write は「1 JSON 行 + `\n` + flush」で完結し Mutex が直列化するので、user message と control frame が混線しない（調査 §4 が flag した「submit と control_response が stdin を奪い合わない」の担保）。

追加する host API（いずれも `&self` — LanePool read lock 下から呼べる、`submit` と同じ理由）:

```rust
// 逆方向 can_use_tool への回答（request_id は EchoesEvent 経由で GUI から戻る）
pub async fn respond_permission(&self, request_id: &str, decision: PermissionDecision) -> Result<()>;
// 能動 control（我々→claude）。request_id を自前採番し control_response を待つ
pub async fn interrupt(&self) -> Result<()>;
pub async fn set_permission_mode(&self, mode: &str) -> Result<()>;
```

- `respond_permission` は `control_response{subtype:"success", request_id, response:{behavior:"allow", updatedInput}}` を組み立てて stdin に書くだけ（純粋な action）。`updatedInput` は AskUserQuestion なら `{questions, answers}`、tool 承認なら原 input（or sanitize 済み）。deny は `{behavior:"deny", message}`。
- `interrupt` / `set_permission_mode` は我々が能動で送る control_request。`request_id` を採番して送り、対応する `control_response` の到着で完了を確認する（応答の request_id マッチング）。

### 2.4 逆方向 can_use_tool の受信 — stdout ポンプの分岐

現行 stdout ポンプ（`host.rs:207-236`）は各行を `EchoesTranslator::ingest` に通して `EchoesEvent` を broadcast する。ここに **control_request の検出を前置**する:

```
stdout 1 行:
  type == "control_request" && subtype == "can_use_tool"
     → tool_name で分岐して EchoesEvent へ翻訳:
         "AskUserQuestion"  → EchoesEvent::Question{ request_id, questions }
         その他（Write/Bash…） → EchoesEvent::PermissionRequest{ request_id, tool_name, input }
     （ExitPlanMode / plan clarifying は §4 で扱い分け）
  type == "control_response"（我々の能動 control への応答）
     → 対応する pending request を解決（EchoesEvent には出さない）
  それ以外
     → 従来どおり EchoesTranslator（EchoesEvent::MessageChunk 等）
```

- **`request_id` を EchoesEvent に載せる**のが肝。GUI は選択後にこの id を IPC で戻し、host が `respond_permission(request_id, …)` で control_response を書く。id が「どの pending 質問への回答か」を結ぶ。
- `EchoesTranslator` は未知 type を `serde(other)` で握り潰す（doc 32 §10.3）ので、control frame を translator に流しても実害は無いが、**制御面は translator の手前で抜く**（EchoesEvent 語彙を engine 制御で汚さない、translate.rs の責務分離を保つ）。
- **待ちの寿命**: `can_use_tool` は callback が返るまで engine の turn が pause する（＝人間の判断を待つ間 engine が自然に停止する、これが HITL のあるべき挙動）。GUI 応答まで無期限に待てる。engine が待機中に死んだ場合は既存の Error 経路（`host.rs:232`）で GUI に途絶が出る。

### 2.5 permission mode の遷移方針

- **既定は現状維持 = `bypassPermissions`**（`host.rs:155`）。§8 run3 のとおり、bypass + stdio で tool は素通し・AskUserQuestion だけ routing されるので、**質問（①）と中断（②）は既定のまま効く**。
- **tool 承認（③）と plan（④）は opt-in**。GUI から `set_permission_mode("default")`（承認を全 tool で要求）/ `"plan"`（plan mode）に切替えた時だけ、通常 tool の `can_use_tool` が飛ぶ。切替は spawn 時固定ではなく実行時に動的（spikeB で実測、§8 参照）。
- mode の現在値は SessionInit の `permission_mode`（`event.rs:26`）で GUI に既に届いている。切替 UI はそれを表示・変更する。

## 3. EchoesEvent の additive 拡張

`EchoesEvent`（`event.rs:18`、PR1 で凍結・`#[serde(tag="kind")]`）に **2 variant を additive 追加**する。既存 variant は不変（凍結語彙原則）。`event.rs:99-100` の「permission_request は MVP 非対象、将来 control protocol ごと実装」の布石コメントを、本 doc が回収する:

```rust
/// clarifying question（AskUserQuestion の can_use_tool 横取り）。GUI は PromptCard(選択肢)で描く。
Question {
    /// control_response の request_id マッチング用（回答時に GUI から戻す）。
    request_id: String,
    /// AskUserQuestion input の questions（1〜4 質問 × 2〜4 択、multiSelect 含む）。
    questions: Vec<QuestionSpec>,
},
/// tool 承認要求（permission-mode=default 時の can_use_tool）。GUI は PromptCard(allow/deny)で描く。
PermissionRequest {
    request_id: String,
    tool_name: String,
    input: serde_json::Value,
},
```

- `QuestionSpec` は spike で採取した実 input 形をそのまま型化: `{ question: String, header: String, options: Vec<{label, description}>, multi_select: bool }`（§8 の生 wire）。
- **応答 submit 面**: GUI → SP の新 IPC/method（§4）。`Question` の回答は `{request_id, answers}`、`PermissionRequest` は `{request_id, behavior, message?}`。
- **doc 34（wire バブル PR4）との非衝突**: doc 34 は EchoesEvent に wire 専用バブルを additive 追加する（同じ enum・別 kind）。本 doc の `Question`/`PermissionRequest` とは **kind が異なるので衝突しない**。ただし戻りの transport は構造的に別（doc 34 の wire は user message 注入 = `submit` 経由 / 本 doc の質問は control_response 経由）。**表示語彙（PromptCard）は共有、戻り transport は分離**（調査 §7 の結論）。EchoesEvent の HITL/wire バブル種別を 1 箇所（event.rs）で持つことで語彙分裂を防ぐ。

## 4. GUI — 単一 PromptCard

ChatView（`chatview.tsx`、586 行）の `foldInto` switch（`chatview.tsx:64`）に case を追加し、`ChatItem` union（`chatview.tsx:23`）に **1 つの `prompt` variant** を足す。種別（question / permission / plan）で装飾だけ替える単一コンポーネント:

| 種別 | 由来 EchoesEvent | UI | 応答 |
|---|---|---|---|
| **質問** | `Question{request_id, questions}` | 各 question を見出し + 選択肢ボタン（multiSelect は複数選択 + 確定ボタン） | 選んだ label を `answers` に詰め `echoes:respond {request_id, answers}` |
| **permission** | `PermissionRequest{request_id, tool_name, input}` | tool 名 + input 要約 + allow/deny ボタン | `echoes:respond {request_id, behavior}` |
| **plan** | `PermissionRequest`（tool=ExitPlanMode）| plan 本文 + 承認/却下 | 同上（承認 = mode を default/acceptEdits へ） |

- **submit 面の兄弟**: 現行 `submit()`（`chatview.tsx:274`）は IPC `echoes:submit {lane, prompt}` を送る（`chatview.tsx:282`）。回答は兄弟 IPC `echoes:respond {lane, request_id, …}` → SP の新 dispatch method（`unison_server.rs:961` の table に追加、`handle_echoes_submit` `unison_server.rs:506` と同型）→ `host.respond_permission`。
- **既存 awaiting_input / conn-hitl への接続**: `Question` / `PermissionRequest` 到着で当該 lane の `awaiting_input[lane]=true`（`SidebarState.ts:67`）を立てる。これで sidebar の `conn-hitl`（magenta diamond の pulse、`Shell.tsx:348`、data source は `awaiting_input`、`ProjectAccordion.tsx:47`）が **Act I（OSC99 由来）と同じ機構で点灯**する。回答 or TurnCompleted で false に戻す。「engine が人を待っている」signal を新規に作らず再利用する。
- **motion**（doc 32 §7）: PromptCard の出現は状態変化の伝達。到着でカードが settle、応答で送信フィードバック → 折りたたみ。`prefers-reduced-motion` 尊重。

## 5. turn 中断（interrupt）

②は control `interrupt` で実装（spikeB で実測済み = `sleep 25` を 4s で中断、`result.subtype=error_during_execution`。§8 参照）。設計は薄い:

- host `interrupt()`（§2.3）を新 IPC `echoes:interrupt {lane}` → SP dispatch → 呼ぶだけ。
- ChatView に**停止ボタン**（送信ボタン `chatview.tsx:451` の隣、turn 実行中のみ活性）。キーは Esc 相当（pane-level の `onDocKey`（`chatview.tsx:335`）に Esc を追加。作文中の textarea では抑制 — 既存 Home/End の textarea 抑制（`:340`）と同じ扱い。Escape の既存 handler は無いので新設）。
- 中断後、engine は次 submit を受けられる（turn は終わるが engine プロセスは生存）。

### 5.1 type-ahead バッファリング（送信と応答の処理順を保つ）

②の裏面。走行中 turn に対して user が**先に2通目を打って送る**（type-ahead）ときの順序整合を定める。

#### 問題 — 構造的な順序破れ

現状 `submit()`（`chatview.tsx:534`）は streaming 中でも user バブルを optimistic に `items[]` へ即 push する。`message_chunk` の畳み込み（`:107`）は「末尾が assistant なら append / 違えば新バブル」だけで **turn 境界を持たない**ため、走行中に2通目を送ると三重に壊れる:

```
[user1][assistant A前半]                 末尾=assistant（生成中）
 └送信→ [user1][assistant A前半][user2]   optimistic push で末尾が user に変わる
 └turn A の続き→ 末尾=user なので新バブル … [user2][assistant A後半]   ①②
 └turn 完了は封印しない→ turn B の chunk: 末尾=assistant に append … [assistant A後半＋B]  ③
```

- ① user2 が turn A 完了前に割り込む
- ② turn A の残りが user2 への応答に見える（中身は user1 への応答）
- ③ 別 turn の応答が1バブルに融合（`turn_completed` が末尾バブルを封印しないため）

#### 不変条件

> **表示順序 = engine が実際に処理した順序（= transcript の順序）**

engine の transcript（cc_session）が処理順の正本。live の optimistic 経路はこれを**追い越してはならない**。replay 経路（`replay_start` → `transcript ++ in-flight tail`、`chatview.tsx:88`）は既にこの順序で再構築するので、live もここへ収束させれば reconnect で履歴が並び替わらない（live / replay の冪等収束が保証される）。

原理を一行で言うと: **optimistic 描画が許されるのは engine が idle のときだけ**（送信順 = 処理順）。streaming 中の送信は buffer し、turn が閉じてから流す。

#### 設計（3点）

1. **turn 境界の封印**: `turn_completed` / `error` で末尾 assistant item に `sealed` を立て、次 turn の chunk は必ず新バブルから始める（融合③の根治 + reconnect 等の他経路の保険）。
2. **streaming 中の送信は buffer**: `submit()` は streaming 中なら engine へ送らず per-lane store の `pending` に退避し、`items[]` を触らない（①②の芽を断つ）。
3. **turn 閉時に flush**: engine が turn を閉じた瞬間に pending を「user バブル push + `echoes:submit`」で流す。

#### flush トリガは "state" ではなく "event"

flush は `turn_completed` / `error` **イベントの受信**を契機にする（`foldEvent` 入口、`chatview.tsx:181`）。派生状態 `streaming===false` を見てはならない — false になる契機は他に2つあり、どちらも flush してはいけない:

| streaming=false になる契機 | flush? | 理由 |
|---|---|---|
| `turn_completed` / `error` | ✅ する | turn が処理し切って ball が user に戻った |
| `question` / `permission_request`（HITL pause、`:158`/`:168`） | ❌ しない | turn A を処理中（回答待ちで中断）。ここで流すと PromptCard への回答と stdin で混線 |
| `replay_start`（reconnect / demand replay、`:94`） | ❌ しない | 再同期であって turn 完了ではない。流すと走行中 turn を追い越し = 順序破れ |

イベント契機にすれば、この3ケースは構造的に弁別される（HITL / replay は turn-close イベントではない = トリガ集合に入らない）。

#### 端ケース

- **interrupt（停止）**: 中断は user 起点の turn close で、stream には `error`（`error_during_execution`, §8）/ `turn_completed` として現れる（§5）。よって**通常完了と同じ扱いで pending を flush** する（「止めて、代わりにこれ」= redirect が最頻の意図）。単一ルール（*turn が閉じたら流す*）を保つ。dogfood で「止めるだけで送りたくない」が優勢なら interrupt 時のみ hold に倒せる（reversible）。
- **flush の対象 lane**: `foldEvent` の `lane` 引数を対象にする（`activeLane()` ではない）。背面 lane の turn 完了はその lane 自身の pending を流す。
- **buffer は単一 draft**: 「エディタ上でバッファ」= textarea 1枠に対応。streaming 中の追記は同じ `pending` に積み上がり（改行で連結）、turn 閉で **1メッセージ = 1 turn** として流す。複数を別 turn に割る queue 化は将来拡張。

## 6. UI ギャップ 10 項目との対応

調査 SSOT の 10 項目（優先度付き）に対する本 doc のカバー範囲:

| # | UI | 優先度 | 本 doc | PR |
|---|---|---|---|---|
| ① | 質問ダイアログ（AskUserQuestion） | 高 | ✅ **本命**（native 横取り、§2-4） | PR1 |
| ② | turn 中断（interrupt） | 高 | ✅ カバー（§5） | PR2 |
| ③ | permission prompt（tool 承認） | 中 | ✅ カバー（§2.5 opt-in、§4） | PR3 |
| ④ | plan 承認 + mode 切替 | 中 | ✅ カバー（ExitPlanMode も stdio で点灯、§8） | PR4 |
| ③' | engine 途絶可視化 / 再起動 | 高 | ⏸ **defer**（v0.35.2 で Error broadcast は出荷済 `host.rs:232`。再起動ボタンは別 doc） | — |
| ④' | 通知（TurnCompleted→OscNotification） | 高 | ⏸ **defer**（`echoes-act2-notification-signal` で実装済） | — |
| ⑤ | slash command palette | 中 | ⏸ defer（init の `slash_commands[]` は取得済 `event.rs:34`、UI は別 PR） | — |
| ⑥ | @-mention / 画像入力 | 中 | ⏸ defer（doc 32 PR4） | — |
| ⑦ | context 手動操作（/compact） | 低 | ⏸ defer（slash palette に相乗り） | — |
| ⑧ | hooks 由来通知 UI | 中/低 | ⏸ defer（`--include-hook-events` 余地） | — |
| ⑨ | MCP OAuth 等 interactive 認証 | 低 | ❌ **非対象**（構造的、Act I へ誘導） | — |

**本 doc のスコープ = ①②③④（HITL 4 面）**。③'④' は別機構で既に着地済み、⑤〜⑧ は defer、⑨ は構造的非対象。

## 7. PR 分割（直列、各 PR 単独で nightly に載る）

doc 32/34 と同じ流儀。順序の根拠: **既定を壊さない最小の①から入り**、以降を opt-in で加算する。①は bypassPermissions を維持したまま質問だけ足すので現状の体験を退行させない。

| PR | 内容 | exit criteria |
|---|---|---|
| **PR1** | 質問レール: spawn に `--permission-prompt-tool stdio`（同時に `host.rs:153-154` の「`--permission-prompt-tool` は 2.1.197 で削除済み」誤記コメントを訂正 — §8 が反証済）+ initialize handshake + 逆方向 `can_use_tool`(AskUserQuestion) 受信 → `EchoesEvent::Question` + host `respond_permission`(stdin 書き戻し) + ChatView PromptCard(選択肢) + awaiting_input 点灯 | 実タスクで clarifying question が GUI の選択肢ダイアログで出て、選ぶと turn が続く。既定 bypass のまま tool は素通し（退行なし） |
| **PR2** | turn 中断: host `interrupt()` + `echoes:interrupt` dispatch + ChatView 停止ボタン(Esc) | 走行中 turn を停止ボタンで中断でき、engine は次 submit を受けられる |
| **PR3** | permission prompt: host `set_permission_mode()` + `PermissionRequest` variant + PromptCard(allow/deny) + mode 切替 UI | `mode=default` に切替えると Write/Bash が承認ダイアログ経由になり、allow で実行・deny で回避する |
| **PR4** | plan 承認: `set_permission_mode("plan")` + ExitPlanMode / plan clarifying を PromptCard(承認/却下) 化 | plan mode に入れて plan 承認 UI で抜けられる（承認で mode が戻る） |
| **PR5** | type-ahead バッファリング（§5.1）: `ChatItem.assistant.sealed` + `ChatState.pending` + `submit()` の streaming 分岐 + `foldInto` の `turn_completed`/`error` で封印 + `foldEvent` の flush hook + pending の "送信待ち" chip 描画 | 走行中に送った2通目が turn 完了後に `user2 → assistant B` の順で並び、turn A の応答と融合しない。reconnect / HITL pause 中は流れない |
| **末尾** | doc-only: `event.rs:99-100` の布石コメント撤去、doc 34 との EchoesEvent 語彙合流 sweep（HITL/wire バブル種別を 1 箇所に整理） | — |

- 実装運用は doc 32 §8 と同じ（team-b レビュー → `gh pr create --base nightly` → auto-merge、GitNexus impact/detect_changes、pre-MVP 原則、dogfood の daemon 再起動は gentle のみ）。
- **実装土台**: spike スクリプト（`.vp-scratch/act2-ui-gap/spikeB_control.mjs` / `spikeD_canusetool.mjs` / 本 lane `spikeE_capability.mjs`）が control_request/response の動く wire 形を持つ。Rust への移植は自前実装（外部依存ゼロ、handshake は stdin/stdout の JSON のみ）。

## 8. 付録: 決定 spike（実測結果）

**問い**: capability 宣言つき initialize（Agent SDK 相当の `canUseTool` 宣言）で、native AskUserQuestion が headless の `tools[]` に現れ、`can_use_tool` 経由で発火するか。両調査ともここ未到達だった。

**点火条件の特定**（SDK ソース精読）: `claude-agent-sdk-python` は `can_use_tool` callback を渡すと、内部で `permission_prompt_tool_name = "stdio"` を設定し（`client.py`「Automatically set permission_prompt_tool_name to 'stdio' for control protocol」）、CLI に **`--permission-prompt-tool stdio`** を渡す（`subprocess_cli.py`）。initialize payload 自体は `canUseTool` capability を明示宣言せず（`query.py`）、**この CLI flag が「permission/question を stdio control channel に routing する」点火条件**。調査はこの flag を渡さず bare initialize で止まったため auto-deny になっていた。

**環境**: claude 2.1.197、`env -u VP_LANE` + cwd=/tmp + `--model haiku`（現行 Act II と同一の隔離作法）。共通 flag = `-p --input-format stream-json --output-format stream-json --include-partial-messages --verbose --permission-prompt-tool stdio`。initialize = `{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","hooks":null}}`。生ログ: 本 lane `.vp-scratch/hitl-doc35/spikeE_{write,question,bypass_q}.jsonl` + `spikeE_capability.mjs`。

**結果**:

| run | permission-mode | init `tools[]` | AskUserQuestion in tools[] | can_use_tool 発火 | turn |
|---|---|---|---|---|---|
| write | default | 113 | **true** | `Write` ×1（allow→ファイル生成） | `result success` |
| question | default | 224 | **true** | **`AskUserQuestion` ×1**（full questions）→ allow(answers) → `Write` ×1 | `result success` |
| bypass_q | **bypassPermissions** | 70 | **true** | **`AskUserQuestion` のみ ×1**（Write は素通しで非発火） | 回答後 assistant 継続 ×6 |
| （参照）spikeD | default（**flag なし**） | 85 | **false** | 0（Write auto-deny） | 調査の停止点 |

> `tools[]` の総数（113/224/70）は MCP/slash の非同期ロードでブレる noise。**決定的シグナルは AskUserQuestion の有無**で、これは stdio flag で確定的に反転する（flag あり = true ×3 / flag なし = false）。

**逆方向 can_use_tool の生 wire（AskUserQuestion）**:
```json
{ "type": "control_request", "request_id": "4785df80-…",
  "request": { "subtype": "can_use_tool", "tool_name": "AskUserQuestion",
    "display_name": "AskUserQuestion", "tool_use_id": "toolu_018eD…",
    "input": { "questions": [ { "question": "Which language …?",
      "header": "Greeting Language",
      "options": [ {"label":"English","description":"…"},
                   {"label":"Japanese (日本語)","description":"…"} ],
      "multiSelect": false } ] } } }
```
**応答（我々→claude）**:
```json
{ "type": "control_response",
  "response": { "subtype": "success", "request_id": "4785df80-…",
    "response": { "behavior": "allow",
      "updatedInput": { "questions": [ … ], "answers": { "Which language …?": "English" } } } } }
```

**判定 = YES**。
1. `--permission-prompt-tool stdio` + initialize で **AskUserQuestion が tools[] に載り**（bare init では不在）、**`can_use_tool` control_request として発火**する。
2. `control_response{behavior:"allow", updatedInput:{questions, answers}}` で回答すると **turn が正しく継続**する（run question で回答後に Write→`result success`）。
3. `bypassPermissions` + stdio で **通常 tool は素通しのまま、AskUserQuestion だけが routing**される（run bypass_q）→ 既定を bypass に据えたまま質問だけ拾える。
4. 同 flag で **ExitPlanMode も tools[] に載る**（run write の HITL 系採取 = `['AskUserQuestion','ExitPlanMode']`）→ plan 承認（④）も同レール。

→ **質問経路 = native AskUserQuestion（can_use_tool 横取り）に確定**。VP MCP `ask_user` は不要（構造化フォーム補完に限定）。副次的に「`--permission-prompt-tool` は 2.1.197 で削除」という調査 §10.1 の結論を訂正する（`--help` 非表示だが flag は生存・機能）。

**未決点**（実装時に確認）:
- initialize handshake が routing に必須か、`--permission-prompt-tool stdio` flag 単独で足りるか（spike は SDK 同形で両方送った。実害無いので実装は handshake を送る）。
- multiSelect=true の回答形（label 配列 or `", "` 結合）の実 wire 確認。
- `set_permission_mode("plan")` 中の ExitPlanMode の can_use_tool 形（PR4 着手時に spike 追加）。
