# Stand スモーク行列 — エンジン横断の動作チェック

> **日付**: 2026-07-16（cursor 実機 dogfood 起点、bikeboy 一次メモから正式移設）  
> **正本**: 本ファイル（stand 追加 PR の DoD がここを参照する）  
> **進め方ノート**: `.note/multi-engine-advancement.md`（基板=面軸 / 差分=engine / 記録=セル。lane 作業用の非コミット文書）  
> **関連 doc**: [doc 37](../design/37-echoes-two-axes.md)（engine × Act 直交格子）/ [doc 38](../design/38-lane-multi-session.md)（1 Lane = N session — 排他の単位は session に改定済み）  
> **status**: living checklist（stand 追加のたびに列を足す）

## なぜ必要か

Lane engine（stand）は増える一方:

| stand | エンジン | 備考 |
|-------|----------|------|
| `echoes` (cc) | Claude Code / claude CLI | 既定。Act I + Act II |
| `cursor` | cursor-agent | 2026-07 dogfood 中 |
| `codex` | OpenAI Codex CLI | |
| `agy` | Antigravity CLI | Act I のみ（chat 非対応） |
| `xai` | （追加予定 / 追加済） | 行列に行を足す |
| `shell` | 素 shell | engine なし（床のみ） |

unit test の `EngineKind::ALL` roundtrip は **名前対応の防壁**にはなるが、
**実 console での tool 権限・MCP・PTY・session resume** は実機でしか壊れない。
stand を足すたびに同じ穴を踏まないよう、**能力表 + スモーク手順 + 観測ログ**をここに集約する。

## 層分け

| 層 | 何を守るか | 誰が回すか |
|----|-----------|-----------|
| **A. 名前 / 能力表** | `from_stand` / `stand_name` / chat_capable 等 | `cargo test`（`engine.rs` 既存） |
| **B. spawn / resume** | create-chat / --resume / fail-open / SP 再起動で stand 保持 | 半自動 + 実機 |
| **C. Console 実機** | Act I 対話・Act II chat・入力・表示 | **手動 dogfood（本 doc の主戦場）** |
| **D. Agent tool surface** | Shell / MCP / Read-Write / wire | **stand 内 agent が自己報告**（下記テンプレ） |

C/D は CI に載せにくい。代わりに **stand 追加 PR の必須チェック**と**実機ログの追記**で回す。

## 能力表（期待値・正）

| 能力 | echoes | cursor | codex | agy | xai | shell |
|------|:------:|:------:|:-----:|:---:|:---:|:-----:|
| Act I (TUI console) | ✅ | ✅ | ✅ | ✅ | ❓ | ✅（床） |
| Act II (GUI chat) | ✅ | ✅ | ✅ | ❌ | ❓ | ❌ |
| session 指名 resume | ✅ cc_session | ✅ chatId | ✅ thread | — | ❓ | — |
| model 切替 (console_set_model) | ✅ | ❌（TUI 内） | ❓ | ❌ | ❓ | — |
| MCP `CallMcpTool` (vp) | ✅ 想定 | ❓ 観測中 | ❓ | ❓ | ❓ | — |
| Shell tool | ✅ 想定 | ❓ 観測中 | ❓ | ❓ | ❓ | n/a |
| wire_send/recv | ✅ | ❓ | ❓ | ❓ | ❓ | — |
| SP restart 後 stand 保持 | ✅ | ✅ 要確認 | ✅ 要確認 | ✅ | ❓ | ✅ |

`❓` = 未計測。stand 追加時は必ず列を埋め、❌/制限は「仕様」か「バグ」かを注記する。

## C. Console 実機チェックリスト（stand ごと）

新 stand / 大きな console 変更のたびに、対象 stand で全部通す。

### 起動

- [ ] `add_performer(stand="<name>")` または GUI「+」で lane が立つ
- [ ] sidebar に正しい stand 名 / icon が出る
- [ ] Act I console に prompt / TUI が出る（login 待ちならその旨が分かる）
- [ ] 未知 stand / CLI 不在時に **床だけ残って死なない**（fail-open）

### 対話

- [ ] 短いユーザ発話 → 応答が console に見える
- [ ] 長文・日本語が崩れない
- [ ] 再送 / 中断（あれば）が期待どおり

### Session

- [ ] lane 切替 → 戻ってきても同一 session（指名 resume）
- [ ] New Session / fresh で本当に新規になる
- [ ] SP Restart 後も **stand が echoes に化けない**

### Act II（chat_capable な stand のみ）

- [ ] console_set_mode(chat) できる
- [ ] submit → ストリーム表示
- [ ] chat → tui 往復で session がつながる

## D. Agent tool surface 自己診断テンプレ

stand 内の agent に次を投げ、結果を下表に転記する（コピペ可）。

```text
あなたは VP stand 実機スモーク中です。次を試し、OK/NG/メッセージを表で返してください。
1. Shell: `echo stand-smoke-ok && pwd`
2. Read: リポジトリ直下の何か 1 ファイル先頭数行
3. Write → Delete: /tmp/vp-stand-smoke-<stand>.tmp を書いて消す
4. GetMcpTools(server=vp) の serverStatus
5. CallMcpTool vp/show（短い markdown ping）
6. CallMcpTool vp/wire_inbox
7. CallMcpTool vp/list_lanes
8. creo-memories: GetMcpTools の auth 状態（認証は強制しない）
拒否やエラーはそのまま原文で残すこと。「User rejected」と自動ブロックの区別が付かない場合はそう書く。
```

### 転記用表

| # | 操作 | echoes | cursor | codex | agy | xai |
|---|------|--------|--------|-------|-----|-----|
| 1 | Shell | | | | | |
| 2 | Read | | | | | |
| 3 | Write/Delete | | | | | |
| 4 | GetMcpTools(vp) | | | | | |
| 5 | vp/show | | | | | |
| 6 | vp/wire_inbox | | | | | |
| 7 | vp/list_lanes | | | | | |
| 8 | creo-memories auth | | | | | |

## 観測ログ

### 2026-07-16 — cursor console（bikeboy / VP Cursor 実機）

実行環境: VP Cursor stand コンソール（agent 経由）。対話応答自体は成立。

| # | 操作 | 結果 | メモ |
|---|------|------|------|
| 1 | Shell（`echo` / `true` / `vp`） | **NG** | 常に `Rejected`（承認 UI か自動拒否か不明） |
| 2 | Read / Grep / Glob | OK | |
| 3 | Write | OK | |
| 4 | Delete | **NG** | `File deletion rejected`。Write 可・Delete 不可の非対称 |
| 5 | GetMcpTools(vp) | OK | `serverStatus: ready` |
| 6 | CallMcpTool vp/* | **NG** | 全て `User rejected MCP: vp-<tool>`（show / list_lanes / wire_inbox / flow_progress / add_performer / capture_canvas） |
| 7 | GetMcpTools と Call の乖離 | **バグ候補** | ready なのに call 全拒否 → 状態表示が嘘になる |
| 8 | FetchMcpResource(vp) | **NG** | `-32601 resources/read`（未実装なら仕様として明記したい） |
| 9 | Task（subagent） | OK | `probe-ok` |
| 10 | creo-memories mcp_auth | **NG** | `Interactive MCP authentication is only available in the Cursor desktop IDE` — VP Cursor agent 環境では認証 UI 不可 |
| 11 | 会話文脈 | 弱 | 「もう一回」の前履歴が transcript に無く、再実行対象が不明（session 切替？） |
| 12 | skill 文書 drift | 注意 | vantage-point skill 0.18.0 は `tmux_*` 等を列挙するが実 MCP に無い |

残置: `bikeboy/.cursor-console-hw-test.tmp`（Delete 拒否のため）

**仮説（未検証）**

1. VP Cursor 経由だと tool/MCP 承認がデスクトップ Cursor と別経路で、全部 User rejected に正規化されている
2. Shell/Delete がポリシーで先に落ち、MCP だけ承認待ちに見えるが実際は即 reject
3. 「ready」はプロセス接続のみで、lane の permission bridge は未配線

## stand 追加時の Definition of Done（短い）

1. `EngineKind` / stand_store バリデーション / UI chip に名前を足す（層 A）
2. 本 doc の能力表に列を足し、❓ を実測で潰す（層 C/D）
3. 観測ログに 1 節追記（日付・環境・表）
4. 失敗は「仕様で制限」か「バグ」かを一行で書く（曖昧禁止）

## 自動化の次の一歩（まだやらない / やりたい）

- [ ] 層 D を stand 内 agent が JSON で吐き、conductor が集約する smoke harness
- [ ] CI では層 A のみ必須。層 C/D は release / dogfood checklist
- [ ] `User rejected` と auto-block をエラー種別で分離（観測 6・仮説 1）
