# 86. VP / GFP の共有チャット基盤

> 旧番号 74。nightly に `74-brand-identity.md` が先に入ったため 86 へ繰り下げ（2026-10-22）。

> **Status**: Draft — protocol・engine 翻訳・Codex 要求台帳・起動契約・Codex 履歴・subagent task 翻訳は共有済み。VP は nightly に追随し `vendor/agent-chat` submodule で GFP と同じ revision を指す（2026-10-22、移行単位 5 の依存固定）。process lifecycle / reducer / UI の移行（単位 3〜5 の続き）と publish は未完了
> **Related**: design 32、40、41、65、67、69、70、72、`mem_1CfcoK3QjHurVQ173tZDuA`
> **対象**: `crates/vantage-point/src/conversation/`, `crates/vp-app/webview/chat-model.ts`, `crates/vp-app/webview/chatview.tsx`、GFP `crates/gfp-app/src/echoes/`, `web-bundle/src/echoes.ts`, `web-bundle/src/components/ChatLog.tsx`

## 目的と到達点

Claude / Codex の host、イベント、状態管理、SolidJS のチャット表示を一か所で保守し、
VP と GFP が同じ共有実装を依存として使用する。片方にコードをコピーした段階や、Rust
だけ共有した段階では完了にしない。GFP に VP アプリや lane registry の依存を持ち込まない。

両アプリ × 両 engine の開始・streaming・中断・再開、ツール表示、許可・拒否・会話内許可、
質問応答を保持する。元会話 ID、履歴、会話ごとの権限と MCP 設定を失わない。
VP のその他の engine（ACP / vpcode）と Console / Chat 切替も壊さない。

## 調査基準

| 対象 | 確認した状態 | 移行上の注意 |
|---|---|---|
| VP lane | `37e72d84`, `wip/shared-agent-chat` | GitNexus workspace index は同じ commit |
| GFP lead | `166978c`, `feat/mcp-approval-session`、未コミット変更あり | 読み取りのみ。既存差分を移行の差分と混同しない |
| GFP origin/nightly | `0605aed` | `166978c` の MCP 会話内承認の追加が入っていない |
| GFP 別 checkout | 引き継ぎでは `box-case-open`, `cb99160` | 統合前に現在の HEAD と app/UI 差分を再確認する |

両アプリの Codex は persistent app-server。GFP `mod.rs` に残る turn-scoped の説明は
現行 `codex_host.rs` の実装と一致しない。旧説明を移行の前提にしない。

### 既存契約の相違

| 面 | VP | GFP | 共通化で保つもの |
|---|---|---|---|
| `session_init.mcp_servers` | `string[]` | `{name,status}[]` | 名前に加えて接続状態。旧形式の読み取りは adapter |
| Claude 承認 | `Question` / `PermissionRequest` | suggestions、取消、mode、path policy | 質問と承認の区別、session scope の提案、取消・実効値 |
| Codex 要求 | `CodexInteractions`、elicitation、独立権限要求、非同期質問 | `CodexRequests`、MCP tool approval、`can_remember` | engine 固有の要求と応答能力を欠落させない |
| Codex 履歴 | native snapshot、送信 ID 照合、hydration | replay/store と actor | native の復元と snapshot/live の順序保証 |
| UI model | flat item stream、独立 reducer | turn/block、純関数 fold とアプリ用データが同居 | 共通 reducer を正本としアプリ表示を射影 |
| UI 依存 | Solid `^1.9.15` / creo-ui `^0.30.0` / TS `^7.0.2` / vitest | Solid `^1.9.0` / creo-ui `^0.30.0` / TS `^5.5.0` / Bun tests | consumer ごとの型検査・bundle 検証 |

## 共有パッケージの配置案

**ユーザー裁定で確定**: 独立 repo `agent-chat` に Rust workspace と TS workspace を同居させる。
仮称は `agent-chat-protocol` / `agent-chat-host` / `@chronista-club/agent-chat`。
Rust protocol → ts-rs の生成型・serde fixture → TS model / Solid UI の向きで型を一元化する。
両アプリに生成型の独立コピーを持たせず、TS package の export を import する。

開発中は同一 checkout を指す Cargo path / Bun のローカル依存で双方の変更を検証する。
配布時は crates.io / npm の同じ release を両 consumer で固定し、lockfile を同時更新する案。
ローカル package の `0.1.0` は開発中の仮番号で、publish は無効。
remote 作成、公開名、license、正式初版番号、registry 公開は未実施・未確定。
ローカルの絶対 path に依存した状態を配布可能とは扱わない。

更新は共有側の release manifest（Rust と TS の版・protocol revision）を基準に、
両 consumer の依存更新 PR と検証結果を対応づける。共有修正の片側取り込みで完了にせず、
両側が同じ revision を使うことを gate にする。既存アプリの自動更新・再起動は別の操作。

## 責務と注入契約

共有 host は stdio と engine protocol を所有し、アプリ transport を知らない。
公開 contract の名称・細部は最初のテストで固定する。以下は責務の案。

| 契約 | アプリが与えるもの | 共有層が守ること |
|---|---|---|
| Launch context | 実行ファイル、cwd、環境変数、MCP 設定、追加 rules、明示的な engine settings | protocol 引数を構成して起動。権限や reviewer を勝手に補完しない |
| Conversation binding | 不透明な会話キー、engine identity、resume ID | host 世代・thread/turn/request を照合。別会話・遅延応答を受理しない |
| Persistence | session ID/settings 記録、transcript 読取、async question の保存 hook | 書込失敗を表面化。resume 失敗を新規会話へ変換しない |
| Permission policy | GFP PathPolicy / sandbox、VP の現在の設定・許可リスト | 判断不能なら人に返す。会話内許可を永続設定へ拡張しない |
| Event sink / subscription | Unison topic または WebView IPC | replay と live を順序付け、購読解除と再接続を扱う |
| UI transport | submit / interrupt / respond / configure / requestHistory | Promise/ACK の成功と UI 操作を区別し、会話・要求 ID を往復させる |
| UI slots / capabilities | header、案件/lane メタデータ、音声、リンク、画像入力、利用可能能力 | 本文・ツール・承認・質問カードと reducer を共通にする |

VP adapter に session registry、`SessionKey`、agent env、skill docs、Unison 配信を残す。
GFP adapter に ProjectStore / StoreRecorder、案件 workspace、CLI resolver、
`gfp-estimate` MCP の探索と秘密情報の取得、WebView bridge、PathPolicy を残す。
既存 `SessionRecorder::record` は失敗を返せないため、そのまま唯一の永続化契約にはしない。

イベントは共通 envelope の会話キー・host 世代と、engine 固有の payload を分ける。
既存 wire の `kind` を即座に全面変更せず、段階移行用 adapter が旧形式を扱う。
未知の機能は capability として表示し、既存機能を最低共通仕様へ削らない。
MCP status、notice、request の runtime snapshot は過去の承認を再実行させない。

## 最初の移行単位

1. **契約と基準**: 両現行実装の fixture を採取し、MCP status / 会話内許可 / replay 終端 /
   native 履歴 / late event を固定する。現行の期待値と新たな契約の RED を区別する。
2. **event/protocol**: 共有 Rust protocol crate と TS 生成型を作り、双方の public export を
   adapter 経由で切り替える。最初の PR の出口は両 consumer で serde / 型 / reducer 契約が通ること。
3. **host**: VP の lifecycle/history を基礎に app 依存を注入へ移し、GFP の承認・MCP policy
   契約を統合。双方が同じ Claude/Codex host を使用する。旧コピーを残して成功としない。
4. **model/UI**: 純粋 reducer、interaction model、本文/ツール/質問/承認表示を共有 package へ。
   VP header/voice/Console と GFP 案件操作を slots / transport に分ける。
5. **依存更新・重複削除**: 同一 release の導入、consumer 検証、開発・更新 guide。

### GFP 作業の準備

専用 worktree を最新 `origin/nightly` 起点で作り、`166978c` の承認差分を明示的に突合する。
lead の未コミット変更を無差別に持ち込まない。Box 関連 app / App.tsx / Sidebar の変更は
別 checkout と衝突範囲を確認し、bridge とチャット描画部分に移行を限定する。
GFP 側には次の契約実装が必要。

- project/conversation → 不透明会話キーと案件 cwd の対応。
- StoreRecorder の成功・失敗を返す記録と resume ID の読み取り。
- local MCP / rules / sandbox の launch context 注入。
- native イベントを既存 project 宛の WebView message へ運ぶ adapter。
- ChatLog と composer の共有 UI 接続、MCP/案件 header の slot。
- MCP approval の `can_remember` と `allow_session` を保持する往復テスト。

## 検証と完了条件

- Rust serde fixture と生成 TS の網羅・drift 検査。両 consumer の型検査。
- 偽 CLI の JSONL roundtrip で開始、stream、interrupt 後の再送、resume、異常終了。
- resume 失敗時の元 ID 保持。snapshot と live / optimistic 送信の重複防止。
- 要求 ID の host 世代分離、複数会話、遅延応答、取消、二重回答、書込失敗。
- 許可・拒否・会話内許可・質問回答、未知要求の非許可。無断の権限拡大なし。
- 同じ共有 reducer / 表示 / 承認の fixture を VP と GFP adapter 経由で実行。
- 両アプリで Rust 対象 tests、TS tests、typecheck、bundle。既存失敗を区別する。
- 実機 2 アプリ × 2 engine の検収結果は別記。稼働アプリを無断で再起動しない。

## やってはいけない

- GFP の古い turn-scoped 説明を実装の事実として使う。
- VP の型に統一するだけで GFP の MCP 接続状態・session scope の許可を落とす。
- 設定省略を `auto_review` や full access の選択と読み替える。
- 再接続時の snapshot を通常の live 操作として再発火する。
- GFP の未コミット変更、稼働バイナリ、VP lead の branch を変更する。
- protocol の移行だけで全体完了と報告する。

## protocol 移行の現在地

- `/Users/makoto/repos/agent-chat` に独立 Git repo と `agent-chat-protocol` crate を作成。
- VP `ConversationEvent` と GFP `EchoesEvent` は同じ Rust enum の re-export / alias。
  GFP の bridge 語彙も共通 enum に保持し、host 切替前に消さない。
- MCP status / Claude permission suggestions / GFP Codex session permission を往復する
  3 テストで現行 VP の RED を確認し、共有 crate と VP 接続後に GREEN を確認。
- MCP の旧 string 形式の読取と旧 request の `can_remember=false` を回帰テストで保持。
- 30 kind の Rust serde fixture と ts-rs 生成型は共有 repo が所有。VP の旧 generated path は
  package を指す shim。GFP も同じ event 型と fixture を使う。
- モデル名検証は条件を変えず共有関数へ移設。VP `EngineSettings` の TS 生成は
  Codex 型だけ package の型を参照し、consumer が共有型を再生成しない。
- GFP の `166978c` は dedicated worktree に no-commit 適用。lead の未コミット39項目は保持。
- Bun `file:` 依存の package 内容はキャッシュされるため、共有 package を変更した後は
  consumer の `bun install --force` で更新して検証する。Cargo path は共有 source を直接使用。
- GFP の型検査には `./node_modules/.bin/tsc --noEmit` を使う。
  `bun exec tsc --noEmit` はこの環境で期待した検証にならず lead に JS を生成した。
  開始時に無かった40生成ファイルのみ `/private/tmp/agent-chat-tsc-emitted` へ退避し、
  lead の状態を開始時の39項目に戻した。初回の型エラーは正しい worktree の基準として使わない。

## Status log

- 2026-10-22: VP 側を nightly（8febd2ad）に追随させ、依存を `vendor/agent-chat` submodule で固定（task `mem_1CgHKDoqQYVjXcmz92CZdG`）。
  lane の 6 commit を 1 つに squash して rebase、衝突 9 file。shim 系（event / claude_translate /
  conversation_event_fixtures / generated TS 3 本）は lane 側を採り、nightly #1201 の subagent task_* 翻訳
  （`SubagentTask` event、`SubagentRole::{ToolUse, ToolResult}`、golden testdata）は **agent-chat PR #1** へ移植
  （共有 71 成功。LICENSE MIT / Apache dual と license metadata も同梱、mako 裁定で public 化予定）。
  codex_host は nightly の 4 修正（継承 env の剥がし / 起動失敗 message に program / active writer の recovery 文 /
  main → lead）を保持したまま共有 `codex_process::command` を使う。chatview は nightly の subagent float と
  未確認入力の復元を保持し、画像添付は共有 `createImageAttachments` の `restore` へ。
  nightly 以降に増えた esbuild 込み test 15 file に `preserveSymlinks: true` を揃えた（symlink 越しの共有部品から
  solid-js を解決できず落ちる）。`PermissionRequest` を組み立てる test 4 箇所に共有 protocol の optional field を補った。
  submodule: `.gitmodules` url `../agent-chat.git`、root `exclude = ["vendor"]`、Cargo path と Bun `file:` を vendor 相対へ。
  CI は全 checkout を `submodules: recursive` + `token: AGENT_CHAT_TOKEN || github.token` + `persist-credentials: false`（GFP と同形）。
  README / setup.md / AGENTS.md に `--recurse-submodules` と `submodule update --init`。設計書は 74 → 86 に繰り下げ。
  検証（VP）: `cargo check --tests`、lib conversation 192 成功・6 ignored、契約 3 + fixture 1、`cargo clippy --workspace
  --all-targets -D warnings`、webview typecheck / build、vitest 938 成功（順次実行。並列 worker は load 13 の機械で
  esbuild 込み test が 20s を超える = 環境起因、基準測定でも同じ）。GFP（nightly 4f2a628 起点の worktree、同じ submodule
  commit）: web-bundle tsc、共有関連 bun test 51、bundle、`cargo test -p gfp-app --lib echoes` 180 成功・7 ignored。
  残: process lifecycle 状態機械 + 永続化 callback、純粋 reducer / ChatLog カードの共有、publish（crates.io / npm か
  submodule 継続か）、実機検収（mako）。`vp lane new` は provisioning で `submodule update --init --recursive` を
  best-effort で流す（lead 裁定 A、同 PR。失敗は warn で続行）。

- 2026-10-02: Codex 起動契約を `agent-chat-host::codex_process::command` へ移設し、VP/GFP の
  両 process host が使用。stdio endpoint / piped stdio / kill-on-drop を共有側が所有し、
  executable の探索・cwd・identity env・MCP/rules の overrides はアプリから注入する。
  権限・reviewer の暗黙指定は追加しない。GFP のテスト用 executable でも本番と同じ引数を渡す
  （以前は overrides を全て落としていた。RED→GREEN）。実プロセスで argv/cwd/env/JSONL往復を検証。
  native 履歴も `agent-chat-host::codex_history` へ移設し VP を re-export に変更。
  空の thread/turn/item ID と重複 turn/item ID を拒否する（旧実装の空ID受理を RED で確認）。
  GFP の native 履歴読取・UI接続はまだ未実施。process lifecycle 状態機械と永続化 callback の
  統合は次段であり、共通 command builder だけをホスト本体の統合完了とは扱わない。
  検証: 共有65成功（host unit53 + integration4 + protocol8）、VP会話178成功・6 ignored、
  GFP全体268成功・8 ignored。共有fmt、3 repo差分チェック成功。実機確認は未実施。

- 2026-10-02: Codex の item 翻訳を `agent-chat-host::codex_rpc_translate` へ移設（VP 版を基礎）。GFP の決まりを統合:
  MCP の tool 名を `mcp__<server>__<tool>`（GFP の変更系判定、VP UI は `mcp__*` を汎用表示）、入力は引数のみ、
  `result.isError` / `declined` を失敗、content の text を本文に、mcpToolCall 完了で `McpStatus`（connected/failed）。
  非同期質問の `item_key` / `async_questions` も共有側へ。VP は re-export、会話 185 成功（翻訳 6 本を移設）。
  GFP は旧 exec 書き換えと exec 翻訳器・TurnHost を削除し `codex_message` を描画。共有 host 43 成功。実機確認は未実施。
- 2026-10-02: GFP は Codex を主 engine にする裁定（`mem_1Cfcsi6EVd1J8bb2ockFaF`）を受け、host は Codex から。
  第一単位として Codex の要求台帳を `agent-chat-host::codex_interactions` へ移設。VP の台帳
  （elicitation / 独立権限 / cancel_on_deny / 二重回答防止）を基礎に、GFP の MCP ツール承認
  （`McpApproval`）・会話内許可・信頼サーバーの自動許可・権限の一括 turn 許可・Cancel と interrupt を統合。
  表示語彙は各アプリの射影に残し、VP は旧 card を維持（ツール承認は項目なしの MCP 手続き、会話内許可は未表示）。
  protocol の `CodexQuestion` に `is_other` を追加（VP が読み捨てていた自由記述の印）。
  VP の台帳 10・フォーム 1 テストは共有側へ移し、GFP 由来 5 本を追加して共有 36 成功。
  GFP は台帳 API と既存テスト 7 本を無変更で通過、echoes 172 成功・6 ignored。
  VP 会話 191 成功・6 ignored（移設 11 本減・射影 3 本増）、契約 4 成功。両 WebView の型検査・
  VP TS 143・GFP TS 114・GFP bundle 成功。VP の未使用 `jsonschema` 依存を削除。実機確認は未実施。
- 2026-10-02: Claude translator を `agent-chat-host` へ移設し、VP は re-export、GFP は
  返値を event 列にする adapter へ変更。GFP の承認 metadata / suggestions / 取消と未対応要求の
  検出も共有側が所有。control routing は transcript 翻訳と分離し、VP の質問 router を保持。
  GFP で synthetic 応答欠落・中断の Error 化を2件の RED で再現し、共有切替で GREEN。
  VP の20テストは共有側へ移動。共有31、VP会話199成功・6 ignored、GFP172成功・6 ignored。
  GitNexus `ingest` は HIGH（直接17・合計23、receiver type未解決1件）、変更前に報告。
  GFP は索引未登録のため呼出検索と回帰テストで補完。

- 2026-10-02: 引き継ぎを受領。両コードと VP 索引の現状を確認し、契約・移行単位を Draft 化。
  配置/名称/配布方式は提案段階。GFP 既存 TS テスト 29 件成功。
- 2026-10-02: ユーザーが独立 repo `agent-chat` を選択。protocol を共有し両アプリに接続。
  共有 Rust 8 テスト、VP 会話 Rust 219 成功・6 ignored、GFP Rust 170 成功・6 ignored。
  両 WebView のローカル型検査と bundle 成功。VP 対象 TS 143、GFP 既存対象 TS 33、
  GFP 共有 fixture 契約1件が成功。配布依存への固定、host/reducer/UI 移行、実機確認は未完了。
