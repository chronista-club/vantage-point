# doc 37 — Echoes の 2 軸整理: engine × surface（Act）

> **status**: 設計確定（2026-07-15）
> **supersedes**: [doc 36](./36-echoes-engine-axis.md) §4「A（lane サブコンソール）方向で攻める」決定 → **保留**に格下げ（本 doc §6）
> **関連**: [doc 36](./36-echoes-engine-axis.md)（engine 軸格子・サブコンソール設計）/ [doc 33](./33-console-unification.md)（console-unification, 1 lane=1 engine 法）/ [cursor-engine.md](./cursor-engine.md) / [doc 32](./32-echoes-act2-gui.md)
> **動機**: cursor(#773/#776) で 2 エンジン目、codex を 3 エンジン目に見据えた段階で、「Act」という語が **surface（TUI/GUI）**と **engine（頭脳）**の二重の意味を背負い始めた。engine が増えるほどこの二重性が破綻するため、2 軸の mental model を確定して開発の背骨にする（user 指示 2026-07-15）。

## 0. TL;DR — 法

> **Echoes は engine 軸 × Act(surface) 軸の直交格子。ただし 2 軸は対等でない。**
> **engine = session に束縛される identity（永続）。Act = その session に被せる切替 view（一時的）。**

- **原則: Act ≠ engine**。エコーズの Act(進化形態)は並列の兄弟である engine には写らない。
- **現決定: engine は lane-pinned**（1 lane = 1 engine、doc 33 の法を維持）。複数 engine = 複数 lane。
- サブコンソール（1 lane 内で engine 共存、doc 36）は**保留**。3 エンジン実機 dogfood 後に「N session/lane 再カット」として再検討。

## 1. 語彙（mental model）— 3 層とコード対応

Echoes は「コーディングアシスタント」という能力の namespace。その中に 2 つの直交軸がある。

| 層 | 意味 | コードの正体 | 束縛 |
|----|------|-------------|------|
| **Echoes 💬** | 能力 / namespace | `stand="echoes"`（+ engine 種別） | — |
| **engine 軸** | どの頭脳か（claude / cursor / codex …） | `EngineKind` + `ChatHost { Claude, Cursor, Codex }`（`echoes/engine.rs`）/ `cc_session` \| `cursor_session` \| `codex_session` | **session 束縛・永続** |
| **Act(surface) 軸** | どう視るか（Act I 端末 / Act II chat / Act III 将来 canvas） | `ConsoleMode { Tui, Chat }`（`lane/console_mode.rs`） | **view・切替可能** |

- **「Act」はコード識別子ではない**。コードにあるのは `ConsoleMode`（surface）と `ChatHost`（engine）の 2 enum だけ。`Act I/II` は doc と会話の中にしか無い mental model ラベルなので、**意味の再割り当てはコスト 0**（リネームも migration も発生しない）。この doc はその「意味」を確定するだけ。
- コードは既に 2 軸を直交分離済み（`ChatHost` × `ConsoleMode`、pump は engine 非依存）。**壊れているのは構造ではなく語彙**。本 doc は構造を追認して語彙を締める。

## 2. 非対称性 — なぜ 2 軸は対等でないか

| 軸 | 何を表すか | いつ決まるか | 切り替えの意味 |
|----|-----------|-------------|---------------|
| **engine** | 走っている実体（頭脳） | spawn 時（stand で確定） | 別 engine = **別会話**（cc_session/chatId が別） |
| **Act** | 覗く窓（presentation） | いつでも切替可 | 同 engine=同会話のまま view だけ変わる（`--resume` で継続） |

- doc 33 の法「1 lane = 1 Console = ≤1 engine = 1 cc_session」は、実は **engine ≈ session** を宣言していた。
- cursor-engine.md の `record-from-init`（Act II で進めた会話が Act I へ継がれる）も「engine は session 束縛、Act は view」を裏付ける。`console_mode` を Tui↔Chat と切っても engine=会話は不変。
- **帰結**: 「Act = engine」はカテゴリエラー。engine は「何が走っているか」、Act は「どう視るか」。

## 3. JoJo 忠実性 — Act を engine に使わない理由

- エコーズの **Act1 → Act2 → Act3** = 同一スタンドが進化して別の姿になる**成長ラダー**（一度に使えるのは 1 つ、切替）。
- **claude / cursor / codex は並列の兄弟**。claude は cursor の「Act1」ではない。engine はラダーではなく横に並ぶ選択肢。
- → Act（進化して richer な形態）が自然に写るのは **surface 軸**の方: Act I 素の端末 → Act II 構造化 chat → Act III 将来の Canvas 統合コックピット。これは本物の成熟ラダー。
- engine 軸には Act を付けず、素の「engine」語（外向けは普通の用語、CLAUDE.md の命名方針どおり）で呼ぶ。

## 4. 元ビジョンの回収

user の原イメージ「その名の通りいろんなエコーズ（Act1,2,3）がいる」は、実は **2 軸を 1 つに束ねていた**。分解して両方を活かす:

- **「いろんなエコーズ」→ engine 軸**: claude-Echoes / cursor-Echoes / codex-Echoes。多様な頭脳が「いろんなエコーズ」。
- **「Act1,2,3」→ Act(surface) 軸**: 同じ session が端末 → chat → canvas と姿を変える進化。

両方生き残る。混同をやめるだけ。

## 5. 現決定 — engine は lane-pinned（doc 33 の法を維持）

- **1 lane = 1 engine = 1 session**。複数 engine を触りたければ**複数 lane**を立てる。
- cursor は既に `add_performer(stand="cursor")` で動く（cursor-engine.md）。engine 軸は「格子に row を 1 本足す」だけで増える。
- **サブコンソール split（1 lane 内 engine 共存）は作らない**。理由:
  - pre-MVP 最小化方針（over-scope しない）。主+副 2 枚固定 split は codex 3 本目でスケールしない。
  - 「1 lane 内で別 engine を即重ねる」体験が本当に要るかは、**3 エンジンを実機で触ってから**判断する方が確度が高い。

## 6. doc 36 の再配置

- doc 36 §4「A（lane サブコンソール）方向で攻める」= **保留**（本 doc が supersede）。
- doc 36 Phase 0 実装（todo `mem_1Cd3bo6Y4YepXnRqQyeWf8`）は本決定により**着手保留**。
- 再検討する時の第一候補は doc 36 の「主+副 split」ではなく、§2 の非対称から素直に出る一般形:
  > **1 lane = N session、engine = session 単位、Act = 各 session の view。**
  - これなら codex が 3 本目でも「session を 1 本足す」だけで済み、主/副の 2 枚固定を持たない。
  - doc 36 の webview split / topic 分離 / sub-key アドレスの実装知見は、この再カット時にそのまま素材として使える（doc 36 は「保留された設計資産」として残す）。

## 7. codex を 3 本目に足す（調査確定 2026-07-15、codex-cli 0.144.4 実測）

> **✅ 実装済（2026-07-15、branch `mako/echoes-multi-engine`）**。実装は「雛形の複製」ではなく
> **機構の抽出**で行った: turn-scoped 機構（queue / interrupt=kill / self-heal / 偽 Error 回避）を
> `echoes/turn_host.rs` の `TurnHost<E: TurnEngine>` に一元化し、cursor / codex は差分
> （コマンド構築・翻訳器・session 永続・表示名）だけを `TurnEngine` 実装として差す。
> 併せて `EngineKind`（stand↔engine 対応表 + 能力表明の SSOT、`echoes/engine.rs`）を新設し、
> stringly な stand 比較 4 箇所を畳んだ。session state file の共通核は `lane/session_store.rs`。

**結論: codex は cursor と同型 = turn-scoped host。`CursorAgentHost` を雛形に `CodexAgentHost` を作る。**
`codex exec` は 1 invocation = 1 turn の one-shot で、claude の常駐 stream-json stdin に相当する
機構は無い（`app-server` / `exec-server` / `mcp-server` は experimental、採らない）。cursor が「翻訳層 +
host を足すだけで乗った」直交性の配当（doc 36 §1）を踏襲する。

### 確定した command 面（`codex exec --help` / `codex exec resume --help` 実測）

- **初回 turn（session 未記録）**: `codex exec --json --dangerously-bypass-approvals-and-sandbox "<prompt>"`
  （cwd は spawn 時に設定、cursor/claude と同じ）。
- **継続 turn**: `codex exec resume <SESSION_ID> --json --dangerously-bypass-approvals-and-sandbox "<prompt>"`。
  SESSION_ID = UUID（`thread.started` イベントで採取。"UUIDs take precedence"）。
- **prompt** = positional arg（`Command::arg` 渡し = shell 非経由で injection 安全、cursor と同じ）。
- **全ツール素通し** = `--dangerously-bypass-approvals-and-sandbox`（claude の `bypassPermissions` /
  cursor の all-tools 相当）。⚠️ `-s danger-full-access` 単体では承認プロンプトが残り headless で
  許可待ち → [[echoes-act2-parity]] の error 化を踏むので不可。
- **record-from-init**: `--json` の `thread.started{thread_id}` を `codex_session` に永続（cursor の
  `system/init` → record と同型）。**codex には cursor の `create-chat` 相当（thread id 先取り）が無い**
  → 初回は resume 無しで走らせ thread_id を採取するだけ（cursor の pre-allocation より単純）。`--last`
  （最新 picker）は使わず UUID 指名で決定的にする。

### イベント翻訳（`codex_translate`、`--json` JSONL → EchoesEvent）

| codex --json | → EchoesEvent |
|---|---|
| `thread.started{thread_id}` | `SessionInit` + record |
| `item.*` type=`agent_message` | `MessageChunk` |
| item type=`reasoning` | thinking chunk |
| item type=`command_execution` / file change / MCP / web / plan | `ToolCall` / `ToolCallUpdate` |
| `turn.completed{usage}` | `TurnCompleted` |
| `turn.failed` | `Error` |

⚠️ **`agent_message` が delta streaming か whole-message か**（cursor の `--stream-partial-output` delta
判定に相当）は **live 実測が要る**（`codex login` 後に `codex exec --json "hi"` を 1 本流して確認）。
外すと二重描画（cursor-engine.md の教訓）。

### Act I（TUI 床）

- 床 = bare `codex`。resume = `codex resume <SESSION_ID> || codex`（cursor の
  `cursor-agent --resume '<id>' || cursor-agent` と同型）。
- ⚠️ `codex resume` は help 上「picker by default」。**id 指名が picker を挟まず直行するか**は要 live
  実測（claude `--continue` の dashboard 化 / cursor が create-chat を選んだ理由と同じ罠の可能性）。id
  直行が駄目なら Act I は `codex resume --last` か床を exec ベースに変える検討。

### その他スロット（cursor と同じ）

- **session 永続**: `codex_session`（`cursor_session` 同型、`vp_state_dir()/codex_sessions/<project>__<lane>`、
  id 検証は UUID 形）。
- **HITL**: control channel 無し → `respond_permission` / `set_permission_mode` は Err（cursor と同じ）。
- **model**: codex は `-m <model>` / `-c model=` を持つ（cursor と違い model 注入が**可能**）。ただし
  `engine_model` 連携は v1 スコープ外（cursor 踏襲）。
- **wire**: claude 専用のまま（codex に hook 注入機構は使わない。doc 36 §2 の非対称踏襲）。
- **stand 分岐**: `ensure_chat_engine`（`ChatHost::Codex` を 3 番目の arm に追加、全 match を埋める）/
  `set_console_mode` の Chat 許可 `stand ∈ {echoes, cursor, codex}`。
- ⚠️ **doc 33 §10 の「新経路の登録漏れ」4 点**: IPC tag（`is_main_ipc_tag`）/ canvas subscribe
  （discovery.rs）/ dispatch table（unison_server.rs）/ engine 分岐。

### 残る empirical gap（`codex login` gated — 実装は両方を保守的に吸収済み）

1. `agent_message` の delta/whole 判定 → **実装は completed-only emit で吸収**
   （`codex_translate` module doc: delta が存在する版でも二重描画は構造的に起きない。
   streaming 感を上げたくなったら login 後に `codex exec --json "hi"` 1 本で形を確定して緩める）。
2. `codex resume <id>` の picker 直行性 → **Act I 床は `|| codex` fallback で吸収**
   （id が拒否されても素の codex に倒れて床は成立。headless 側は実測済 —
   存在しない id は「no rollout found」即エラー終了 = TurnHost self-heal が効く）。

## 7.5 agy（Antigravity CLI / Gemini CLI 後継）— 実測 2026-07-15、agy v1.1.2

> brew cask `antigravity-cli`（既に install 済 `/opt/homebrew/bin/agy`、binary `antigravity -> agy`）。
> agy は 2026-06-18 廃止の **Gemini CLI の後継** = doc 36 §1 grid の "gemini?" セルの実体。

**結論: agy は Act I 可・Act II は現状不可。engine の readiness は「engine × Act のセル単位」で、engine 単位ではない。**

- **Act I（TUI 床）= 今すぐ可、ただし v1 は fresh-only**（実装時確定 2026-07-15）: 床 = bare `agy`。
  `agy --conversation <ID>`（id 指名 resume）自体は CLI にあるが、**id の書き手が存在しない** —
  cursor の create-chat 相当（先取り採番）が無く、Act II が無いので record-from-init 経路も無い。
  `agy -c` / `--continue`（最新）は help 上 cwd 非スコープで、複数 lane が同時に使うと**別 lane の
  会話を拾うクロス誤爆リスク**があるため採らない。よって v1 は respawn ごとに新規会話
  （fresh-only）。resume 化の条件 = ① agy が JSONL を出して Act II が立つ（record-from-init が
  書き手になる）or ② `-c` の scope（cwd/project 単位か）を login 後に実測して安全と確認。
  参考: 全ツール素通し = `--dangerously-skip-permissions`、mode = `--mode plan|accept-edits`、
  model = `--model`（いずれも v1 では注入しない — TUI 内で user が選ぶ）。
- **Act II（rich chatview）= 現状ブロック**: agy の headless は `agy -p "<prompt>"`（`--print` / `--prompt`、
  single prompt を非対話で print、`--print-timeout` 既定 5m）。だが **v1.1.2 に構造化出力フラグが無い**
  （`--output-format json` 等は実物 help に不在。web 記事は `--output-format json` を主張するが実物と
  食い違う = 実物が真実源）。→ `-p` はプレーンテキストの最終応答のみで、tool call / thinking / streaming を
  `EchoesEvent` に翻訳する材料が無い（cursor/codex の translate 層に相当するものが作れない）。
- **将来 Act II を開ける条件（2 択）**: ① agy 側が JSONL イベント stream を出す（要 upstream。agy は
  数週間前リリースで急進化中 = 時間の問題の可能性）② VP が agy の会話履歴ファイルを追尾 → 翻訳
  （doc 33 §4 の「transcript 追尾 cross-mode mirror」路。重く fragile、当面採らない）。
- **auth**: 未ログイン（`~/.antigravity` 不在）。dogfood は agy login が gate（cursor/codex と同じ関門）。

**per-cell readiness の含意**: doc 36 §1 の格子は「各ベンダーが両セルを埋める」前提だったが、agy は
**Act I セルだけ埋まり Act II セルは空**。engine 追加の意思決定は engine 単位でなく **(engine × Act)
セル単位**で評価する。codex は両セル ready、agy は Act I セルのみ ready。

## 7.6 header の engine 一般化（Act I `cc:` chip bug を相乗り）

> bug todo: `mem_1Cd3icsvKiGsQ8TtX8t1FR`（Act I で `cc:xxxxxxxx` session chip が出ない）。multi-engine
> 実装と一体で直す。**✅ 実装済（2026-07-15、同 branch）**。実装上の 1 決定: `cc_session_id` の
> 意味は広げず（delivery_actor channel D が claude `--resume` に使う claude 専用契約）、表示用の
> **`engine_session_id` を別 field として追加**した（additive、契約分離）。chip prefix は
> `cc:` / `cur:` / `cdx:`（`sessionChipPrefix`、chip が engine indicator を兼ねる）。

**根因**: `EchoesHeader.tsx:149-159` の `cc:` chip は `summary().sessionId` に依存し、その供給源は
`vpConsole.headerState()` = **EchoesEvent(`session_init`/`turn_completed`) の畳み込みのみ**（`console.ts`）。
Act I は raw PTY で claude TUI を直ホストし EchoesEvent を出さない → sessionId が空 → Act I で chip が
隠れる。ヘッダは「Act I/II 共通」を謳うが sessionId のデータ経路は Act II 専用、という非対称。

**なぜ multi-engine と一体か**: `cc:` prefix は **claude 固定**。cursor(chatId) / codex(thread UUID) /
agy(conversation) では session の呼称が違うので、engine を足すと header ラベルの一般化が**不可避**。
その不可避改修に Act I 供給を相乗りさせる。

**fix**:
1. **Act I 供給路**: Rust が per-engine session state file（claude=`cc_session` / cursor=`cursor_session` /
   codex=`codex_session`）を読み、`push_active_view`（setActivePane）payload の `HeaderLaneCtx` に
   session id を載せて push。`HeaderLaneCtx` に `sessionId` field を追加。Act II event fold と OR 合流
   （event 真値が来たら上書き）。
2. **ラベル一般化**: `cc:` 固定を engine 中立/engine 別表記へ（例: `sid:` or engine icon + 短縮 id）。
   agy は Act I のみなので agy lane では state file 由来の id を出す。

## 8. 非目標（over-scope 防止）

- **サブコンソール実装** → 保留（doc 36、§6）。
- **CLAUDE.md / stands.rs の Echoes 記述更新**（「Claude CLI オーケストレーター」→ multi-engine エンジンオーケストレーター）→ 語彙が固まった別 PR で。
- **Act III（Canvas コックピット）設計** → surface 軸の将来。別 doc。
