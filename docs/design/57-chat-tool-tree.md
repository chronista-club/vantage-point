# doc 57 — chat tool tree: 塊の木と live root（tool が chat を占領しない）

**Status**: **設計確定（2026-07-31、mako × Claude）。** 骨格 3 点（§3）+ 細部 3 点（§6）を
AskUserQuestion ダイアログの裁定で確定。実装は P1 → P2（§5、mako 裁定「doc に起こしてから」
に従い doc 確定後に着手）。
発端 = bikeboy-ladyland の turn で tool ピルが 15 行近く chat を占領した screenshot。
mako「chat の大半をツールが占める時があって、現在のアコーディオンして、情報は降りれる
ようにしつつ、表示的には、アコーディオンの root + そのツールの１ライナーステータス。
ツリー構造で、今動いている現在のツールや agent の状態を把握したい。」
**Owners**: vp-app（webview/chatview.tsx のみ — server / wire 変更なし）
**Related**: [55-board-projection.md](./55-board-projection.md)（表示所有権 = user の開閉を
システムが上書きしない。§4.5 が継承）/ doc 35 §4（PromptCard = HITL。木に**入れない**側の
根拠）/ [52-board-redesign.md](./52-board-redesign.md)（対話面 = chat の役割定義）

---

## 1. 問題: ピルに情報がなく、畳みは同名連続のみ

現行の tool 表示は 2 段構えだが、どちらも「壁」を撃ち抜けない:

| 部品 | 現状 | 帰結 |
|---|---|---|
| `ToolRow`（単発） | head = `🔧 + tool 名 + ✓` のみ。input 由来の情報ゼロ | 「Bash ✓」が並んでも何をしたか分からない |
| `ToolGroupRow`（×N） | **連続同名**だけ畳む（`classifyToolRun`） | `Write → Edit → Bash …` の異名連鎖は全部裸 → 15 行の壁 |

一方で下地は既にある:

- **入れ子**: Agent の subagent 発話は `SubagentBlock` で ToolRow 内に木として入る
- **偽らない集約**: `toolGroupStatus` は 1 件でも未 done なら running + `{done}/{count}`
  （エンジン状態を偽らない方針）
- **描画時のみ集約**: `classifyToolRun` は reducer（`foldInto`）を触らず描画時に束ねる —
  replay / 孤児 `tool_call_update` の不変条件（§C2）を汚さない設計。これを一般化して使う

## 2. 原理: 幹は対話、作業は節

chat の幹（主役）は **user ↔ assistant の対話（本文）**。tool / thinking は「作業の痕跡」で、
幹に並べず **1 つの節（木）に畳む**。ただし情報は捨てない — 降りれば全部ある
（アコーディオンの原則は維持、mako 要件）。

**人の操作・判断を要する item は「作業」ではなく「対話」** — PromptCard（質問）/
PermissionCard（承認）は木に入れず幹に残す。畳まれた木の中に HITL が埋もれたら、
engine が人を待っている合図（doc 35）が死ぬ。

## 3. 裁定 3 点（2026-07-31、AskUserQuestion ダイアログで mako 承認）

| 分岐 | 裁定 | 効き |
|---|---|---|
| 塊の単位 | **thinking も木に入れる** — 本文（と HITL）だけを区切りに | 1 turn ≈ 1 root。圧縮が最大 |
| 走行中の root | **畳んだまま live root** — root 行自体が現在地を生更新 | 面積は常に最小、現在地は常時可視 |
| 進め方 | **doc に起こしてから**実装 | 本 doc → レビュー → P1/P2 |

## 4. 設計

### 4.1 塊（activity run）の定義

- **木に入る kind**: `tool` / `thinking`
- **区切る kind**: `text`（本文）/ `prompt`（question・permission）。将来の人間向け item も
  既定で区切り側（迷ったら幹）
- `classifyToolRun` を `classifyActivityRun` に一般化: 連続する {tool, thinking} run を
  `single / head / member` に分類。**描画時のみ・reducer 不変**の原則をそのまま継承
- **run 長 1（tool 単発）は root を作らない** — 従来の 1 行表示（+ §4.4 の 1 ライナー）。
  単発に root を被せると行が増えるだけで本末転倒
- **thinking だけの塊も root を作らない**（P2 実装時に確定した細則、2026-07-31）— 作業で
  なく思考の流れなので従来の ThinkingBlock のまま。root 化の条件 = **長さ ≥2 かつ tool ≥1**

### 4.2 root 行（live root）

```
走行中:  ▸ ⟳ cargo test -p vantage-point --lib (12/15 · agent 1)
完了:    ▸ ✓ 15 tools · 1 agent · 3m12s
error:   ▸ ✗ 15 tools · 1 agent · 3m12s（error 色。1 件でも error なら）
```

- 走行中の 1 ライナー = **未 done の最新 tool** の 1 ライナー（§4.4）。「今なにをしているか」
  が畳んだまま見える — これが mako 要件「今動いている現在のツールや agent の状態を把握」の
  実装点
- 計数は tool のみ（thinking は節としてだけ入り、`{done}/{total}` に数えない）
- `toolGroupStatus` を拡張した純関数で導出（テストで固定）。走行中に error が出ても
  全 settle までは running を維持（現行と同じ「偽らない」規律）
- **経過時間** = 塊の先頭 item の受信時刻 〜 最後の tool settle。受信時刻は `foldInto` の
  append 時に item へ刻む（§4.6 の注記）。**transcript replay では出さない** — 実時間は
  再現できないので、測っていないものを表示しない（偽らない規律の適用）
  - 既知の edge（P2 レビュー所見、未再現の理論値）: replay watchdog（10s）が replay 途中で
    強制発火すると以降の replay item に実時刻が刻まれうる。塊の一部でも時刻が欠ければ
    elapsed は null に落ちるので、嘘の経過が出るのは「watchdog 後に丸ごと流れた塊」のみ

### 4.3 木の中身（展開時）

```
▾ ⟳ cargo test -p vantage-point --lib (12/15)
    ✓ Write   keystage/model.rs
    ✓ Edit ×4 keystage/model.rs ほか
    ▸ thinking  AppState の配線を検討…
    ▸ ✓ Agent   moody-blues — diff レビュー     ← 掘ると subagent の木（従来どおり）
    ⟳ Bash    cargo test -p vantage-point --lib
```

- 子 = 1 ライナー付き tool 行。**連続同名はこれまで通り ×N に畳む**（孫 = 個別 ToolRow）—
  既存 ToolGroupRow の価値を木の中で保持
- thinking 節 = 現行 ThinkingBlock 相当の開閉 + **1 ライナー = 冒頭 1 行**（畳んだまま
  思考の流れが読める）
- Agent 節 = 現行 ToolRow + SubagentBlock そのまま（木の 3 層目）

### 4.4 1 ライナー summarizer（P1 の本体）

純関数 `toolOneLiner(name, input): string | null` を表駆動で:

| tool | 出どころ | 例 |
|---|---|---|
| Bash | `description`（CC が送る日本語説明）→ 無ければ `command` 先頭 | `cargo test を実行` |
| Edit / Write / Read / NotebookEdit | `file_path` を**親 1 段 + basename** に短縮 | `keystage/model.rs` |
| Grep / Glob | `pattern` | `permission_choices` |
| Agent | `description` | `auto 解禁 diff のレビュー` |
| Skill | `skill` | `release` |
| WebFetch / WebSearch | `url` / `query` | — |
| `mcp__*` / その他 | input 中の最初の意味ある string field | — |
| fallback | `null` → 従来どおり tool 名のみ | — |

- 表示: tool 名（mono）+ 1 ライナー（text-secondary、1 行 ellipsis）
- 単発 ToolRow / 木の子 / ×N group header（代表 = 先頭の 1 ライナー + ほか）全部に効かせる

### 4.5 開閉と表示所有権（doc 55 継承）

- 既定 = 畳み（走行中も）。**user が開いたら、stream 追記・turn 完了でも勝手に閉じない**
  （表示所有権は user。システムは初期値だけ決める）
- 開閉状態は塊の identity（先頭 item の id）に紐づけ、`foldInto` の append-only 特性で
  run が末尾に伸びても状態は保たれる

### 4.6 不変条件

- reducer（`foldInto`）の**挙動は不変** — 集約は描画時のみ。P2 で唯一足すのは append 時に
  受信時刻を item へ刻む field（経過時間の材料、§4.2。既存 item の生成・変異経路は不変）。
  id 一致 done 化・孤児 tool_call_update の §C2 不変条件には触らない
- server / wire 変更なし（vp-app webview 完結、bundle 再生成のみ）

## 5. 実装フェーズ

| Phase | 中身 | 出荷単位 |
|---|---|---|
| **P1** | `toolOneLiner` + 既存 ToolRow / ToolGroupRow への配線 + vitest | 小 PR（木がなくても即効く） |
| **P2** | `classifyActivityRun` + root 行（live root）+ thinking 取り込み + 開閉所有権 | PR（本丸） |

答え合わせ: 発端 screenshot と同型の turn を実機で再現し、走行中（live root）と完了後
（1 行収束）を screenshot で確認。

## 6. 細部裁定（2026-07-31、ダイアログで mako 承認）

1. **file path の 1 ライナー表示形** = **親 1 段 + basename**（`keystage/model.rs`）。
   末尾（ファイル名）が情報の主役で、幅は有限
2. **完了 root の文言** = **経過時間も出す**（`✓ 15 tools · 1 agent · 3m12s`）。
   turn の重さが一目で分かる。replay では出さない（§4.2 の偽らない規律）
3. **thinking 節の 1 ライナー** = **冒頭 1 行を出す**（畳んだまま思考の流れが読める）

## 7. 終了を告げずに消えた subagent（2026-10-12）

進行中の subagent は会話の流れから独立した float（`SubagentDock`、`runningSubagents` が表示源）に
出す（#1201）。終わりの合図は claude が流す `task_notification` / `task_updated`（終了 status）。
**この行が来ないまま subagent が消える経路がある**: 親 claude プロセスが落ちると、プロセスの中で
動く subagent も一緒に死ぬが、終了の行は誰も書かない。

- foreground は「`streaming` でなければ出さない」で隠れていた（隠れていただけで、次の turn が
  streaming を立てると回り出す）
- background（`is_backgrounded`）は tool が即返るので streaming を見ない。**終了の行が来ない限り
  永遠に「進行中」**（2026-10-12 実機: 起動直後に親が落ち `--resume` で復帰、float が 1 時間半残った）

### 7.1 区切りの事実を見た側で閉じる

開いたままの subagent（task が `running`、または task 情報なしで Agent tool が返っていない）を
`closeOpenSubagents` が tool item の `interrupted` 印で閉じる。呼ぶのは 2 か所:

| 区切り | 何の事実か | 経路 |
|---|---|---|
| `engine_exited` | 親プロセスの stdout が閉じた = 中の subagent も全部死んだ | live（host の stdout ポンプが必ず送る） |
| `user_message` | 結果の無い Agent の後に次の人間の発話 = その turn は切れている | replay 専用 event |

- **`session_init` は区切りに使わない。** claude は同じプロセスでも user message 1 通ごとに
  `system/init` を出す（実測 claude 2.1.296: 2 通送って init 2 回）。ここで閉じると、turn を
  またいで正当に走っている background subagent を次の送信のたびに殺す
- **replay は `subagent_task` を運ばない**（transcript.rs は `system` 行を捨て、in-flight tail は
  chunk しか持たない）。だから background は replay で蘇らない — tool_result が即返っているので
  task 無しの done な Agent 行になる。replay で残る穴は「結果の無い foreground Agent」で、生成中の
  turn に着地すると streaming が立って回り出す。区切りを次の `user_message` に置くのは、replay に
  turn の区切りとして確実に載るのが人間の発話だけだから（turn_completed は載らない。session_init は
  host が最後の 1 件を保持して配り直すだけで、turn ごとには並ばない — `conversation_replay.rs`）
- **live の user 発話では閉じない。** live の user 行は送信時に直接足すもので（chat-model の
  submit 経路）`user_message` event を通らず、background は turn をまたいで生きるのが正しい
- **status は書き換えない。** `running` は claude の語彙で、「中断」は claude が言っていない。
  最後に分かった status はそのまま残し、「もう走っていない」は `interrupted` 印で表す。表示は
  ToolRow の状態が「中断」、task 詳細の見出しが `task · running · 中断（engine 終了で停止）`
- 閉じた後に task の行が来たら、生きている証拠として印を外す（新しい事実を優先）
- **時間で消さない。** 長い background 作業を誤って閉じる。閉じる根拠は「死んだ / 切れた」事実だけ
- 塊（activity run）の root / ToolGroupRow の spinner は `done` だけを数えるので、親の死で結果の
  来ない tool（Agent 以外も）は今も「実行中」に数える。本節の範囲外（float の幽霊だけを閉じる）
