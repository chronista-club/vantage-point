# doc 41 — CodexRpcHost: codex app-server 常駐統合（dev path step 3）

> 2026-07-18、mako + Fable 5 conductor session。doc 39 §7「対応 engine は常駐型のみ
> （claude / codex=app-server / grok=ACP）の一枚岩」の codex 実装編。
> 実測 de-risk（§1、全 PASS）を先に済ませてから設計を固定した（facts over narrative）。

## 0. TL;DR

codex の Act II を turn-scoped（`codex exec` を turn ごと spawn する TurnHost）から
**常駐 RpcHost（`codex app-server` 子プロセス + JSONL JSON-RPC）**に置き換える。
1 session = 1 app-server 子プロセス（EchoesAgentHost と同じ per-session 常駐形）。
会話 id（thread id）の記録は doc 40 の registry 直結（`set_conversation`）。
codex 移行完了で TurnHost の残る客は cursor のみ → step 4 で TurnHost 系ごと撤去（doc 39 §7）。

## 1. 実測 de-risk（2026-07-18、codex-cli 0.144.5、ChatGPT auth、全 PASS）

scratchpad の bun script で `codex app-server`（stdio）と直接会話して検証:

| Phase | 内容 | 結果 |
|-------|------|------|
| A/B | initialize → initialized → `thread/start`（approvalPolicy=never + sandbox=danger-full-access）→ `turn/start` → stream → `turn/completed` | **PASS** |
| C | 同一プロセス 2nd turn — thread 文脈保持（常駐の核） | **PASS**（turn1 の指示語を turn2 が正答） |
| D | プロセス kill → **新プロセスで `thread/resume`** → 文脈継続 | **PASS**（= VP の SP 再起動シナリオが成立） |

確定した protocol 事実:

- **枠組**: JSONL over stdio。JSON-RPC 2.0 だが **`jsonrpc` field は wire で省略**（README 明記）。
  request = `{id, method, params}` / response = `{id, result|error}` / notification = `{method, params}`
- **handshake**: `initialize`（clientInfo 必須 — Compliance Logs で client 識別に使われる）→
  response → `initialized` notification。以後の request が解禁
- **thread id** = UUID v7 形（例 `019f7207-7392-…`）。`thread/start` / `thread/resume` の
  response `result.thread.id`。既存の `codex_session::is_valid_thread_id`（英数+ハイフン）で通る
- **turn 駆動**: `turn/start {threadId, input:[{type:"text", text}]}` → 即 response（turn obj）→
  `turn/started` → item 通知列 → `turn/completed {turn.status: completed|interrupted, error}`
- **観測された通知語彙**（翻訳層の素材、実 wire から）:
  `thread/started` / `thread/status/changed {status.type: active|idle}` / `turn/started` /
  `item/started` / `item/completed`（item.type: `userMessage` / `agentMessage {text, phase}`）/
  `item/agentMessage/delta {delta}` / `thread/tokenUsage/updated {tokenUsage.total/.last}` /
  `account/rateLimits/updated {rateLimits.primary.usedPercent, planType}` /
  `mcpServer/startupStatus/updated` / `turn/completed`
- **approval 無効化**: thread/start の `approvalPolicy: "never"` + `sandbox: "danger-full-access"`
  = 現行 `--dangerously-bypass-approvals-and-sandbox` の等価（claude bypassPermissions と同じ
  Act I/II parity ポリシー、[[echoes-act2-parity]]）。approval server→client request は
  never では飛んでこない（headless 安全）
- **schema の正**: `codex app-server generate-json-schema --out DIR` が**インストール済み binary
  から**生成される = version ズレなしの ground truth（実装時の参照は生成物を使う）

## 2. 設計 — CodexRpcHost

### 2-1. プロセスモデル: per-session 常駐（1 session = 1 app-server 子）

app-server は 1 接続で複数 thread を多重化できるが、**採らない**:

- VP の chat engine 所有は per-session slot（`chat_engines[addr][key]`、drop = 停止）。
  per-SP 共有プロセスは routing 層（threadId→session 配信）と lifecycle 所有の複雑さを足す
- per-session なら **EchoesAgentHost（claude 常駐 stream-json）と完全に同型** — spawn / pump /
  broadcast / in_flight / stop の既存パターンをそのまま流用できる（pre-MVP 最適パス）
- プロセス数が実測で問題になったら多重化はいつでも寄せられる（先に予約しない）

### 2-2. lifecycle（EchoesAgentHost 対応表）

| 段 | claude（EchoesAgentHost） | codex（CodexRpcHost） |
|----|--------------------------|----------------------|
| spawn | `claude -p --input-format stream-json [--resume id]` | `codex app-server`（stdio）→ `initialize`/`initialized` → conversation あり: `thread/resume {threadId, cwd, approvalPolicy:"never", sandbox:"danger-full-access"}` / なし: `thread/start {cwd, 同}` |
| session id 捕捉 | stream の `SessionInit` | `thread/start\|resume` response の `thread.id` → `EchoesEvent::SessionInit` emit + **registry `set_conversation`（doc 40 §4 — 新 host は registry 直結、codex_session store は書かない）** |
| submit | stdin に user message JSONL | `turn/start {threadId, input:[{type:"text",text}]}`（送信は request/response で ack される） |
| stream | stdout JSONL → translate | notification JSONL → translate（§2-3） |
| interrupt | プロセス kill（現行） | **`turn/interrupt {threadId, turnId}`** — 会話プロセスを殺さず turn だけ止める（TurnHost の kill より上等。turnId は `turn/started` で保持） |
| 停止 | Child kill_on_drop | 同（Drop で子プロセス kill。thread は disk rollout に残る = resume 可能） |
| self-heal | resume 失敗 → fresh 落ち | `thread/resume` error → `thread/start` に倒す（TurnHost の「no rollout found → 記録破棄」と同じ流儀。doc 40 の conversation は新 thread id で上書き） |

### 2-3. 翻訳表（app-server notification → EchoesEvent）

| notification | EchoesEvent | 備考 |
|--------------|-------------|------|
| `thread/started`（+ start/resume response） | `SessionInit {session_id: thread.id}` | chip / registry の源 |
| `item/agentMessage/delta` | `MessageChunk {text: delta}` | 主 stream |
| `item/completed`（agentMessage） | —（emit しない） | delta で全文流した後の重複を避ける（claude 翻訳と同じ規律）。delta が一切無かった場合のみ全文 fallback |
| reasoning 系 item / delta | `ThoughtChunk` | 実 wire 未観測（低 effort turn のため）— 実装時に schema `ThreadItem` の reasoning variant を確認して対応 |
| `commandExecution` / `fileChange` 等の item | `ToolCall` / `ToolCallUpdate` | started→Call / completed→Update。表示粒度は codex_translate（exec 版）の既存写像を踏襲 |
| `turn/completed` | `TurnCompleted` | `turn.status: interrupted` も完了扱い（Error にしない）。error 非 null は `Error` 併発 |
| `turn/failed` 相当 / JSON-RPC error | `Error {message}` | |
| `thread/tokenUsage/updated` / `account/rateLimits/updated` | —（v1 は捨てる） | **将来素材**: usage/rate limit chip（planType, usedPercent が既に流れている） |
| `mcpServer/startupStatus/updated` ほか | —（無視） | |

### 2-4. 差し替え点（VP 側）

- `ChatHost::Codex(CodexAgentHost)` の中身を TurnHost→RpcHost に**直切替**（pre-MVP:
  並行経路や feature flag は作らない）。`subscribe / in_flight / stop / submit` の既存 enum
  dispatch 面は不変 = pump / topic / chatview は無改修
- `ensure_chat_engine` の codex arm: `resolved.conversation` を RpcHost config に渡す（doc 40 済み
  の読み経路そのまま）
- Act I（床）は現状維持: `codex resume '<id>'` type-ahead（stand_spawner）。thread id は registry
  共有なので II⇄I 継続は自動整合（backfill bridge は PR-2 までの過渡）
- `codex_session` store: RpcHost は書かない（registry 直結）。store の退役は doc 40 PR-2 に合流

## 3. --oss / LM Studio 裏打ち（PR-B 実測、2026-07-18 **PASS**）

実測完了（de-risk script 派生、codex-cli 0.144.5 × LM Studio × qwen2.5-coder-14b）:

- **`lmstudio` は codex の built-in provider**（`model-provider-info` ソース確認: port 1234 /
  `WireApi::Responses` 固定 — 旧 `chat` wire は撤去済み）。config.toml への追記は**不要**で、
  `thread/start` に `model: "qwen/qwen2.5-coder-14b"` + `modelProvider: "lmstudio"` を渡すだけで
  local LLM に thread が張れる。同型の built-in に `ollama`（port 11434）
- **LM Studio 側の前提 2 つ**: ①server 起動（`lms server start`）+ model load 済み
  ②**context length 32k+ で load**（`lms load <model> --context-length 32768`）。既定 4096 では
  codex の initial prompt が溢れ「tokens to keep > context length」で Reconnecting 1/5〜5/5 ループ
  になる（実測で踏んだ→32k で解消）。LM Studio の `/v1/responses`（Responses API）実働も確認済み
- 実測結果: turn 完走 33.2s（14B / M-series）、`account/rateLimits/updated` は planType null =
  無課金経路。会話品質・agent loop（tool 呼び出し）の実力は未評価（今回は疎通のみ）

**配線の scope 確定**: protocol の通り道は開通済み。`CodexRpcHostConfig` に model/provider を
足すのは数行だが、**供給源**（per-lane engine_model か per-session か / GUI picker）は
dev path step 7（local LLM 正式化）の設計判断 — ここでは予約しない（pre-MVP: 実感してから作る）。

## 4. 段階

| 段 | 内容 |
|----|------|
| **PR-A** | `echoes/codex_rpc_host.rs`（RpcHost 本体 + JSONL RPC client）+ `codex_rpc_translate.rs` + ChatHost::Codex 差し替え + テスト（RPC client は stdio mock で固定） |
| **PR-B** | --oss / LM Studio 実測 → model/provider 注入の配線（実測結果次第で scope 確定） |
| step 4 | TurnHost 系撤去（cursor オミット確定後、doc 39 §7）+ doc 40 PR-2 合流 |

## 5. 既知の考慮点

- **experimental**: `codex app-server` は help 上 experimental。protocol drift は
  `generate-json-schema`（binary 同梱）で追える + 翻訳は未知 notification を無視する寛容設計で吸収
- **rate limit**: 実測中に `account/rateLimits/updated` で plus plan / usedPercent=12% を観測。
  常駐化で idle コストは無い（通知はイベント駆動）が、resume 時の input token
  （実測 41k、cached 23k）は thread 履歴長に比例する — 長寿命 thread の compaction
  （`thread/compact/start` あり）は将来の考慮点
- **`turn/steer`**: 実行中 turn への注入（claude に無い能力）。v1 scope 外だが、queued-message
  の上位互換として picker/UX の将来素材（doc 39 §6 でも記録済み）
