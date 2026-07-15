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

- **Act II（GUI chat / stream-json host）**: `EchoesAgentHost` は claude stream-json 専用。
  cursor lane が Chat モードに切り替わろうとしたら `set_console_mode` で明示拒否する
  （「cursor エンジンは Act I (console) のみ対応です」）。 claude が誤 spawn されない事を保証する。
- **engine_model 連携（`--model` 注入）**: `engine_model` は claude alias 前提の state。
  cursor の model は cursor-agent TUI 内の `/model` で選ぶ。
- **wire hooks（`--settings '{WIRE_HOOKS}'`）**: cursor に相当する hook 機構が無いため注入しない。

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
