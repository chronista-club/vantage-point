> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# cursor engine — cursor-agent を Act I lane エンジンとして追加

## 目的

Cursor CLI（`cursor-agent`）を VP の lane console（Act I）エンジンとして追加する。
Echoes（claude）と並ぶ「エンジンごと専用統合」の第二エンジン。 stand 名 = `"cursor"`。

cursor-agent は claude と CLI surface が酷似するため、 既存の Act1-layered 構造
（`docs/design/tmux-decoupling.md` §13）にそのまま載る:

```
PtySlot → $LOGIN_SHELL -l                       ← Act1: 常に生きる「床」
   ↓ initial_input（type-ahead 注入）
   cursor-agent --resume '<chatId>' || cursor-agent   ← Act3（|| fallback は shell が native 処理）
```

- `cursor-agent` = 対話 TUI 起動
- `cursor-agent create-chat` = 空チャットを作り chatId を stdout に返す（headless、 handoff 用）
- `cursor-agent --resume '<chatId>'` = ID 指名 resume

## ID 指名 resume（create-chat 先取り）を選んだ理由

claude の `--continue`（「cwd の最新 session」を拾う）は Background Agents 在りで Agent View
dashboard 化し、 type-ahead 注入が list-nav UI に化ける既知の罠がある
（`stand_spawner::claude_command` / `cc_session` doc 参照）。 cursor の latest resume も同型の
「最新」曖昧性リスクを持つため使わない。

代わりに `cursor-agent create-chat` で chatId を先取りし、 それを lane 単位で永続して
`--resume '<chatId>'` で**指名** resume する。 spawn 前に Rust が state file を直読みするので
決定的（実行環境の「最新」に依存しない）。

- 書き手 / 読み手: `crate::lane::cursor_session`（`cc_session` の cursor 版）
- 置き場: `vp_state_dir()/cursor_sessions/<project>__<lane>`（1 lane 1 file 1 行 = chatId）
- chatId 検証: `[A-Za-z0-9_-]`（claude 版は `-` のみだが cursor の chatId 形式が未知なので `_` も許容）。
  `--resume '<id>'` の single-quote 埋め込みが injection にならないための防壁。

## fail-open 原則

`create-chat` の失敗（cursor-agent 不在 / timeout 10s 超過 / 出力が chatId 形式外）は
**すべて `None` に倒す**。 素の `cursor-agent`（新規チャット）起動に落ちるだけで lane は必ず成立する。

- `cursor_session::cursor_cli_path()`: launchd 起動 daemon の細い PATH でも create-chat を
  exec できるよう明示解決（`which cursor-agent` → `$HOME/.local/bin/cursor-agent` → 素の名前）。
  `agent::get_claude_cli_path` と同じ問題への対処。
- 未ログイン（`cursor-agent login` 未実施）は console に login プロンプトが出るだけで床は無事。
  ログイン状態は user 側の責務。

## fresh の扱い

- `fresh=true`（"New Session"）: `cursor_session::clear` して**素の `cursor-agent` を注入**する
  （create-chat は exec しない = fresh path を exec-free に保ち決定的にする）。 新しい chatId は
  次の非 fresh spawn で `ensure_chat_id` が採番し直す。
- `fresh=false`: `ensure_chat_id`（既存あれば再利用、 無ければ create-chat 採番）→
  `cursor-agent --resume '<id>' || cursor-agent`。

## v1 スコープ外

- **engine_model 連携（`--model` 注入）**: `engine_model` は claude alias 前提の state。
  cursor の model は cursor-agent TUI 内の `/model` で選ぶ。Act II の `console_set_model` も
  cursor lane では拒否する（「cursor エンジンの model は cursor-agent 側で選択します」）。
- **wire hooks（`--settings '{WIRE_HOOKS}'`）**: cursor に相当する hook 機構が無いため注入しない。

> Act II（GUI chat）は v2 で対応済み（下記）。当初は「Act I のみ」だったが、turn-scoped host を
> 足すことで Chat モードにも乗せた。

## Act II（v2）— turn-scoped CursorAgentHost

Cursor lane を Act II（GUI chat）でも駆動できるようにする。claude の `EchoesAgentHost`（常駐
stream-json host）とは別に、cursor 専用の **turn-scoped host**（`crate::echoes::cursor_host`）を
足す。GUI 語彙 `EchoesEvent` は engine 非依存なので、chatview / topic 配線 / vp-app は無改修。

### turn-scoped 化の理由

cursor-agent には claude の `--input-format stream-json`（常駐 stdin 連投）に相当する機構が**無い**。
よって「1 プロセス常駐 + stdin にメッセージを流し込む」形は取れず、**submit のたびに 1 プロセスを
spawn** する:

```
cursor-agent -p "<prompt>" [--resume '<chatId>'] --output-format stream-json --stream-partial-output --trust --force
```

prompt は positional arg（`Command::arg` 渡し = shell 非経由なので injection 安全）。`--resume` は
chatId 未記録時は付けない。`CursorAgentHost::spawn` は**プロセスを起動しない**（channel と状態を
用意するだけ = `ensure_chat_engine` を exec-free に保つ）。実プロセスは初回 submit で立つ。

`--trust` は workspace trust の自動付与（2026-07-16 dogfood で追加）: headless は trust prompt を
対話で出せず、cursor-agent 初見の workspace では `Workspace Trust Required` の stderr を残して
即死する。lane の cwd は user 自身が VP に登録した workspace なので、自動 trust は claude
（bypassPermissions）/ codex（full bypass）と同じ姿勢。Act I（TUI 床）は対話 prompt が出るため
付けない = user 判断のまま。

`--force`（= `--yolo`）は tool 承認 gate の一括開放（2026-07-16 P0 切り分けで追加）: headless は
`system/init` の `permissionMode` が `default` 固定で承認 prompt を出せず、**非 allowlist の Shell /
File deletion / MCP tool call が全て auto-block される**（実測。baseline = `--trust` のみで
`{"rejected":{"reason":"File deletion rejected"}}` / `{"rejected":{"reason":"User rejected MCP:
vp-*"}}` が返る — 人間不在なのに "User rejected" と誤表示される auto-block）。`--force` で success
到達を確認。`--approve-mcps` は server 承認レベルで per-call には効かない（実測で無効）。詳細と flag 別
差分表は `docs/guide/stand-smoke-matrix.md` の 2026-07-16 §2。⚠️ 権限拡大（deny 空なら実質全許可）。
Act I（TUI 床）は対話承認が効くため付けない。

### イベント翻訳表（`cursor_translate`）

| cursor stream-json | → EchoesEvent |
|---|---|
| `system/init` | `SessionInit{session_id, model, permission_mode, cwd, tools:[], mcp:[], slash:[]}` |
| `user`（送信 prompt のエコー） | 捨てる（GUI が optimistic user bubble を出す = claude と parity） |
| `assistant`（`timestamp_ms` 有 & `model_call_id` 無） | `MessageChunk{text}`（= 新規 delta） |
| `assistant`（`timestamp_ms` 無 / `model_call_id` 有） | 捨てる（バッファ全文 / tool 前 flush の重複） |
| `tool_call/started` | `ToolCall{id:call_id, name:key の末尾 "ToolCall" 落とし, input:args}` |
| `tool_call/completed` | `ToolCallUpdate{tool_use_id:call_id, content:result 文字列化, is_error:result に "success" 無し}` |
| `result`（success） | delta ゼロなら result 全文を先に `MessageChunk` → `TurnCompleted`（安全網） |
| `result`（is_error） | `Error{message}` |
| JSON でない行（未ログイン等） | イベント化せず内部 buffer（Error message の材料） |

`--stream-partial-output` の delta 判定（`timestamp_ms` 有 & `model_call_id` 無）が肝。これを外すと
全文が二重・三重に描画される。

### record-from-init（Act I ⇄ II 会話共有）

`system/init` の session_id（= chatId）を `cursor_session::record` で永続する。Act I（console）も
同じ state file（`cursor_sessions/<project>__<lane>`）を読むため、II で進めた会話は I へ、I で進めた
会話は II へ `--resume` で継がれる（claude の `record_session_from_init` と同型）。

### interrupt = kill

cursor には claude の control channel（`interrupt` control_request）が無い。中断は実行中の子プロセスの
kill で行う（turn task を abort → `kill_on_drop` が子を殺す）。中断後は `TurnCompleted` を broadcast
して streaming を畳む（意図的中断はエラーではないので Error バブルは出さない）。

### 偽 Error を出さない（claude host との差）

`EchoesAgentHost` は「stdout close = engine 途絶」で無条件に `Error` を broadcast する（常駐前提）。
cursor は turn ごとにプロセスが**正常に**終了するため、この論理を持ち込むと毎 turn 偽 Error が出る。
よって cursor host は `translator.saw_result()`（終端 `result` 観測 = 正常終端）が true なら**何もしない**。
false のときだけ（未ログイン / crash）Error を broadcast する。

### resume 失敗の self-heal（1 回）

`--resume` 付きで走らせて正常終端も実イベントも無かった（chatId が cursor 側で消えて resume 空振り）
場合に限り、chatId を破棄して resume 無しで同一 prompt を 1 回だけ再実行する（Act I の
`|| cursor-agent` fallback と同じ意味論）。再実行も失敗したら Error。

### nudge の意味論変化（走行中は queue）

turn 実行中の submit は即実行できない（1 turn 1 プロセス）。走行中の submit（wire nudge の mid-turn
注入含む）は queue に積み、現 turn 終了後に順次 spawn する。claude host は engine 側が queue するが、
cursor は host 側で queue する（観測される挙動は同じ = 次 turn で注入）。

### HITL / replay は非対応（空 chat 許容）

- **HITL（`Question` / `PermissionRequest`）**: cursor に control channel が無いので `respond_permission`
  / `set_permission_mode` は Err を返す（enum 側で bail）。承認は cursor-agent 側の挙動に委ねる。
- **transcript replay-on-attach**: cursor lane は `cc_session` を持たない（chatId は `cursor_session`
  側）ため、`handle_echoes_demand_start` は必ず no_session path に落ち、空 chat で attach する
  （`[ReplayStart, ReplayEnd{in_flight:false}]` だけ送る）。UI は破綻せず、会話は live event で進む。

### エンジン分岐の入口

- `LanePool::ensure_chat_engine`: `info.stand == "cursor"` で `CursorAgentHost`、それ以外は `EchoesAgentHost`。
  host は `ChatHost` enum（`Echoes` / `Cursor`）で束ね、pump / 各 `*_chat` メソッドは engine 非依存。
- `LanePool::set_console_mode`: Chat 許可を `stand ∈ {echoes, cursor}` に緩める。

## 既知の制約

- **fresh 直後の会話は respawn で継がれない**: fresh は chatId を採番せず素の cursor-agent を
  起動するため、 その会話は state に記録されない。 次の非 fresh spawn で新しい chatId が採番される。
- **stale chatId は毎回 `||` fallback に落ちる**: chatId が cursor 側で消えると `--resume` が
  失敗し、 素の cursor-agent（新規チャット）に倒れる。 fresh で明示的に解消できる。
- **performer の stand は SP 再起動をまたいで永続しない**: lane の `stand` は LanePool の
  in-memory `LaneInfo` にのみ持ち、 disk には per-lane 永続されない。 SP 再起動後に
  filesystem watcher / reconcile が disk 上の performer を respawn する際は `default_stand`
  （= echoes）に倒れる。 これは cursor 固有ではなく `"shell"` stand と共有する既存の性質で、
  per-lane stand 永続（`cc_session` / `engine_model` 同型の state file）は v1 スコープ外。
  cursor lane の canonical な作成経路は MCP `add_performer(stand="cursor")` / HTTP `lane_create`。
