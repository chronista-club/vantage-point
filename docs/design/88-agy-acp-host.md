# doc 88 — Antigravity (agy) AcpHost: ACP 常駐統合

> **status**: Step 1・Step 2 実装済み（2026-11-17）。実機の GUI での確認は未（§7）。
> permission の要求の形（§1-6）は未実測
> **関連 memory**: `mem_1Ch7Ka5RWFt8kdRtvyqprD`（指示書）/ `mem_1Ch7JP6WKoFk7BprEUzqrM`（事前調査）

## 0. 一言で

**Google Antigravity CLI（`agy`）を `AcpAgentHost` の 3 本目の ACP 常駐 engine として復帰させる。**
2026-07（doc 39 §7、PR #807）に「会話 id 供給なし・常駐路なし」で一度撤去したが、agy 1.5.0 以降で
`agy acp`（ACP protocolVersion 1）と `--conversation <id>` が揃い、TUI（console）と GUI（chat）で
同じ会話 id を行き来する VP の常駐一枚岩モデルにそのまま乗ることを実測で確認した。

Step 1 では ACP コア統合に加え、`agy acp` が標準応答で返す **model / mode / slash commands /
context usage / edit diff** を `AcpAgentHost`・`AcpTranslator` で接続する。Step 2 で設定パネル
（`session/set_config_option` による Model・Thinking Level・Mode 切替）と画像入力を積む（§6）。
目標は Grok / Codex の chat と同じ水準で日常に使えること（mako 2026-11-17: agy は平易な文章を
書くので、ドキュメントまわりで使う）。

## 1. 実測 wire 事実（2026-11-17、agy 1.5.9、`/opt/homebrew/bin/agy`）

1. **入口**: `agy acp`（JSONL over stdio、`jsonrpc: "2.0"` 付き ACP protocolVersion 1）。
   `agentInfo`: `{"name": "Antigravity ACP Server", "version": "1.5.9"}`。
2. **capabilities**:
   - `loadSession: true`、`sessionCapabilities: {resume, close, load, list, additionalDirectories}`
   - `promptCapabilities: {image: true, audio: false, embeddedContext: true}`
3. **`session/new` / `session/load` 応答**:
   - `sessionId`: UUID v4 形式（例: `"29fa9ed4-cd23-429f-9a63-a5d490b0d9b3"`）
   - `models`: `currentModelId`（例: `"gemini-4-argon-medium"`）+ `availableModels`
   - `modes`: `currentModeId`（`"default"` / `"accept-edits"` / `"auto"` / `"plan"`）+ `availableModes`
   - `configOptions`: `model` / `thought_level` / `mode` の 3 軸 select
4. **`session/update` 通知**:
   - `available_commands_update`: `availableCommands: [{name, description}]`（builtin slash command + skill 一覧）
   - `usage_update`: `{used: 9546, size: 1048576}`（現在の context tokens と window 総量）
   - `user_message_chunk`: 投入プロンプトの echo（`AcpTranslator` は無視するため ChatView の optimistic bubble と二重にならない）
   - `session_info_update`: `{title: "..."}`（自動生成タイトル）
   - `tool_call` / `tool_call_update`:
     - コマンド実行（`run_command`）は `content: [{type: "content", content: {type: "text", text: "..."}}]`
     - ファイル編集（`write_file` / `edit_file`）は `content: [{type: "diff", path: "...", oldText?: "...", newText: "..."}]`
5. **`session/load` の過去通知順序**:
   - `session/load` 要求後、`result` 応答の**前**に過去ターンの `user_message_chunk` / `tool_call` / `agent_message_chunk` が流れる。`AcpAgentHost` は `loading = true`（`result` 到着まで）の間の `session/update` を捨てるため、`replay_log` と二重再生されない。
   - `result` 応答の**直後**に `available_commands_update`・`usage_update`・`session_info_update` が届く。
6. **permission と起動フラグ**:
   - `~/.gemini/antigravity-cli/settings.json` の `"toolPermission": "always-proceed"` 環境では素の `agy acp` でも承認要求なしで tool が走る。
   - 未設定環境での停止を防ぐため、Grok（`--always-approve`）と同様に **`agy acp --dangerously-skip-permissions` + `AcpAgentHost` の `session/request_permission` 自動 allow の二段防御**で揃える。
   - ⚠️ **未実測**: 測った Mac が always-proceed だったため、flag の有無で要求が来るかを区別できていない。
     自動 allow が agy の要求の形（options の kind / optionId）で通るかも未確認。flag が `agy acp` の後ろで
     受理されることは `agy help acp` で確認した
7. **TUI との会話 ID 共用**:
   - ACP で採番した `sessionId` を `agy --conversation '<id>'` に渡すと同一会話を再開できる。

## 2. 設計 — AcpEngine::Agy と EngineKind::Agy（Step 1）

| 項目 | grok | opencode | agy |
|---|---|---|---|
| spawn | `grok agent --always-approve stdio` | `opencode acp` | `agy acp --dangerously-skip-permissions` |
| CLI パス解決 | PATH → `~/.grok/bin/grok` | PATH → `/opt/homebrew/bin/opencode` | PATH → `/opt/homebrew/bin/agy` |
| 会話 id 形式 | UUID（英数+ハイフン） | `ses_` + 英数字 | UUID（英数+ハイフン） |
| TUI resume | `grok -r '<id>' \|\| grok` | `opencode -s '<id>' \|\| opencode` | `agy --conversation '<id>' \|\| agy` |
| chip prefix | `grok` | `oc` | `agy` |
| Sidebar / Pane icon | `mingcute:grok-line` / `-fill` | `ph:code` / `-fill` | `material-symbols:antigravity-outline` / `antigravity` |

## 3. ACP Wire メタ情報の接続（Step 1）

1. **`SessionInit` の充実と replay 保持**:
   - `session/new`・`session/load` の `result` から `models.currentModelId`（fallback: `configOptions[id=="model"].currentValue`）と `modes.currentModeId` を抽出し、`SessionInit { model, permission_mode, .. }` に載せる。
   - `available_commands_update` 到着時に `slash_commands` と `command_docs` を更新して `SessionInit` を再送し（turn 中は終わりまで保留 — §6.3）、`AcpAgentHost::session_init()` に保持する（GUI 再接続時の `splice_session_init` で `/` 補完と実測 model 表示が復元される）。
2. **Context ゲージ**:
   - `usage_update` の `{used, size}` を `AcpState` に記録し、turn 完了時の `TurnCompleted { context_tokens, context_window, .. }` に渡す。
   - 実測（Gemini 3.8 Flash、2026-11-17）: その turn の `usage_update`（`used: 9264`）は prompt の応答の
     **前**に届く → `TurnCompleted` にはその turn の値が載る。prompt の応答の result にも
     `usage`（inputTokens / outputTokens / thoughtTokens / totalTokens …）が入るが、VP は使っていない
3. **Edit diff 結果の翻訳**:
   - `AcpTranslator` の `tool_call_update` で `type: "diff"` ブロック（`path` / `oldText` / `newText`）もテキスト化し、ファイル編集ツールの詳細展開で変更内容を読めるようにする。

## 4. TUI（console）側の起動

`agent_spawner::build_agent_command_for_session` に `EngineKind::Agy` arm（`agy_command`）を追加する:

```
agy --conversation '<id>' || agy    # conversation 有り（指名 resume + 消失時 fallback）
agy                                 # 無し（新規会話）
```

ACP 側で採番・記録された `sessionId`（UUID v4）を `session_registry` 経由でそのまま引き継ぐ。

## 5. GUI（chat）の会話復元 = replay_log tap

`agy acp` の `session/load` は過去履歴を `session/update` 通知で再送するが（§1-5）、doc 42 §3・doc 43 §5 と同じ規律で `load` 完了まで翻訳を繋げず、VP 自前の `replay_log` を唯一の GUI 復元源にする（`pool.rs` / `conversation_replay.rs` / `conversation_ops.rs` / `replay_log.rs` の 4 点同時更新）。

## 6. Step 2 — 設定パネルと画像（2026-11-17）

### 6.1 実測（agy 1.5.9、prompt は送らずに測った）

- `configOptions` は 3 軸。どれも `type: "select"`:
  - `model`（例 `gemini-4-argon`。`models.currentModelId` の `gemini-4-argon-medium` とは粒度が違う —
    model × thought_level の組が `models` 側の id）
  - `thought_level`（`low` / `medium` / `high`）
  - `mode`（`default` / `accept-edits` / `auto` / `plan`、`description` 付き）
- `session/set_config_option` `{sessionId, configId, value}` の result は **configOptions の全体**。
  続けて `session/update` の `config_option_update`（全体）、mode なら `current_mode_update`、
  さらに同じ `available_commands_update` が届く
- 不正値は JSON-RPC error `-32602`、文は `Thinking level "…" is not offered for model "…"`
  — **thought_level の候補は model で変わる**:
  - `gemini-4-hydrogen` は `xhigh` が増える
  - `gemini-pro-orchestrator-g1-pro` と `gpt-oss-120b-medium` は **thought_level の軸ごと無い**
  - 軸の無い model を経由して別の model に替えると、thought_level は既定（medium）に戻った。
    argon → hydrogen では low のまま残った。新しい session で argon → `gemini-3.8-flash` に替えると `high` になった
- 応答を待たずに 2 本続けて送っても、1 回の測定では順に処理された（保証は無いので、VP は頼らない）
- **切り替えた値は `session/load` の後に残らない**（別プロセスで load すると thought_level と mode が
  既定に戻った）

### 6.2 設計

| 要素 | 置き場 | 役割 |
|---|---|---|
| `AgySettings { model?, thought_level?, mode? }` | `conversation/agy_settings.rs` | user が選んだ値だけを registry（`{"agy": …}`）に残す。未選択の軸は engine 既定 |
| `AcpConfigView`（configOptions を写した型） | `conversation/acp_config.rs` | engine が出す軸と候補。VP は固定の表を持たない |
| 当て直し | `AcpAgentHost::adopt_session` → `AcpState::next_reapply` | session 確定後、registry の意図を **1 軸ずつ**送る。応答のたびに最新の configOptions で次の要否を決め直す（軸が消えた / 既定に戻った、を拾う）。**終わるまで prompt を送らない**。15 秒で打ち切る見張り付き |
| user の切替 | `conversation_set_settings` の `Agy` の腕 → `AcpConfigHandle::set` | 軸ごとに送り、engine の応答を待ってから registry に足す。pool の lock は待つ前に離す。registry は `update_settings`（読みと書きを 1 回の lock の中で）で書く |
| GUI への配送 | roster（`LaneSessionView.acp_config`） | `ConversationEvent` は共有の agent-chat にあるので種類を増やさない。roster は状態なので GUI を開き直しても復元される |
| panel | `webview/agy-settings-panel.tsx` | 軸ごとの select。現在値は engine の実測（応答を待ってから roster が返るので intent 優先は要らない） |

細部:

- model を先に当てる（thought_level の候補が model で変わるため）。user の切替の後は、選んである軸を
  engine の現在値に揃える（`AgySettings::follow_engine` — model の切替で thought_level が既定に戻った時、
  registry が古い値のままだと次の spawn で「勝手に変わる」）。engine に軸が無い時は意図を残す
- 失敗（engine のエラー文）は `acp_config.error` に残り、panel に ⚠ として出る（全文は title）。次の成功で消える
- **当て直しの失敗は registry から消さない**。使える model は契約や枠で変わり得る
  （`initialize` の `_meta.antigravity.tier`）ので、一時的に出ない値を自動で消すと user の選択が黙って
  失われる。⚠ に「選び直すと解消します」と出し、選び直しで上書きする
- user の切替が 15 秒以内に返らなければ失敗として返し、遅れて届いた応答は捨てる（registry に書かずに
  engine だけ進む食い違いを作らない。engine が実際に変えた値は `config_option_update` で roster に出る）
- panel の「送信中」は、この session の設定面の**中身**が変わった時に解く（roster は全 lane 共有の
  signal なので、別 lane の更新で解かない）。中身が変わらない失敗の繰り返しは 20 秒で解く
- roster の供給は既存の 2 点（`build_lanes_snapshot` / `emit_lane_update`）。どちらも
  `LanePool::acp_configs` を `apply_acp_configs` で投影する。vp-app は `settings` と同じく透過する
- engine が動いていない session への切替は Err（候補は engine が出すので、開いてからでないと検証できない
  — Codex の `configure_codex` と同じ）。panel は engine が動いている間しか出ない

### 6.3 slash command の更新を turn の終わりへ回す

`available_commands_update` は SessionInit を出し直す（§3-1）。ただし GUI は SessionInit を
「engine が立った」合図として扱い、活動表示を idle に戻す（`session-activity-bridge.ts`、
`console.ts` の `vp:console-ready`）。新しい lane の最初の送信では、溜めた prompt を送った直後に
この通知が来る。そこで **turn の途中に来た変更は保留し、`TurnCompleted` の直前に 1 回だけ出す**
（`AcpState::note_commands` / `take_deferred_init`）。実機の E2E で、event の並びが
SessionInit → MessageChunk → SessionInit（保留分）→ TurnCompleted になることを確かめた。

### 6.4 画像

`promptCapabilities.image: true` に合わせ、`EngineKind::image_capable` に agy を入れた。prompt は
text の後ろに ACP の image block `{type: "image", mimeType, data}` を並べる。VP は画像を保存しない
（`docs/spec/chat-image-input.md` の規律のまま）。

**実機で確認済み**（2026-11-17、Gemini 3.8 Flash）: 8×8 の赤 PNG を添えて色を聞くと `"red"` と答えた。

- agy の**使用枠は model ごとに別**だった。既定の Gemini 4 Argon は枠切れ（`Individual quota reached …`。
  枠切れの文は `agent_message_chunk` で届き、prompt の応答は `end_turn` — error にはならない）、Flash は応答した
- 実機 E2E の model は `VP_AGY_E2E_MODEL` で選べる（registry の意図と同じ経路 = spawn 後の当て直しで
  切り替わり、終わるまで prompt を待たせる。その経路の確認も兼ねる）:
  `VP_AGY_E2E_MODEL=gemini-3.8-flash cargo test -p vantage-point --lib -- e2e_real_agy_image_prompt e2e_real_agy_acp_turn_roundtrip --ignored`

設定まわりは `e2e_real_agy_config_set_and_reapply`（prompt を送らない）で実機の agy に当ててある:
切替 → 不正値の失敗 → model と thought_level の切替 → 別プロセスの load で 2 軸とも当て直される。

## 7. 実機で見る手順（dev build）

1. agy の lane を作り、chat を開いて 1 往復する
2. 入力欄の下に Model / Thinking Level / Mode の select が出る。Thinking Level を替える → 表示が替わる
3. Model を替える → Thinking Level の候補が model に合わせて変わるか（Orchestrator / GPT-OSS では
   Thinking Level の select が消える）
4. chat ⇄ console を行き来する / daemon を再起動する → 選んだ値が戻っているか（当て直し）
5. 画像を貼って送る（Argon の枠が切れていたら Model を Gemini 3.8 Flash に替えてから）
6. 新しい lane の最初の送信で、応答中のサイドバーが idle に戻らないか（§6.3）
