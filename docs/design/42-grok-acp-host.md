> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 42 — AcpAgentHost: grok の ACP 常駐統合（dev path step 5）

> 2026-07-18、mako + Fable 5 conductor session。doc 39 §7「常駐型のみの一枚岩
> （claude / codex / **grok=ACP**）」の grok 実装編。doc 41（codex RpcHost）と同じ進行:
> 実測 de-risk（§1、全 PASS）→ 設計固定 → 実装。

## 0. TL;DR

grok CLI（`grok agent stdio`）は **ACP（Agent Client Protocol、protocolVersion 1）**を実装
していると実測で確認。Act II を **AcpAgentHost**（常駐 ACP 子プロセス、1 session = 1 プロセス =
Echoes/CodexRpc と同型）で統合し、`EngineKind::Grok`（stand `"grok"`）を追加する。
会話 id（ACP sessionId）は **registry-native 第一号**（旧 store も backfill bridge も最初から
無い、doc 40 の純化形）。ACP は標準 protocol なので、host は program 差し替えで
opencode 等にも流用できる（§5、bonus 未実装）。

## 1. 実測 de-risk（2026-07-18、grok 0.2.64、cached_token auth、全 PASS）

| Phase | 内容 | 結果 |
|-------|------|------|
| A | initialize → `session/new` → `session/prompt` → update stream → response | **PASS** |
| B | 同一プロセス 2nd prompt — 文脈保持 | **PASS** |
| C | プロセス kill → 新プロセス **`session/load`** → 文脈継続（SP 再起動シナリオ） | **PASS** |
| D | tool 実行 prompt → `session/request_permission` 1 件 → allow 応答 → tool 実行成立 | **PASS** |

確定した protocol 事実:

- **入口**: `grok agent stdio`（常駐、JSONL over stdio）。**`jsonrpc: "2.0"` field を含む**
  標準 JSON-RPC（codex app-server は省略形だった — この差は host 実装が吸収）
- **handshake**: `initialize {protocolVersion: 1, clientCapabilities.fs: {read/write: false}}` →
  `agentCapabilities.loadSession: true` / authMethods（cached_token = `~/.grok/auth.json`）/
  `_meta.modelState.currentModelId: "grok-build"`
- **sessionId** = UUID v7 形（例 `019f72c0-de47-…`）— codex thread id と同形、英数+ハイフン
  validator で通る
- **turn 駆動**: `session/prompt {sessionId, prompt: [{type:"text", text}]}` — streaming は
  `session/update` notification（下記 variant）、**turn 終了 = prompt request の response**
  （`{stopReason: "end_turn"}`）。codex の turn/completed notification と違い **response 駆動**
- **resume**: `session/load {sessionId, cwd, mcpServers}` — 過去会話を `session/update` 列で
  **replay してから** response が返る（VP は replay_log 経由の自前 replay を使うため、load 時の
  replay 列は捨てる — §3）
- **中断**: `session/cancel` notification（要 request でなく通知）→ prompt response が
  `stopReason: "cancelled"` で返る
- **観測した update variant**: `agent_message_chunk` / `agent_thought_chunk` /
  `available_commands_update` + x.ai 拡張（`_x.ai/session_notification` の `turn_completed` /
  `hook_execution`、`_x.ai/queue/changed` 等）— 拡張 method は全て無視して良い（標準 ACP だけで
  会話が成立することを実測確認）
- **permission**: tool 実行時に server→client request `session/request_permission`
  {options: [{optionId, kind}]} が来る。**allow 系 option を選んで応答すれば tool が走る**（実測）。
  親 command の `--always-approve` flag も存在（`grok agent --always-approve stdio`）
- **Act I（床）**: TUI は `grok -r '<SESSION_ID>'` で同一 session を resume 可能（`-r` は
  「ID 指名 or 最新」— 指名形を使う。codex / claude と同じ床共有の形）

## 2. 設計 — AcpAgentHost

**CodexRpcHost（doc 41 §2）と同型の per-session 常駐**。差分だけ記す:

| 面 | codex（RpcHost） | grok（AcpAgentHost） |
|----|------------------|---------------------|
| spawn | `codex app-server` | `grok agent --always-approve stdio`（bypass parity — 下記） |
| wire | JSONL、`jsonrpc` field 省略 | JSONL、`jsonrpc: "2.0"` 付き |
| handshake | initialize → initialized notif | `initialize`（notification 不要） |
| 会話確立 | thread/start \| resume（response の thread.id） | `session/new` \| `session/load`（response の sessionId / 指定 id）。load error → new へ self-heal |
| turn | turn/start → **turn/completed notification** | `session/prompt` → **request の response** が終了 signal（pending map の response 到着 = TurnCompleted emit） |
| 中断 | turn/interrupt request | `session/cancel` **notification**（応答なし。prompt response が cancelled で畳む） |
| 逆方向 request | approvalPolicy=never で来ない | `session/request_permission` が来る → **host が allow 系 optionId を自動応答**（claude/codex と同じ bypass parity — [[echoes-act2-parity]]。`--always-approve` と二段防御） |
| 会話 id 記録 | registry 直結（bridge 併存） | **registry 直結のみ**（grok に旧 store が存在しない = doc 40 純化形の第一号） |
| 途絶 | dead flag + submit Err → 自己修復（moody #1） | **同じ配線を最初から実装**（PR #802 の教訓を初期設計に含める） |

- 床（Act I）: `stand_spawner` に grok arm 追加 — `grok -r '<id>' || grok`（resume 空振りは
  TUI 内で fallback。claude/codex と同じ 3 arm 目の形、bare/fresh 分岐も同型）
- replay: grok session の disk 実体は grok 側（`~/.grok`）にあるが、VP の Act II replay は
  他 engine と同じ **replay_log tap**（pump 書き）に統一 — `session/load` の replay 列は
  翻訳しない（二重表示防止。VP 自前 replay と競合するため）

## 3. 翻訳表（session/update → EchoesEvent）

| update.sessionUpdate | EchoesEvent | 備考 |
|----------------------|-------------|------|
| `agent_message_chunk`（content.type=text） | `MessageChunk` | 主 stream（実測） |
| `agent_thought_chunk` | `ThoughtChunk` | 実測（grok は thought を平文で流す — claude の暗号化と違い replay 可能だが v1 は他 engine と同じ扱い） |
| `tool_call` | `ToolCall` | ACP 標準形 {toolCallId, title, kind, rawInput}。実測 D では省略された（hook_execution 経由）が標準対応しておく |
| `tool_call_update` | `ToolCallUpdate` | {toolCallId, status, content}。status=failed → is_error |
| `plan` | `Plan` | ACP 標準 {entries: [{content, status}]} |
| `available_commands_update` / その他未知 | 無視 | protocol drift 吸収（doc 41 §5 と同じ寛容規律） |
| `_x.ai/*` method 全般 | 無視 | 拡張。標準 ACP だけで会話成立を実測確認済み |
| session/load 中の replay 列 | **無視**（load 完了まで translator を繋がない） | VP 自前 replay（replay_log）との二重化防止 |

host 側 lifecycle（translator 外）: session/new\|load response → `SessionInit` + registry
`set_conversation` / prompt response → `TurnCompleted`（stopReason 問わず — cancelled も完了）/
JSON-RPC error・途絶 → `Error`。

## 4. 差し替え点（VP 側）

- `EngineKind::Grok` 追加（stand `"grok"`、`ALL` 5 本目、chat_capable=true、
  model_switchable=false — model は grok 側 `-m` / TUI）。roundtrip / stands / capability テスト更新
- `ChatHost::Grok(AcpAgentHost)` variant + dispatch（permission 系は claude 専用のまま —
  grok の permission は host 内で自動応答するため GUI 面に出さない）
- `ensure_chat_engine` に grok arm（`resolved.conversation` → session/load）+ replay tap を
  Grok にも
- `session_registry::is_valid_conversation` に Grok arm（UUID 形 = 英数+ハイフン）。
  **backfill は不要**（grok の旧 store が存在しない — bridge の対象外という「正しい欠如」）
- `stand_spawner` に床 arm（`grok -r` resume / bare 分岐は claude と同型）

## 5. opencode bonus（未実装、予約のみ）

ACP は標準 protocol（Zed 発、agentclientprotocol.com）。AcpAgentHost の spawn program を
差し替えれば opencode 等の ACP 対応 CLI が同じ host で乗る。実装は「opencode を使いたい実感」が
出てから（pre-MVP: 予約しない）— その時の作業は EngineKind 1 本 + AcpHostConfig の program 化のみ。

## 6. 既知の考慮点

- **leader mode**: grok には `--leader`（複数 client で backend 共有）があるが v1 は使わない
  （per-session 独立プロセス — codex と同じ判断、doc 41 §2-1）
- **`--always-approve` の効き**: 実測 D は flag 無しで request_permission が来た。flag 付きで
  来なくなるかは未実測 — host の自動応答が主防御、flag は belt（来ても来なくても正しく動く）
- **auth 失効**: authMethods の cached_token が切れた場合の挙動は未実測。stderr 診断
  （doc 41 moody #2 の機構を流用）で観測可能にしておく
- **update 語彙の拡張**: `_x.ai/session_notification` の `turn_completed` は将来 usage/
  timing 素材（`_meta.totalTokens` も流れている — codex の tokenUsage と同型の将来素材）
