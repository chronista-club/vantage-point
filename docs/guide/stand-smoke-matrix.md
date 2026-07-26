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
| `shell` | 素 shell | engine なし（shell のみ） |

unit test の `EngineKind::ALL` roundtrip は **名前対応の防壁**にはなるが、
**実 console での tool 権限・MCP・PTY・session resume** は実機でしか壊れない。
stand を足すたびに同じ穴を踏まないよう、**能力表 + スモーク手順 + 観測ログ**をここに集約する。

## 層分け

| 層 | 何を守るか | 誰が回すか |
|----|-----------|-----------|
| **A. 名前 / 能力表** | `from_stand` / `stand_name` / chat_capable 等 | `cargo test`（`engine.rs` 既存） |
| **B. spawn / resume** | create-chat / --resume / fail-open / repo 再起動で stand 保持 | 半自動 + 実機 |
| **C. Console 実機** | Act I 対話・Act II chat・入力・表示 | **手動 dogfood（本 doc の主戦場）** |
| **D. Agent tool surface** | Shell / MCP / Read-Write / wire | **stand 内 agent が自己報告**（下記テンプレ） |

C/D は CI に載せにくい。代わりに **stand 追加 PR の必須チェック**と**実機ログの追記**で回す。

## 能力表（期待値・正）

| 能力 | echoes | cursor | codex | agy | xai | shell |
|------|:------:|:------:|:-----:|:---:|:---:|:-----:|
| Act I (TUI console) | ✅ | ✅ | ✅ | ✅ | ❓ | ✅（shell） |
| Act II (GUI chat) | ✅ | ✅ | ✅ | ❌ | ❓ | ❌ |
| session 指名 resume | ✅ cc_session | ✅ chatId | ✅ thread | — | ❓ | — |
| model 切替 (console_set_model) | ✅ | ❌（TUI 内） | ❓ | ❌ | ❓ | — |
| MCP `CallMcpTool` (vp) | ✅ 想定 | ✅ II=`--force`要 | ❓ | ❓ | ❓ | — |
| Shell tool | ✅ 想定 | ✅ II=`--force`要 | ❓ | ❓ | ❓ | n/a |
| wire_send/recv | ✅ | ❓ | ❓ | ❓ | ❓ | — |
| repo restart 後 stand 保持 | ✅ | ✅ 要確認 | ✅ 要確認 | ✅ | ❓ | ✅ |

`❓` = 未計測。stand 追加時は必ず列を埋め、❌/制限は「仕様」か「バグ」かを注記する。

## C. Console 実機チェックリスト（stand ごと）

新 stand / 大きな console 変更のたびに、対象 stand で全部通す。

### 起動

- [ ] `add_performer(stand="<name>")` または GUI「+」で lane が立つ
- [ ] sidebar に正しい stand 名 / icon が出る
- [ ] Act I console に prompt / TUI が出る（login 待ちならその旨が分かる）
- [ ] 未知 stand / CLI 不在時に **shell だけ残って死なない**（fail-open）

### 対話

- [ ] 短いユーザ発話 → 応答が console に見える
- [ ] 長文・日本語が崩れない
- [ ] 再送 / 中断（あれば）が期待どおり

### Session

- [ ] lane 切替 → 戻ってきても同一 session（指名 resume）
- [ ] New Session / fresh で本当に新規になる
- [ ] repo Restart 後も **stand が echoes に化けない**

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

### 2026-07-16 §2 — P0 切り分け（cursor Act II headless の承認経路を実測で確定）

> creo todo `mem_1Cd5ByZDv3hZmDNdoJh4nL` / lane `p0-approval`。cursor-agent `2026.07.09-a3815c0`。
> 手法: scratch repo（`/tmp/p0-cursor-probe`、git init 済）で `cursor-agent -p … --output-format
> stream-json --trust` に承認系 flag を差し替えて差分を取得。**§1（bikeboy）の 4 観測を baseline
> （現行 Act II command = `-p … --trust`）で文字列レベルまで完全再現**した = bikeboy の観測は Act I
> ではなく **Act II（headless / stream-json）経路**だった（tool 拒否が `{"rejected":{"reason":…}}`
> という stream-json 構造で出るのが動かぬ証拠）。

**根本原因（1 つ）**: headless `-p` は `system/init` が `"permissionMode":"default"` 固定で、承認
prompt を対話で出せない。よって `default` mode で承認を要する tool は**全て auto-block**される:

- readonly（Read / Grep / Glob）: 承認不要 → **OK**
- trusted workspace の edit / write: auto-approve → **OK**
- 非 allowlist の **Shell** / **File deletion** / **MCP tool call**: 承認要 → prompt 不可 → **auto-block**

**flag 別 差分表**（scratch repo・同一 prompt で実測）

| 操作 | baseline（`--trust`） | `--trust --approve-mcps` | `--trust --force` |
|------|:---:|:---:|:---:|
| Shell `echo`（allowlist 済） | ✅ 実行 | ✅ | ✅ |
| Shell `date +%s`（非 allowlist） | ❌ `{"rejected":{"reason":"","isReadonly":false}}` | ❌ 同左 | ✅ stdout 取得 |
| File write（edit） | ✅ success | ✅ | ✅ |
| File **delete** | ❌ `{"rejected":{"reason":"File deletion rejected"}}` | ❌ 同左 | ✅ deleted |
| `GetMcpTools(vp)` | ✅ `serverStatus:ready` | ✅ | ✅ |
| `CallMcpTool vp/list_lanes` | ❌ `{"rejected":{"reason":"User rejected MCP: vp-list_lanes"}}` | ❌ **同左（無効）** | ✅ server 到達（→ `-32603 SP未接続` = scratch が VP 未登録なだけの正常応答） |

**項目ごとの判定**

| # | §1 観測 | 原因 | 判定 | 対応 |
|---|---------|------|:---:|------|
| 1 | Shell 常時 Rejected | headless default mode は非 allowlist を auto-block。`echo`/`vp` 等 allowlist は通る | **(a)** | `--force` |
| 2 | Write OK / Delete NG の非対称 | write は trusted で auto-approve、delete は高リスクで承認要 → headless で auto-block | **(a)** | `--force` |
| 3 | GetMcpTools=ready なのに Call 全 "User rejected" | per-call 承認 gate の auto-block。**`--approve-mcps` は server 承認レベルで per-call に効かない**、`--force` で開放。`ready` は server 接続の真実であって嘘ではない（= VP バグではない） | **(a)** | `--force` |
| 4 | FetchMcpResource(vp) `-32601` | vp MCP は `ServerCapabilities::builder().enable_tools()` のみ宣言（`mcp.rs:1009`）で **resources 非提供** → `resources/read` に `-32601 Method not found` を返すのが JSON-RPC 上正しい。cursor は無罪 | **(c)/spec（VP 側）** | 記録のみ（resources を出す予定が無い限り不対応が正） |

**DoD:「User rejected」と auto-block の区別**: headless では**全て auto-block**であり、真の user
rejection は存在しない（人間が居ないのに `--force` で flip する事実が証明）。判別法 = `reason` が空 or
`"File deletion rejected"` のような定型文なら auto-block。MCP の `"User rejected MCP: …"` は文言が
**誤解を招く**が実体は同じ auto-block（cursor-agent 側の表示バグ寄り、VP からは変えられない）。

**適用した fix**: `crates/vantage-point/src/echoes/cursor_host.rs` の Act II command に `--force` を
追加（`--trust` の次）。これで cursor Act II が claude（bypassPermissions）/ codex（full bypass）と
tool 権限で parity。⚠️ 権限拡大（deny 空なら実質全許可）。Act I（TUI slot）は対話承認が効くため**未付与**
（§1 の観測は Act II 経路と判明したので Act I の即 Reject は未確認 = 別途 live dogfood 事項）。

**副産物（記録のみ）**

- **creo-memories MCP auth の Desktop 依存**（§1 観測 10）: creo-memories は OAuth 必須の HTTP MCP
  （`~/.cursor/mcp.json` で `url: https://mcp.creo-memories.in/`）。cursor-agent CLI は
  `Interactive MCP authentication is only available in the Cursor desktop IDE` を返し、CLI 単体で
  認証できない = **判定 (c)**（cursor-agent CLI の仕様制限、VP から対処不能）。回避は Desktop Cursor で
  事前認証 or API-key 系 MCP。
- **判定 (b) は無し**: 今回の 4 観測はいずれも VP 基板の未配線ではなく cursor-agent の flag/仕様で説明が
  付いた。VP の承認 bridge（doc 35 HITL）一般化は本 P0 では不要。ただし Act II を `--force` で全許可に
  倒したので、将来「cursor の tool を GUI で個別承認したい」となったら C4（control protocol）で
  can_use_tool 相当を cursor stream に橋渡しする実装が別途要る（推奨アプローチ = §下記）。

**承認 bridge 一般化（判定 (b) 相当、今回は実装しない）の推奨アプローチ**: cursor の
`tool_call{subtype:started}` を interrupt して VP GUI に承認を問い、応答で continue/deny する双方向
制御が要る。cursor-agent CLI は headless で per-call 承認の外部注入口を持たない（`--approve-mcps` は
server 一括のみ）ため、claude の can_use_tool（doc 35）と同型の bridge は cursor では**現状不可能**
＝ C4 で cursor 側 control protocol の対応状況を先に調査してから。それまでは `--force`（全許可）が唯一
現実的な parity 手段。

## stand 追加時の Definition of Done（短い）

1. `EngineKind` / stand_store バリデーション / UI chip に名前を足す（層 A）
2. 本 doc の能力表に列を足し、❓ を実測で潰す（層 C/D）
3. 観測ログに 1 節追記（日付・環境・表）
4. 失敗は「仕様で制限」か「バグ」かを一行で書く（曖昧禁止）

## 自動化の次の一歩（まだやらない / やりたい）

- [ ] 層 D を stand 内 agent が JSON で吐き、conductor が集約する smoke harness
- [ ] CI では層 A のみ必須。層 C/D は release / dogfood checklist
- [ ] `User rejected` と auto-block をエラー種別で分離（観測 6・仮説 1）
