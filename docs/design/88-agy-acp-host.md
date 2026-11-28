# doc 88 — Antigravity (agy) AcpHost: ACP 常駐統合

> **status**: Step 1・Step 2 実装済み（2026-11-17）。実機の GUI での確認は未（§7）。
> permission の要求の形（§1-6）は未実測
> **2026-11-20 更新**: host の実装を、VP の手書き（JSON-RPC を手で組む）から共有 crate
> `agent-chat-acp` の `AcpClientHost`（公式 SDK）に切り替えた（§8）。§1〜§6 の実測は当時の記録として
> 残す。実装の置き場が変わった所（§0 の翻訳、§6.2 の表の当て直しの行と細部、§6.3）と、履歴の扱いが
> 変わった所（§1-5、§5 の見出し）には、§8 への案内か今の置き場を書いてある
> **関連 memory**: `mem_1Ch7Ka5RWFt8kdRtvyqprD`（指示書）/ `mem_1Ch7JP6WKoFk7BprEUzqrM`（事前調査）

## 0. 一言で

**Google Antigravity CLI（`agy`）を `AcpAgentHost` の 3 本目の ACP 常駐 engine として復帰させる。**
2026-07（doc 39 §7、PR #807）に「会話 id 供給なし・常駐路なし」で一度撤去したが、agy 1.5.0 以降で
`agy acp`（ACP protocolVersion 1）と `--conversation <id>` が揃い、TUI（console）と GUI（chat）で
同じ会話 id を行き来する VP の常駐一枚岩モデルにそのまま乗ることを実測で確認した。

Step 1 では ACP コア統合に加え、`agy acp` が標準応答で返す **model / mode / slash commands /
context usage / edit diff** を `AcpAgentHost`・`AcpTranslator` で接続する（翻訳は 2026-11-20 から
共有 crate の `translate` → §8）。Step 2 で設定パネル
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
   - `session/load` 要求後、`result` 応答の**前**に過去ターンの `user_message_chunk` / `tool_call` / `agent_message_chunk` が流れる。当初の `AcpAgentHost` は `loading = true`（`result` 到着まで）の間の `session/update` を捨てて、`replay_log` と二重再生されないようにしていた。**2026-11-20 から、この流れを捨てずに履歴の正にする（→ §8）。**
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

## 5. GUI（chat）の会話復元 = replay_log tap（2026-11-20 まで。今は §8）

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
| 当て直し | 共有 crate の host（`HostOptions::config_intent`。2026-11-20 までは `AcpAgentHost::adopt_session` → `AcpState::next_reapply`） | session 確定後、registry の意図を **1 軸ずつ**送る。応答のたびに最新の configOptions で次の要否を決め直す（軸が消えた / 既定に戻った、を拾う）。**終わるまで prompt を送らない**。15 秒で打ち切る見張り付き |
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
  （**2026-11-20 から**: ⚠ の文は共有 host の `<軸>=<値>: <engine の文>` で、「選び直すと解消します」は
  付かない → §8.3）
- user の切替が 15 秒以内に返らなければ失敗として返し、遅れて届いた応答は捨てる（registry に書かずに
  engine だけ進む食い違いを作らない。engine が実際に変えた値は `config_option_update` で roster に出る）
  （**2026-11-20 から**: 失敗として返すのは同じ。遅れて届いた応答は、共有 host が設定面と意図に
  取り込む。時間切れは `acp_config.error` に残らない → §8.3）
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
（当時は `AcpState::note_commands` / `take_deferred_init`。2026-11-20 からは共有 host が同じ保留を持つ。
一覧が変わらなくても出し直す点が違う → §8.3）。実機の E2E で、event の並びが
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

> 上の 3 本（`e2e_real_agy_image_prompt` / `e2e_real_agy_acp_turn_roundtrip` /
> `e2e_real_agy_config_set_and_reapply`）と grok の `e2e_real_grok_acp_turn_roundtrip` の計 4 本は、
> 手書きの host と一緒に 2026-11-20 に消えた。agy の場面は
> 共有 crate の側の実機 E2E（`vendor/agent-chat/crates/acp/tests/agy_e2e.rs` — 新規 → load →
> 立て直し / 中断 / tool / 設定と画像）が持つ。VP の側には §8.6 の実機 E2E を新しく足した（engine ごとに 2 本 + agy に 1 本、計 7 本）。

## 7. 実機で見る手順（dev build）

1. agy の lane を作り、chat を開いて 1 往復する
2. 入力欄の下に Model / Thinking Level / Mode の select が出る。Thinking Level を替える → 表示が替わる
3. Model を替える → Thinking Level の候補が model に合わせて変わるか（Orchestrator / GPT-OSS では
   Thinking Level の select が消える）
4. chat ⇄ console を行き来する / daemon を再起動する → 選んだ値が戻っているか（当て直し）
5. 画像を貼って送る（Argon の枠が切れていたら Model を Gemini 3.8 Flash に替えてから）
6. 新しい lane の最初の送信で、応答中のサイドバーが idle に戻らないか（§6.3）

## 8. host を共有 crate に切り替えた（2026-11-20、Stage 2）

task `mem_1ChDFn6LxUfPDFsqeXvcyr`。設計の正は agent-chat の `docs/design/01-acp-client-host.md`
（`vendor/agent-chat`、revision `9516b29`）。ここには VP の側で決めたことだけを書く。grok と opencode
も同じ host なので、同時に切り替えた（doc 43 §5 の「片方だけは作らない」）。

### 8.1 何が変わったか

| 面 | 2026-11-20 まで | 今 |
|---|---|---|
| 接続・要求と応答・順序 | VP の `acp_host.rs` が JSON-RPC を手で組む | 共有 crate `agent-chat-acp` の `AcpClientHost`（公式 SDK `agent-client-protocol`） |
| `session/update` の翻訳 | VP の `acp_translate.rs` | 共有 crate の `translate`（VP の code を移したもの）。`acp_translate.rs` は削除 |
| 設定面の解釈と当て直し | VP の `acp_config.rs` の純関数と `AcpState` | 共有 crate の `config` と host。VP の `acp_config.rs` は roster の wire の型だけ（ts-rs の export のため。crate の型から `From` で写す） |
| **履歴** | `session/load` の流れを捨て、VP の `replay_log` から戻す | `session/load` の流れを host が控えに持ち、pump が配る（§8.2）。`replay_log` は使わない |
| VP の `acp_host.rs` | host の本体（1948 行） | 注入だけの薄い adapter: engine の表（CLI の path・引数・名乗り）、identity env、registry への会話 id の書き込み、load の失敗の方針、permission の方針、設定の意図、stderr の log |

結果として、**console（TUI）で進めた分が chat の画面に出る**作りになった（今までは `replay_log` に
載らないので出なかった。mako が 2026-11-17 に実機で確認した症状）。出るのは、chat の engine が
立ち直って `session/load` する時 — 同じ session で chat → console → chat と切り替えると、console に
替えた時に chat の engine が畳まれ（`repo/lane/reconcile.rs`）、chat に戻った時に立ち直る。
**実機の GUI では未確認**（§8.6 の手順 1）。

### 8.2 履歴の配り方 — engine の出力と履歴は pump だけが流す

共有 host の `attach()` は「控え」と「その続きの受け口」を同じ lock の中で返す。控えに入った event は
受け口に出ず、控えに無い event は必ず受け口に出る。

VP は pump が engine の出力を topic に流している（同じ topic には、`vp now` の自己申告と、起動の失敗の
`Error` も別の経路から流れるが、会話の本文と履歴を流すのは pump だけ）。`conversation_replay` が横から控えを読んで topic に
流すと、控えを取ってから `ReplayStart` が届くまでの間に pump が流した event が、消えるか重なる。
そこで **配り直しも pump がやる**（`repo/conversation_pump.rs`、型は `conversation/replay_source.rs`）:

1. chat を開く（`conversation_demand_start`）→ `replay_once` は pump に「配り直して」と頼むだけ
2. pump が `attach()` を呼ぶ（会話の確定を待つ間も、続きの event は流し続ける）
3. pump が `ReplayStart` → `SessionInit` → 控え → `ReplayEnd { in_flight }` を流す
4. pump が受け口を `attach()` の返したものに差し替える

細部:

- 配送は別の task に預けて終わりまで待つ。pump は engine を畳む時に abort されるので、直接流すと
  `ReplayStart` の後で止められて `ReplayEnd` が出ないことがある（長い履歴では tokio が途中で譲らせる）
- pump が取りこぼした時（broadcast の lag）も、同じ口で控えから配り直す
- 溜まった依頼は 1 回の配り直しにまとめる（起動時に demand が 3 つ重なっても、全量を 3 回は配らない）
- 配り直しの後、turn の状態（idle の判定に使う）を控えの `in_flight` に合わせる。受け口を差し替える
  時に、控えに入らない event（`TurnCompleted` など）を古い受け口と一緒に落とすことがあるため
  （最後に読んでから控えを取るまでの間に出た分。daemon は multi-thread の runtime）
- **履歴を取れなかった時（会話を確定できなかった）と、engine を起動できなかった時は、`ReplayStart` を
  送らない** — GUI はあれを見て会話表示を消すので、出ている会話まで消してしまう（Codex の履歴の
  失敗と同じ規律）。理由の `Error` は、確定の失敗なら共有 host が自分で出し（pump は engine の出力と
  して流すだけで、同じ理由をもう 1 件足さない）、起動の失敗なら `replay_once` が出す
- 配り直した控えが「turn の最中ではない」と言うのに、進みの投影（sidebar の「作業中」）が作業中の
  ままなら、終端に合わせる（上の「turn の状態」と同じ理由。応答の時刻は書かないので、その turn の
  応答の時刻が 1 回抜けることがある）
- **engine が終了した後に chat を開き直した時は、終了した host を畳んで作り直す**
  （`ensure_chat_engine` が `ChatHost::exited` = 共有 host の `is_finished()` を見る。agent-chat 設計書 01
  「呼び手の規則」— 終了した host はもう使えない）。作り直した host の load で履歴が戻る。今までは `replay_log` から戻っていた場面
- 控えが上限で切られた時は、省いた旨の `Notice` を足す。GUI は届いた `notice` を帯として描く
  （`chat-model.ts` / `chatview.tsx`、creo `mem_1ChDPjq74fG87DYtSn3ns3`）。`replay_start`〜`replay_end`
  の中で届いた通知は会話の先頭に置き、次の `replay_start` で消す。外で届いた通知は会話の外に置き、
  利用者が閉じるまで残す。同文が内外で届いた時は、閉じるまで残す側に揃えて重複させない。
  受信区間は再同期ローダーとは独立して持つ。`error` や focus・watchdog によるローダー解除では
  区間を閉じず、`engine_exited` では閉じる（`replay_end` 未達でも次の接続へ持ち越さない）。
  状態は GUI の session 単位で、アプリを再生成した後の復元はしない。Codex の既存の省略帯は別のまま。
  **届かなかった通知は表示できない**。接続時の通知は控えに残らず、購読前や受け口の差替え境界で
  欠ける可能性がある。配送の保証は別 task（creo `mem_1ChEnsFzeLhCWybLFfH85B`）で扱う。
- `ChatHost::in_flight()` / `commit_seq()` は Claude の transcript replay だけが読むので、ACP は既定値

控えの上限は共有 crate の既定（1 MiB / 800 event / 1 件 32 KiB）のまま。今までの `replay_log` の
上限は 2 MB で、1 件ごとの上限は無かった。agent-chat の実測では、3.4 MB / 12 turn の会話が既定の
上限で 12 turn とも残った（1 件 32 KiB を超えた分の先頭が落ちる）。VP の側では測り直していない。

### 8.3 注入（手書きの host と同じにした所、違う所）

| 注入 | 値 | 手書きの host との関係 |
|---|---|---|
| load に失敗した時 | `LoadFallback::NewSession`（新規の会話にし、新しい id が registry を上書きする） | 同じ。共有 host は加えて `Notice` を 1 件出す |
| permission | 許可の option（`allow_once` → `allow_always`）、無ければ先頭の option | 同じ向き。違いは 2 つ: 許可の option が複数ある時に、並び順でなく kind で選ぶ。option が 1 つも無い時は取り消す（手書きの host は `optionId: null` の「選んだ」を返していた） |
| 会話 id の永続 | 会話が確定したら registry に書く | 書くのは同じ。保存に失敗した時は `Error` が出る（手書きの host は log の warn だけ）。保存に失敗すると次の再開で会話を失うので、表に出す |
| 設定の意図 | agy だけ（`AgySettings::config_pairs`）。会話中の切替が成功したら、host がその値を今の意図として覚える | 同じ（立て直しの後の当て直しが、起動の時の値でなく今の値になる点は新しい） |
| stderr | 1 行ずつ `tracing::warn!` | 同じ |
| 中断 | `session/cancel` を送り、待たせていた発話も捨てる | 同じ（共有 crate を VP に合わせて直した、agent-chat PR #6）。加えて、engine が中断に待ち切り（既定 3 秒）まで答えなければ、process を止めて host を終了し、`TurnCompleted` → `EngineExited` を出す（2026-11-24、agy が thinking のまま固まって停止が効かなかった件。agent-chat PR #9。手書きの host にも無かった） |

変わった所:

- **engine が終了した時の文**: 「`<engine>` が休眠しました。次の送信で再開します。」から、共有 host の
  文（「engine は終了しています: …」）に変わった。stderr の末尾が付くのは今までと同じで、取り方が
  「最後の 5 行」から「最後の 4096 bytes」になった。自己修復（`submit` が Err → 再 ensure → retry）は同じ
- **会話中に、host が出していない発話が engine から来た時**: `UserMessage` として流す（今までは
  翻訳器が捨てていた）。stdio の 1 接続では起きないはずの場面
- **結果まで入った `tool_call`**（load の流れ）: `ToolCall` と `ToolCallUpdate` の 2 つにする
- 設定の切替の応答が上限（15 秒）までに来なかった時: Err を返すのは同じ。`acp_config.error` には
  残らない（手書きの host は残していた）。遅れて届いた応答は、共有 host が設定面と意図に取り込む
  （手書きの host は捨てていた）
- **履歴が出るのは、engine が会話を確定した後**（実測で 1.5〜2 秒）。今までは `replay_log` を engine と
  無関係にすぐ流していた。idle で寝かされた後に開き直すたびに、この待ちが入る
- **engine を起動できない・会話を確定できない時は、過去の会話が出ない**（理由の `Error` だけ）。
  今までは、engine が立たなくても `replay_log` から出ていた
- **会話の確定に上限が付いた**（共有 host の `establish_timeout`、既定 30 秒。手書きの host に上限は
  無かった）。超えると host は終了する（load の失敗の方針 = 新規の会話、には入らない）。load が
  30 秒を超える会話があるかは測っていない
- **turn の最中に、待ちの発話がある状態で配り直すと**、控えは「今の turn の途中までの答え → 待ちの
  発話」の並びになり、その後に届く今の turn の続きは、待ちの発話の下に別の吹き出しとして出る
  （次の配り直しで正しい順に戻る）。GUI からの送信は turn が閉じてから送られるので、起きるのは
  wire の nudge などで発話が積まれた時に、chat を開いたり lag が起きたりした場合
- **log**: 共有 host の警告（load の失敗で新規に替えた、など）には repo と lane が付かない（engine の
  終了と stop の log には VP が付ける）。設定の当て直し・切替の失敗は log に出ず、roster の
  `acp_config.error` にだけ残る（手書きの host は warn も出していた）
- 細かい違い: slash command の一覧が同じでも `SessionInit` を出し直す（今までは変わった時だけ。turn の
  最中は終わりまで保留するのは同じ）/ `SessionInit` が出るのが設定の当て直しの後になった / 当て直しに
  失敗した時の文から「（選び直すと解消します）」が消えた / 会話の確定の前の設定切替が、すぐ Err を
  返す代わりに確定を待つ / engine が終了した後の `interrupt` が Err を返す / engine の終了後は `pid` が None

共有 host の側で直してもらったもの（VP の自己レビューで見つけ、agent-chat に頼んだ。どれも
agent-chat の側で落ちる test を先に書いて確かめてある、PR #7 → `8ec120e`、PR #8 → `9516b29`）:

- **受け取ってまだ送っていない発話も、控えの末尾に入れる**（PR #8）。直す前は、会話の確定の前
  （engine が立ち上がる 1.5〜2 秒ほどの間）に送った発話が控えに無く、配り直しの `ReplayStart` が
  GUI の仮の吹き出しを消して、発話が画面から消えていた（答えの本文は前の答えの吹き出しに
  つながって出た）。待ちの発話がある間は `in_flight` が true（これから turn が走る）。VP の側は
  `prompt_sent_right_after_opening_survives_the_redelivery`（adapter + pump + 偽の agent。
  `8ec120e` では赤）と、実機の agy（設定の意図あり）で確かめた
- **`stop()` は、会話の確定や設定の当て直しを待っている最中でも、すぐに process を止める**。先に
  積んであった発話は送らない。直す前は、確定が済むまで（上限 30 秒 + 15 秒）process が生き、
  止めたはずの engine に prompt を送ってから止めていた（chat を開いた直後に mode や engine を
  切り替えると、同じ会話 id で process が 2 つ立ち得た）
- **engine の終了を見た時点で、`submit` は Err を返す**（VP の自己修復に入る）。直す前は、stderr の
  読み切りが済むまで（最大 1 秒）Ok を返して、発話を捨てていた
- **会話を確定できなかった時は、理由を `Error` で出してから `EngineExited` を出す**（待っていた
  発話があれば件数も）。engine が終了して、送られなかった発話があった時も `Error` が出る。
  `EngineExited` の文は GUI に出ない（「休眠」とだけ出る）ので、VP はその文を daemon の log にも出す
- `is_finished()`: host が終了したかを待たずに読む口（`ChatHost::exited` の実体）

残る違い: 会話の確定の前に送って、すぐ中断した時は、発話は engine に送られてから中断される
（手書きの host は、その場で待ちを捨てていたので送らなかった）。「chat を開いて 2 秒ほどの間に送って、
すぐ止めた」時だけ起きる。

残る窓: turn の最中に engine が終了した時、送ってしまった発話（答えが来ないまま）は「送られなかった
発話」に数えない。`EngineExited` だけが出る（`TurnCompleted` は出ないことがある。VP の pump は
`EngineExited` で turn の状態を下ろす）。

### 8.4 `restart()` の口（呼び手は無い）と、切替の順

生きている engine が、他の書き手（console）が進めた分を知っているかは、engine による。engine ごとの
実測（試験用の会話。2 人目は headless の 1 turn）。**列ごとに出どころが違う**:

- agy: agent-chat の設計書 01 の実測（2026-11-18）。生きた process で 1 turn → 別 process で 1 turn →
  生きた方に戻る。2 回測って同じ結果
- grok / opencode: lane acp-stage2 の実測（2026-11-20）。生きた process で 2 turn → 別 process で
  1 turn → 生きた方に戻る。**1 回ずつ**

| 確かめたこと | agy 1.5.11 | grok 1.0.89 | opencode 2.3.0 |
|---|---|---|---|
| 生きた process に `session/load` をもう一度 | 2 人目の turn は流れてこない | 流れてくる | 流れてくる |
| 生きた process で「覚えた合言葉を全部挙げて」 | 2 人目の分を答えない | 2 人目の分を答えない | **全部答える** |
| その後、新しい process で load | **2 人目の turn が消えている** | 残っている | 残っている |

（grok の「答えない」は 1 回の測定で、「知らない」のか「model が挙げなかっただけ」かは切り分けていない。
opencode が知っている理由は「process が共有の background service につなぐ作りだから」と考えられるが、
確かめていない。）

共有 host の `restart()`（process を立て直して load し直す）を `ChatHost::restart_engine` として
通してある。**呼び手は無いまま**。VP は「同じ会話を 2 つの process が同時に持たない」で守る
（mako の裁定 2026-11-20 — console に切り替えたら chat の engine を止め、chat に戻ったら新しい
process が load で読み直す。creo `mem_1ChA3fZy2Kq7tb3qaLSmpe`）。生きたまま他の書き手に進められる
場面を作らないので、`restart()` を呼ぶ場面が VP の中には無い。

#### 切替の順（2026-11-21）

chat → console の切替は、**chat の engine を止めて、process の終了を確かめてから、console の slot を
立てる**（`repo/lane/reconcile.rs` の ①、doc 53 §12.5）。それまでは slot を立てた後に engine を
止めていた。engine の種類では分けない（claude / codex / vpcode も同じ順）。終了を確かめるのは、
同じ reconcile が止めた engine について（別の reconcile が並んで走った時の窓は doc 53 §12.5）。

実測（lane acp-stage2、2026-11-21、agy 1.5.13、試験用の会話、**各 1 回** — 1 行目だけ 2 回流し、
1 回目は console の発話が folder の信頼の確認に食われて turn が起きなかったので数えていない。VP の daemon は通さず、
process の段で同じ段取りを再現した: `agy acp` で turn 1 を終える → login shell の pty を立てて 0.8 秒後に
`agy --conversation '<id>' || agy` を流し込む → console で 1 turn → console を畳む → 新しい
`agy acp` で load して数える）:

| 順 | chat 側の様子 | load の後に残った発話（turn 1 / chat の turn 2 / console の turn） |
|---|---|---|
| 以前の順（流し込んだ直後に kill、終了は待たない） | idle | 残る / — / 残る |
| 今の順（kill → 終了を確かめる → shell を立てる） | idle | 残る / — / 残る |
| 以前の順 | turn 2 の答えが流れている最中 | 残る / 残る（答えは途中まで） / 残る |
| 今の順 | turn 2 の答えが流れている最中 | 残る / 残る（答えは途中まで） / 残る |
| 重なりを 5 秒に広げた（VP の順ではない。console が読み込んだ後に kill） | turn 2 の最中 | 残る / 残る（答えは途中まで） / 残る |

- **どの順でも会話は欠けなかった**。順の直しは、実測で見つけた欠けを塞いだものではなく、原則に
  合わせたもの
- 以前の順でも、重なりは短かった: slot の spawn の 800ms は login shell が落ちないかを見る待ちで、
  command を流し込むのはその後。engine を止めるのは流し込んだ直後なので、console の agy が
  起動する頃には chat の process はもう居ない。重なりが伸びるのは、1 回の reconcile で slot を
  2 枚以上立てる時（後の slot の 800ms の間、先に流し込んだ agy と engine が並ぶ）
- kill から終了までは 3.5 ms と 8.8 ms（2 回）
- turn の最中に切り替えると、その turn の答えは途中で切れる（発話と、そこまでの答えは残る）。
  これは順に依らない
- §8.4 の表の「2 人目の turn が消える」は、生きた process が**他の書き手の後にもう 1 turn 送った**時の
  もの。止められた process は書かないので、上の 5 通りでは起きない
- 測っていないこと: VP の daemon を通した切替、grok / opencode / claude / codex / vpcode、
  tool の実行の最中の切替、繰り返した時のばらつき

### 8.5 共有 crate の log と、`serde_json` の `preserve_order`

host を共有 crate に替えた時に、host の外で効いた 2 つ。

- **log の target**: 共有 host の警告は target `agent_chat_acp` で出る（`session/load` の失敗で新規の
  会話に替えた、未対応の要求を断った、など）。daemon の log の filter は `vantage_point=…` しか
  持たなかったので、何もしないと log から消える（EnvFilter は、合う directive の無い target を出さない）。
  filter に `agent_chat_acp` を足した（`cli::vp_log_directive`、既定・`VP_LOG`・`VANTAGE_DEBUG`・
  settings.kdl の全経路）
- **`preserve_order`**: 公式 SDK が `serde_json` のこの feature を有効にする。binary 全体で
  `serde_json::Value` の object の key が「辞書順」から「入れた順」になる（release は vp-app と vp-cli を
  1 回で build するので、vp-app にも付く）。実害があったのは ROTO の setup の hash
  （`roto_setup.rs` — `Value` を bytes にして hash にかけ、保存値と比べて「書き直すか」を決める。並びが
  変わると、同じ setup でも実機への書き込みが 1 回走る）。hash にかける前に key を辞書順に並べ直して
  止めた（`roto_setup::canonical`）。調べた範囲と見ていない範囲は切り替えの PR（#1271）に書いた

### 8.6 検証

- 偽の ACP agent（`conversation/acp_host/testing.rs` — SDK の `Channel::duplex()` の片側で生の
  JSON-RPC を返す。turn の中身をテストが 1 手ずつ進める）で、host の adapter と pump を通す
- 実機 E2E（`#[ignore]`）: 新規 → 送信 → engine を止める → 同じ会話 id で立て直す → 履歴が engine
  から戻る。3 engine に 2 本ずつ（答えるだけの turn / file を 1 つ作らせる turn）

```sh
VP_AGY_E2E_MODEL=gemini-3.8-flash cargo test -p vantage-point --lib e2e_real_agy -- --ignored --nocapture --test-threads=1
cargo test -p vantage-point --lib e2e_real_grok -- --ignored --nocapture --test-threads=1
VP_OPENCODE_E2E_MODEL=opencode/big-pickle cargo test -p vantage-point --lib e2e_real_opencode -- --ignored --nocapture --test-threads=1
```

agy には、立て直した直後（会話の確定の前）に送った発話が控えの末尾に入ることを見る 1 本もある
（`e2e_real_agy_prompt_right_after_respawn_is_in_history`）。

2026-11-20 に回した結果（agy 1.5.11 / grok 1.0.89 / opencode 2.3.0。revision `6e93984`・`8ec120e`・
`9516b29` で 1 回ずつ）: 6 本とも通過。
立て直しで pid が替わり、発話と答えが控えに戻った。tool を使う turn では、3 つとも控えに `ToolCall` 1 件と
`ToolCallUpdate` 1 件が入った（load の流れの形は engine で違う — agy / grok は結果まで入った `tool_call`
1 件、opencode は `tool_call`（pending）+ `tool_call_update` の 2 段 — が、控えでは同じ 1 組になる）。設定面の軸は agy = model / thought_level / mode、
grok = model / reasoning_effort、opencode = model / mode（`opencode/big-pickle` の時。既定の model
`lmstudio/…` の時は model / effort / mode の 3 軸 — 軸の数は model による）。

実機の GUI で見る手順（未確認）:

1. agy の lane で chat を開いて 1 往復 → console に切り替えて 1 往復 → chat に戻る。**console の分が
   chat に出るか**
2. chat を開いたまま daemon を再起動する → 履歴が戻るか（tool の結果、thinking も）
3. turn の最中に別の lane へ行って戻る → 途中までの本文が欠けず、続きが重ならないか
4. agy の設定の切替（Model / Thinking Level / Mode）と、画像つきの送信
5. grok と opencode で 1 往復ずつ、開き直して履歴が戻るか
6. 長い会話（tool の結果が大きいもの）を開く → 省いた旨の表示が出るか、出方は妥当か


## 9. Status log — 構造化質問・初回 command・SDK 5.6.1（2026-11-28）

Spec `mem_1ChUb85e8Zxt8vYkyUonFe` / Todo `mem_1ChUb9syhgm8EsWKkcr5c8`。共有 crate は agent-chat #12
（`e42a9ba`）と follow-up #13（`8e9235c`）・#14（`c7ea200`）、設計の正本は agent-chat の `docs/design/01-acp-client-host.md`。ここには VP 側の接続だけを書く。

- **構造化質問（elicitation）**: engine の `elicitation/create`（`mode: "form"`）を共有 host が
  `ConversationEvent::Question` に写す。GUI は claude の AskUserQuestion と同じ PromptCard で描き、回答は
  `conversation_respond` → `LanePool::respond_permission_chat` → `ChatHost::respond_permission` →
  `AcpAgentHost::respond_permission` と戻る。これまで ACP の 4 engine（grok / opencode / agy / muse）は
  ここで「対話承認を持たない」と Err にしていた。
  - `PermissionDecision::Allow { answers }` → `accept` + content（回答 map を共有 host が form の
    property 名へ戻す）。`Deny { message }` → `cancel`。ACP の elicitation に拒否理由の欄は無いので
    `message` は engine に届かない
  - URL mode と未対応の型は共有 host が invalid_params で断る。質問は履歴に残さない（制御面）
  - tool の承認（`session/request_permission`）は従来どおり host の政策 `auto_allow` が答え、PromptCard
    には来ない。`set_permission_mode` は ACP engine では引き続き Err
  - **答える前に打ち切られた質問**: 共有 host は中断・turn の終わり・立て直しで、残っている質問に
    `cancel` を返す。中断・turn の終わりでは id ごとに `PermissionCancelled` も出し（agent-chat #14）、
    GUI（`chat-model.ts`）はその id のカードを「回答の前に打ち切られました」として閉じる。立て直し・
    停止では出ないので、`engine_exited` で未回答のカードを全部閉じる（process が終われば、どの engine
    でも答え先が無い）。
    ⚠️ turn の終わり（`turn_completed` / `error`）だけでは閉じない — claude は turn をまたいで承認を
    待つ（background subagent の `can_use_tool`）ので、閉じると答えられなくなる（santa round 2 で
    一律に閉じる案を退けた）。Question / PermissionRequest は replay log に保存されないので、復元で過去の
    カードを偽って閉じることも無い。
    それでも届かなかった回答（Err）では、`respond_permission_chat` が**自分で立てた** `turn_active` だけを
    下ろす（立てたままだと idle sweep から外れる。元が true なら、pump が turn の終わりで下ろした値を
    上書きしないよう触らない）
- **初回 slash command**: `initialize` / `session/new` / `session/load` の応答（`_meta.availableCommands`
  を含む）と、`session/new` 直後の `available_commands_update` を、最初の `SessionInit.slash_commands`
  に載せる（新規 session で command がまだ無ければ合計 50 ms まで待つ）。§6.3 の「turn の終わりに
  回す」更新はそのまま
- **SDK 5.6.1**: `agent-client-protocol` 5.3 → 5.6.1。`initialize` で client_info と capabilities
  （`elicitation.form` / `session.notices` / `session.compaction` / `plan`）を名乗る。`auth.terminal` は
  起動導線が無いので false。`notice` / `compaction_update` は `Notice`、`plan_update` / `plan_removed`
  は `Plan` に写す。config の `category` を保持する。tool の表示名はこれまでどおり `title` が先で、
  programmatic な `name` は `title` が無い時だけ使う（agent-chat #13。`name` を先にすると agy の now-line
  が `ls` → `run_command` に痩せる）
- **configId の落とし穴**: SDK の v1 型は `configOptions[*].id` 必須で、配列は `VecSkipError`。v2 形の
  `configId` / `groupId` で返す engine だと、設定の軸が**エラー無しで 0 件**になる。共有 host が応答の
  dispatch（型に通す前）で `id` / `group` を補う（`config::normalize_v1_config_options`）

検証: `conversation::acp_host::tests::elicitation_reaches_question_and_answer_returns_to_engine`
（偽の agent で agy = Allow / grok = Deny を ChatHost 経由で往復させ、turn の完了と使用済み id の Err
まで見る）、`repo::lane::pool::tests::failed_respond_restores_turn_active`、`chatview.test.ts` の
打ち切りの 5 本（`permission_cancelled` / `turn_completed` / `error` / `engine_exited` / 回答済み）。共有 crate 側は `tests/elicitation.rs` / `stabilized.rs` / `fake_agent.rs`。

**未確認（実機）**: 実 CLI（agy / grok）が `elicitation/create` を実際に送る場面と、その時の PromptCard の
見え方・回答後に turn が続くこと。手順: agy の lane で chat を開き、engine に選択を求める依頼
（例「赤と青のどちらで作るか私に聞いてから進めて」）を送る → PromptCard が出るか → 選んで turn が続くか。
engine が elicitation を使わず本文で聞き返すだけなら、この経路はまだ通らない。
