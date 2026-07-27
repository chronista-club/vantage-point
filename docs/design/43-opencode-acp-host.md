> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 43 — OpenCode AcpHost（local LLM 正式化 = dev path step 7）

> **status**: 設計確定（2026-07-19、実測 de-risk 全 PASS → 本 doc → 実装）
> **発端**: dev path step 7（local LLM 正式化）。doc 39 §7-1 の 4 route から **B = OpenCode を
> AcpHost に挿す**を mako 選択（2026-07-19。「codex の設定複雑化を避け、local 専用の常駐 engine を
> 足す」）。doc 41 §3 が step 7 に予約した設計判断「model/provider の供給源」への回答を §3 に含む。

## 0. 一言で

**opencode（`opencode acp`）を 4 本目の常駐 engine にする。** grok で作った AcpAgentHost
（ACP pv1 / JSONL）がそのまま話せることを実測済み — 実装は「AcpHost の spawn を engine 別に
パラメタ化 + EngineKind::OpenCode」の薄い差分。local model（LM Studio 等）は **opencode 自身の
provider 設定**が担い、VP は model を一切注入しない。

## 1. 実測 wire 事実（2026-07-19、opencode 1.18.0 × LM Studio qwen2.5-coder-14b @32k）

probe 3 本（initialize / new→prompt / load→文脈確認、XDG 隔離 + bun driver。
scratchpad `opencode-probe/`）:

1. **`opencode acp` = JSONL / ACP protocolVersion 1** — grok（`grok agent stdio`）と同 protocol。
   `jsonrpc:"2.0"` field あり（grok と同じ標準形）。
2. capabilities: **`loadSession: true`** + `sessionCapabilities: {close, fork, list, resume}` +
   `promptCapabilities: {embeddedContext, image}` + mcp http/sse。
3. `session/new {cwd, mcpServers:[]}` → `sessionId: "ses_089ead04…"` + **`configOptions`**
   （`id:"model"` の select。currentValue に config 既定の `lmstudio/qwen/qwen2.5-coder-14b` が
   反映されていた = ACP レベルの model 切替口が存在する。将来素材、§6）。
4. **local provider は auth 不要**（`authMethods: opencode-login` は cloud 用。LM Studio 経路は
   一度も auth を要求されない）。
5. `session/prompt` → `agent_message_chunk` 通知 → response `{stopReason:"end_turn",
   usage:{inputTokens:9492,…}}`。**opencode の initial prompt ≈ 9.5k tokens** — LM Studio 側は
   **context length 32k+ で load 必須**（doc 41 §3 の教訓がそのまま適用。既定 4096 は溢れる）。
   turn 33s（14B 初回）/ 9s（2 回目、model 常駐後）。
6. **resume 実証**: 新プロセス → `session/load {sessionId, cwd, mcpServers}` 成功 → 文脈依存の
   質問（「さっきの答えに 1 を足すと？」）に正答 = プロセスを跨いで会話が蘇る。
7. **load は全履歴を replay する**: `user_message_chunk` / `agent_message_chunk` が発話順に全量
   再送される（2 往復 = 4 chunks で確認）。§5 の設計判断の入力。

Act I（TUI）側: `opencode -s <session-id>` で session 再開、`-c/--continue`（最新）、`--fork`、
`-m provider/model` あり（`--help` 確認）。

## 2. 設計 — AcpAgentHost の engine パラメタ化

doc 42 の AcpAgentHost は grok 専用の定数（CLI パス解決 / spawn args / ログ文言）を持つ。
これを **engine パラメタ（spawn command + 名乗り）に一般化**し、grok / opencode の 2 engine で
共有する。host 本体のロジック（session/new|load self-heal、prompt-response turn、
session/cancel、request_permission 自動 allow、dead flag 自己修復、stderr 診断）は不変。

| 項目 | grok | opencode |
|---|---|---|
| spawn | `grok agent stdio --always-approve` | `opencode acp`（approve 相当 flag なし — request_permission 自動 allow の一段のみ。§6） |
| CLI パス解決 | PATH → `~/.grok/bin/grok` | PATH → `/opt/homebrew/bin/opencode`（brew） |
| 会話 id 形式 | grok の sessionId | `ses_` prefix（`is_valid_conversation` に OpenCode arm） |
| model | grok 側管理 | **opencode config 管理（§3）— VP 注入なし** |

- **EngineKind::OpenCode** 追加（stand = `"opencode"`、`chat_capable = true`、chip prefix = **`oc`**）。
  対応表（from_stand / stand_name / 能力表 / stands 一覧 / webview prefix）は sweep 6.5 後の
  3 engine 表に 1 行足すだけ。
- 会話 id は registry 直結（doc 40 §4）。opencode に旧 store は存在しない（grok と同じ
  registry-native — backfill bridge の対象外という正しい欠如）。

## 3. model / provider の供給源 = **opencode config（VP は注入しない）**

doc 41 §3 が step 7 に予約した question への回答。VP は model を選ばない・運ばない・保存しない:

- SSOT は user の `~/.config/opencode/opencode.json`（provider 定義 + `"model"` 既定）。
  VP の spawn は素の `opencode acp` — env も config も注入ゼロ。
- 根拠: ①route B 選択の意図（VP 側の設定複雑化を避ける）②opencode は provider 抽象が本体機能
  （LM Studio / Ollama / llama.cpp / cloud を一元管理）で、VP が二重管理すると SSOT が割れる
  ③Act I（TUI）と Act II（acp）が**同じ config を読む** = 両 Act で model が一致する。
- dogfood 設定例（LM Studio）:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "lmstudio": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "LM Studio (local)",
      "options": { "baseURL": "http://127.0.0.1:1234/v1" },
      "models": { "qwen/qwen2.5-coder-14b": { "name": "Qwen2.5 Coder 14B" } }
    }
  },
  "model": "lmstudio/qwen/qwen2.5-coder-14b"
}
```

- LM Studio 側の前提（doc 41 §3 と同一）: `lms server start` + `lms load <model>
  --context-length 32768`。落ちている時は turn が engine Error になり、既存の途絶可視化
  （host pump 終了 → Error broadcast → chatview）に乗る — 専用ハンドリングは作らない。

## 4. Act I（床）

`build_stand_command` に opencode arm を追加 — claude と同型の resume type-ahead:

```
opencode -s <conversation> || opencode      # conversation 有り（resume）
opencode                                    # 無し（bare）
```

root の conversation（`ses_…`）は registry から（P1 で統一済みの読み先）。TUI が落ちても
`-s` で次 spawn が継ぐ（「プロセスは死ぬがコンテキストは蘇る」の opencode 版)。

## 5. Act II の会話復元 = **replay_log tap に OpenCode を追加**（engine-native replay は将来素材）

opencode は `session/load` が全履歴を replay する（§1-7）ため「engine-native 復元」も可能だが、
**採らない**。doc 42 §3 の既定（AcpHost は load の replay 列を捨て、VP の replay_log を
唯一の Act II 復元源にする）に揃える:

- 判定 3 点セット（lanes_state の tap / unison_server の reader・writer）を
  `Codex | Grok` → `Codex | Grok | OpenCode` に更新。**⚠️ #807 の教訓: 3 点 + tap 定義元
  （replay_log.rs の doc）を同一 commit で揃える**（片側更新は dead-write / dead-read を生む）。
- 理由: ①復元経路が engine ごとに割れると attach 側の分岐が増える（一枚岩に反する）
  ②load-replay 採用には「translator を load 中だけ繋ぐ」動的切替が要り、doc 42 §3 の
  二重化防止設計を逆転させる工事になる。
- **将来素材**: ACP engine が 2 本になったので「ACP 系は engine-native replay に寄せて
  replay_log を退役させる」最適化はあり得る。その時は grok も同時に移す（片方だけは作らない）。

## 6. 未実測・残課題

- **tool-use / permission flow**: probe は素の Q&A のみ。opencode の `session/request_permission`
  発火条件と自動 allow の実効は Act II dogfood で確認（AcpHost の自動 allow handler は
  engine 非依存なので配線は既にある）。opencode.json の `permission` 設定との関係も同枠。
- **ACP configOptions（model select）**: session 単位の model 切替口が wire に存在する（§1-3）。
  UI に出すかは dogfood の実感待ち（doc 39 P4 の engine gating と同じ「実感してから作る」枠）。
- **`--fork`（TUI）/ `fork` capability（ACP）**: doc 39 §6「既存会話から分岐して root にする」の
  将来素材。opencode は公式に持っている。
- OpenCode Zen（cloud model 群）が config に混ざる場合の挙動 — local 専用運用なら無関係、
  混在運用は dogfood で。

## 7. 段階

| 段 | 内容 |
|----|------|
| **PR-A** | AcpHostConfig の engine パラメタ化 + EngineKind::OpenCode + 対応表/chip + 床 arm + tap 3 点 + is_valid_conversation + テスト |
| dogfood | mako 実機（LM Studio 起動下で lane 作成 → Act I/II → resume → lane 切替復元） |
