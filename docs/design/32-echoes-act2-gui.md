> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# 32. Echoes Act II — 構造化会話 GUI

> ⚠️ 旧番号 30。nightly の `30-window-state.md` と衝突したため 32 に改番（2026-07-09 rebase 時）。

- **日付**: 2026-07-09（hearing 収束、実装前）
- **branch**: `mako/acp`
- **status**: 設計確定（接続方式・MVP スコープ・diff 型まで決定済み）
- **前提 doc**: [05 §4 Echoes 2モード](05-pane-content-lane-smart-canvas.md)（構想の初出）、[tmux-decoupling](tmux-decoupling.md)（Act I の現行ホスト構造）

## 0. TL;DR

Echoes に GUI モード（**Act II**）を追加する。エンジン接続は **`claude -p --input-format stream-json --output-format stream-json` の常駐プロセス直結**（B案）。ACP adapter は不採用。UI イベント語彙だけ ACP `session/update` のサブセットを借用し、将来のエンジン追加（Antigravity 等）を「翻訳層の追加」で済むようにする。MVP は「**リッチに読む・事後に差分レビュー・リッチに入力する**」。

> ⚠️ **用語再定義**: 「Act」は過去に spawn 構造の用語（Act1=login shell / Act2=tmux / Act3=claude、PR #661）として使われた。旧 Act2（tmux）は tmux decoupling で退役済み。本 doc 以降、**Act = Echoes の UI モード**（Act I = TUI console / Act II = 構造化 GUI）を正とする。

## 1. Why

現行 Echoes は claude TUI の **ANSI バイト列を転送しているだけ**（PtySlot → broadcast → Unison → xterm.js）で、会話構造（メッセージ境界・tool call・diff・plan）を持たない。GUI 化の本質は「バイト列転送」から「**構造化イベントの描画**」への転換であり、構造化データの取得元が最大の設計分岐だった。

## 2. 接続方式の決定（B案）と ACP 不採用の理由

| 案 | 内容 | 判定 |
|---|---|---|
| A. ACP client | 公式 Rust SDK + `claude-agent-acp` adapter | ❌ 不採用 |
| **B. stream-json 直結** | **常駐 headless claude + 自前翻訳層** | ✅ **採用** |

**ACP 不採用の理由**（2026-07 調査、adapter ソース確認済み）:

1. **依存境界の逸脱が二歩**: adapter は Node runtime を要求し、さらに Claude Agent SDK が **platform 別 optional dependency として別の native claude バイナリを同梱**する（`claudeCliPath()` は user の claude を使わない）。「lane の依存は login shell と claude 本体のみ」から二歩外れる。
2. **同型性**: adapter（acp-agent.ts 約 5,600 行）が内部でやることは「常駐 stream-json claude の駆動 + イベント翻訳」であり、B案と土台は同一。差は翻訳層の所有者だけ。
3. **方針一致**: エンジン多様化は「汎用プロトコル 1 本」ではなく「**エンジンごとに適した専用統合を複数持つ**」方針（2026-07-09 決定）。claude には stream-json 直結が最適。
4. claude 本体の native ACP は not planned でクローズ済み（anthropics/claude-code#6686）。subscription 経由 ACP の課金プール統合も一時停止中で不確実性が残る。

**副産物**: 手書きの `protocol/acp.rs`（REQ-PROTO-002 の遺産）は本方針では出番がなく、実装時の削除候補。

## 3. アーキテクチャ

```
SP (Star Platinum)
 ├─ EchoesAgentHost（新規・仮称）
 │   ├─ spawn: claude -p --input-format stream-json --output-format stream-json
 │   │         --include-partial-messages --verbose
 │   │         --permission-mode acceptEdits
 │   │         --resume <cc_session.rs の session id>
 │   │   ⚠️ --permission-prompt-tool は claude 2.1.197 で削除済み（§10 Step 0）。
 │   │      MVP は acceptEdits で auto-apply。対話 permission は control protocol（defer）
 │   ├─ 翻訳層: stream-json → EchoesEvent（§4 の語彙）。**stream_event delta が本流**
 │   └─ 土台 = agent.rs::InteractiveClaudeAgent の昇格・拡張
 │      ⚠️ 現行パーサは stream_event / thinking / tool_result を扱わない（§10）。実質書き直し
 └─ Unison channel "echoes"（仮）で vp-app へ配信（canvas / lanes / terminal と同型）

vp-app
 └─ EchoesChatPane（新規、SolidJS + creo-ui、既存 esbuild/bun 基盤）
     ├─ メッセージ流（marked + mermaid、vp-mdast は将来検討）
     ├─ 事後 diff レビュー / plan ウィジェット / 画像・@-mention 入力
     └─ Act I（xterm）⇄ Act II 切替 = 同一 lane・同一 session id の resume 切替
```

- **1 Act II セッション = 1 常駐 headless claude プロセス**。`claude -p "..."` の都度起動ではない。
- 新規外部依存: **ゼロ**。

## 4. イベント語彙 — EchoesEvent（ACP サブセット借用）

GUI が話す言葉を 1 つに固定する。エンジン追加時は SP 側に翻訳層を足すだけで GUI は無改修（多エンジン方針の支え）。語彙は ACP `session/update` の実績あるサブセットを借用。**由来は Step 0（§10）で確定した実スキーマ**:

| EchoesEvent | 由来 (stream-json 実測) | 描画 |
|---|---|---|
| `session_init` | `system`/`init`（session_id / model / permissionMode / tools / mcp_servers / slash_commands …） | session id 記録（cc_session）・ヘッダ |
| `message_chunk` | `stream_event` → `content_block_delta` → **`text_delta.text`** | ストリーミング本文 |
| `thought_chunk` | `stream_event` → `content_block_delta` → **`thinking_delta.thinking`** | 折りたたみ |
| `tool_call` | `stream_event` `content_block_start`(tool_use: id/name) ＋ 完全 input は `assistant` message の `content[].input` | 実行中インジケータ（§5 配管） |
| `tool_call_update` | `user` message の `tool_result`（tool_use_id 一致）＋ `tool_use_result` サイドカー | 完了/結果 |
| `plan` | `tool_call` name=`TodoWrite` の input | plan ウィジェット |
| `turn_completed` | `result`/`success`（total_cost_usd / session_id） | ターン区切り + diff 集計トリガ |
| `permission_request` | **control protocol**（`control_request`/`can_use_tool`、§10 で defer 判定） | MVP 非対象。acceptEdits で回避 |

- **block index による多重化**: `content_block_delta` は `event.index` を持つ。同一 message 内に thinking(0)→tool_use(1)→text(2) が index で並ぶ。翻訳層は index → 現在の block 種別を追跡して delta を振り分ける。
- **`assistant` 全文 message は delta の累積スナップショット**。streaming 表示には使わず、tool_use の**完全 input 取得**（PR3 diff 用）にだけ使う。
- **ノイズ system subtype**（`hook_started` / `hook_response` / `status` / `thinking_tokens` / `rate_limit_event`）は Act II では基本破棄。`thinking_tokens` は将来の進捗表示に流用余地。
- **sub-agent 帰属**: 全 stream_event が `parent_tool_use_id` を持つ。Task tool のネスト会話はこれで親に紐づく（MVP は非対象、将来のネスト表示の布石）。

## 5. MVP スコープ

**軸 = 「リッチに読む・事後に差分レビュー・リッチに入力する」**（レビュー面であって操作パネルではない）。

| # | 機能 | 内容 |
|---|---|---|
| a | メッセージ流 | markdown / code block / mermaid の本物レンダ、thinking 折りたたみ |
| d | **事後レビュー型 diff** | 編集は acceptEdits で即適用。ターン単位で累積 diff カード（file 一覧 + 展開閲覧）。データは Edit/Write tool イベントから SP 合成（フック不要）。戻しは git。**ゲート型（適用前承認）は非採用**、将来 permission UI と共に再検討 |
| e | plan ウィジェット | TodoWrite → 常設サイドウィジェット |
| f | 画像・@-mention 入力 | 画像 = ペースト → base64 content block。@-mention = 補完 UI は vp-app（fuzzy finder）、実体埋め込みは SP で content block 化（headless は TUI の自動展開に頼れない） |

**配管として必須の薄い最小核**（機能ではなく、詰まらないための配管）:

- tool 実行中の 1 行インジケータ（`🔧 Bash 実行中…`）— ないと長い実行中に画面が凍って見える
- Act II セッションは **acceptEdits 既定**（Step 0 で検証済み、Edit/Write が auto-apply しターンが詰まらない）
- ⚠️ **AskUserQuestion は headless モードで tools に含まれない**（§10 検証）ため、doc 初版の「AskUserQuestion ボタン」配管は headless では発火しない → MVP から外す。headless で人間の判断を要する唯一の経路は control protocol の permission request で、これは MVP 非対象（acceptEdits で回避）。将来 permission ダイアログを入れる時に control protocol ごと実装する

**非スコープ（MVP 後）**: tool call のフル折りたたみカード、permission ダイアログ本体、ゲート型 diff、モデル/モード切替 UI。

## 6. セッション連続性と Act 切替

- session id の SSOT は既存 `cc_session.rs`（`~/.config/vp/cc_sessions/`）を共有。TUI も headless も同じ `~/.claude` セッションストアに載る。
- **Act I ⇄ Act II は resume ベースの切替**。TUI(ANSI) と headless(JSON) はプロセス起動時にモードが決まるため、同一セッションの**同時併走は不可**。
- doc 05 §7 の「Cross-mode Mirror（同 session を Chat と TUI で同時表示）」は本制約により **defer**（単一エンジンプロセス + 二重レンダラが必要になる）。

## 7. Motion design 原則

**「適切な必要なアニメーションを UI に必ず意識して、必要ならばつけていく」**（2026-07-09 user 方針）。アニメーションは装飾ではなく**状態変化の伝達手段**として設計に含める:

| 箇所 | motion の役割 |
|---|---|
| ストリーミング本文 | 到着の連続性（カーソル/フェード） |
| tool 実行インジケータ | 「生きている」ことの伝達（パルス） |
| plan 項目の状態遷移 | todo→in_progress→done の変化を目で追える |
| diff カード | 展開/折りたたみの空間的連続性 |
| ターン完了 | 区切りの settle |

`prefers-reduced-motion` を尊重する。

## 8. 開発パス（Epic 実装プラン、直列 4 PR）

この lane（`mako/acp`）で直列に進める。各 PR は単独で nightly に載り、単独で dogfood 可能。
順序の根拠: **最深部（claude CLI という外部要因）のリスクを PR1 で先に潰し**、以降は純粋な加算にする。UI は PR1 で凍結した EchoesEvent 語彙の上にだけ建てる（手戻り防止）。

### PR1 — 配管: EchoesAgentHost + EchoesEvent + Unison channel

**Step 0（スパイク、PR1 内の最初の作業）**: 現行 claude CLI で以下を実機確認し、結果を本 doc の付録に記録する。`agent.rs::ClaudeMessage` は過去の CLI 向けに書かれているため、**スキーマ乖離の検出が最優先**。
- `claude -p --input-format stream-json --output-format stream-json --include-partial-messages` の実イベント列（init / partial delta / tool_use / tool_result / result）
- headless での `--resume <id>` の挙動（session id の引き継ぎ規則）
- `--permission-mode acceptEdits` 相当のフラグ名と、`--permission-prompt-tool mcp__vantage-point__permission` の往復形
- AskUserQuestion が stream にどう現れ、どう応答するか

**本体**:
- `crates/vantage-point`: `EchoesAgentHost`（仮称、命名は stands.rs 規約で確定）。lane 単位で headless claude を常駐 spawn・停止・異常時 respawn。`agent.rs::InteractiveClaudeAgent` を土台に昇格
- `EchoesEvent` enum（§4 の語彙、serde）と stream-json → EchoesEvent 翻訳層。**この PR で語彙を凍結**
- session id は init イベントから直接 `cc_session.rs` に記録（wire hook 経由にしない。headless への wire hooks 注入要否は本 PR で判断・記録）
- Unison channel `"echoes"`（仮）で配信 — canvas / lanes / terminal と同型パターンを踏襲
- 【発見タスク】vp-app → SP の入力経路: 既存の xterm キー入力が PtySlot に届く経路を特定 → `terminal_write` と同型の `echoes_submit` dispatch で通す ✅ 特定・実装済み

**Exit criteria（SP 側）**: ✅ **達成（2026-07-09）**。実 claude 統合テスト `echoes_submit_roundtrip` が dispatch→topic 終端で SessionInit/MessageChunk/TurnCompleted を通し、host 再利用も確認。SP 再起動後 resume は cc_session + `--resume` で構造的に担保（host 実装済）。

> **スコープ変更（2026-07-09 決定）**: PR1 の vp-app 側（デバッグ表示）は **PR2 に統合**。理由: 購読ループ + IPC submit は PR2 EchoesChatPane が再利用する恒久ブリッジであり、throwaway なデバッグ pane を挟むのは pre-MVP 方針に反する。SP 契約は自動テストで証明済みなので、vp-app は本番 Console UI（PR2）と一体で作る。**PR1 = SP 側で完了**。

**PR1 成果物**（commit 20e9c6b / bc8b898 / ae93f73、mako/acp）:
- `echoes/{event,translate,host}.rs` + `process/echoes_pump.rs`
- `ProcessMessage::EchoesEvent` + topic `process/echoes/data/{lane}/event`
- `unison_server: echoes_submit` dispatch + `ensure_echoes_host`（lazy spawn/再利用）
- AppState `echoes_hosts`/`echoes_pumps`

### PR2 — 読む: vp-app 恒久ブリッジ + EchoesChatPane（a + 薄い配管 + e）

> **再編（2026-07-09、doc 33）**: PR2a（Rust ブリッジ）実装後の構造リデザインにより、本 PR の残りは **doc 33 の C1（Console 骨格 = engine 排他スロット + console_mode + vpConsole facade）→ C2（ChatView）** に分割・置換された。エンジン排他の不変条件と reconcile 安全（chat lane を Dead 扱いしない）が C1 で先行する。以降の実装順序の SSOT は **doc 33 §7**。

- **vp-app 恒久ブリッジ**（PR1 から統合、throwaway でない）: `spawn_echoes_session`（`spawn_terminal_session` 同型）で `process/echoes/data/{lane}/event` を "canvas" channel 購読 → `AppEvent::EchoesEvent { lane, event: Value }` → JS へ。IPC `echoes:submit {lane, prompt}` → `echoes_submit` request。⚠️ lane reconcile 中枢（`terminal_sessions` map 隣接）を触るため teardown バグ（memory: performer console snapshot teardown）に注意
- vp-app に `EchoesChatPane`（SolidJS + creo-ui、marked + mermaid は既存 dep）
- メッセージ流: streaming 描画（カーソル/フェード、§7）、code block、mermaid、thinking 折りたたみ
- 薄い配管: tool 実行 1 行インジケータ（パルス）/ AskUserQuestion 最小ボタン（応答は PR1 の入力経路に相乗り）/ acceptEdits 既定
- plan ウィジェット: TodoWrite → 常設サイド表示、状態遷移アニメーション
- 入口は dev 向け最小（lane メニュー等から Act II を開く。正式な切替 UI は PR4）
- `prefers-reduced-motion` 対応をこの PR から

**Exit criteria**: 実タスクの会話 1 本を GUI だけで完走できる（読める・詰まらない・plan が動く）。

### PR3 — レビュー: 事後 diff カード（d）

- SP: ターン内の Edit / Write / MultiEdit / NotebookEdit tool イベントを蓄積し、`turn_completed` に diff summary（file 一覧 + 加減行数）を添付。per-file diff は tool input の old/new から合成（新規 Write は全行追加扱い）
- vp-app: diff カード（file 一覧 → 展開で unified diff 閲覧、展開/折りたたみアニメーション）
- **検証**: 実編集ターン後、カードの diff が `git diff` と一致すること

**Exit criteria**: 編集を含むターンの完了時にカードが出て、内容が git と一致する。

### PR4 — 入力: 画像・@-mention（f）+ Act 切替

- 画像: WebView ペースト → base64 → user message content block
- @-mention: 補完 UI は vp-app（プロジェクトファイルの fuzzy finder、データ源は既存ファイル基盤を流用）、実体は SP で content block 埋め込み
- Act I ⇄ Act II 切替 UI: 同一 session id の resume 切替（相手モードのプロセス停止を伴うため確認 UX を挟む、§6）

**Exit criteria**: 画像を貼って言及させられる / @file の内容を claude が読める / 会話途中の TUI→GUI 切替で文脈が継続する。

### 実装運用ルール（全 PR 共通）

- **実装モデル**: Opus（設計変更・多段因果の詰まりは Fable にエスカレーション — model-tier 方針）
- **ship flow**: 実装 → team-b レビュー → `gh pr create --base nightly` → auto-merge ON（順序厳守）
- **GitNexus**: symbol 編集前に `impact`、commit 前に `detect_changes`。index 更新は `bunx gitnexus analyze`
- **dogfood 安全**: この lane は VP 上で動いているため、検証時の daemon 再起動は **gentle（`vp daemon stop`）のみ**。cascade（`mr daemon`）は自分の claude を殺す。dev 検証は `VP_PROFILE=dev` + `vpd` + `cargo install --locked`
- **pre-MVP 原則**: 最短で canonical、中間状態・dead code を作らない
- **cleanup（Epic 末尾、別小 PR）**: 未使用の手書き `protocol/acp.rs`（+ ToAcp trait 群）の削除。blast radius があるため本流 4 PR には混ぜない

## 9. 未決事項

- `EchoesAgentHost` / channel 名 / Content kind（`echoes` mode: chat、doc 05 案）の最終命名 → stands.rs / 実装時
- pane の役割語彙（EchoesChatPane = **Console**、表示面 = **Monitor**、媒体 = **Canvas**）は **doc 31** で確定 — PR2 は doc 31 の語彙で実装する
- performer lane への Act II 適用範囲（conductor 先行か）
- plan mode（claude の permission mode）の Act II での扱い
- vp-mdast への置換タイミング（MVP は marked で開始）

## 10. 付録: Step 0 スパイク結果（現行 CLI の実スキーマ）

- **環境**: claude 2.1.197 (Claude Code)、macOS、`--model haiku` で実測（2026-07-09）
- **生ログ**: scratchpad の `spike/spikeA.jsonl`（プレーン）/ `spikeB.jsonl`（Read+Edit）/ `spikeC.jsonl`（resume）/ `spikeD.py`（permission 調査）
- **結論**: **doc 前提と実スキーマに複数の乖離あり。§3/§4/§5 は本節を反映して更新済み。** 語彙（EchoesEvent）自体は変更不要 — 由来のマッピングだけ実測に合わせた。

### 10.1 確定した乖離（doc 前提 → 実測）

1. **`--permission-prompt-tool` は削除済み**（2.1.197 の `--help` に無い）。doc 初版 §3 の spawn 行は誤り。MVP は `--permission-mode acceptEdits` で auto-apply（Spike B で Edit が承認なしに適用され、ターンが詰まらないことを確認）。permission mode 値は `default` / `acceptEdits` / `bypassPermissions` / `plan`。
2. **ストリーミングは `stream_event` 経由**（Anthropic Messages API 形式を `{"type":"stream_event","event":{…}}` で包む）。現行 `agent.rs::ClaudeMessage` は `stream_event` を**一切パースしない**（`assistant` 全文の prefix-diff で代用）。Act II は delta ベースに書き直す。
3. **tool_result は `user` タイプ message で戻る**（`content[].tool_result`、`tool_use_id` 一致）＋ 構造化 `tool_use_result` サイドカー。現行パーサは `user` を無視し tool 結果を捨てている。
4. **tool の完全 input は `assistant` message に載る**。Edit なら `input = {file_path, old_string, new_string, replace_all}`。→ **PR3 の diff 合成は input_json_delta を組み立てず assistant message から直接読める**（実装が大幅に簡単）。
5. **`AskUserQuestion` は headless の tools 一覧に無い**。doc 初版 §5 の「AskUserQuestion ボタン」配管は headless では発火しない → MVP から除外。
6. **`--resume <id>` は session_id を保持**（`--fork-session` 無しで同一 id 継続、文脈も保持）。Act I ⇄ II 切替の前提が実証された（Spike C）。
7. **`system/init` は想定より遥かにリッチ**: `session_id` / `model` / `permissionMode` / `cwd` / `tools[]` / `mcp_servers[]`（status 付き）/ `slash_commands[]`（164 個）/ `skills[]` / `agents` / `output_style` / `memory_paths` / `claude_code_version`。slash_commands が取れるのは将来の slash command サポートの布石。

### 10.2 1 ターンの実イベント列（Spike B: Read→Edit→text）

```
system/hook_started ×N, system/hook_response ×N   （global hook 群、ノイズ）
system/init                                       → session_init
system/status                                     （ノイズ）
stream_event message_start                        （message 境界）
stream_event content_block_start (thinking)       → thought block 開始
stream_event content_block_delta (thinking_delta) → thought_chunk
system/thinking_tokens ×N                         （ノイズ）
stream_event content_block_delta (signature_delta)（署名、破棄）
stream_event content_block_stop
stream_event content_block_start (tool_use: Read) → tool_call（id/name）
stream_event content_block_delta (input_json_delta)× → 完全 input は下の assistant を使う
assistant {content:[tool_use{input:{…}}]}         → tool_call の完全 input 確定
stream_event content_block_stop
user {tool_result, tool_use_result}               → tool_call_update（結果）
stream_event message_delta, message_stop
（次 message: Edit tool_use → user tool_result → …）
（最終 message: content_block_start(text) → text_delta× → text）
result/success {total_cost_usd, session_id}       → turn_completed
```

### 10.3 翻訳層の実装方針（PR1 で凍結）

- **状態機械**: `message_start`→`message_stop` を 1 assistant message、`content_block_start`→`stop` を 1 block とし、`event.index` で block 種別（thinking/text/tool_use）を追跡。`content_block_delta` の delta を index の種別に応じて `thought_chunk`/`message_chunk`/（tool input 蓄積）へ振り分ける。
- **tool_call の発火**: `content_block_start`(tool_use) で id/name を得て即 `tool_call`（実行中表示）。完全 input は直後の `assistant` message で確定させ、`tool_call` に input を後追い添付 or `tool_call` 自体を assistant message 契機で発火（PR1 実装時に一方に決める）。
- **破棄リスト**: `hook_started`/`hook_response`/`status`/`thinking_tokens`/`rate_limit_event`/`signature_delta`。
- **serde**: `stream_event` は `#[serde(tag="type")]` の外側 enum ＋ `event` の内側 enum（`content_block_delta` の delta は `#[serde(tag="type")]` で `text_delta`/`thinking_delta`/`input_json_delta`/`signature_delta`）。`serde(other)` で未知を安全に握り潰す。
