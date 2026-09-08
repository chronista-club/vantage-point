# doc 60 — vp-app の module 配置と依存 rule（app / daemon / lane / webview / flows）

> **Status**: 実装中（6-0 / 6-0b / 6-1 = 2026-09-08 着地、次は A / B / C、6-2 は別 conception）
> **Date**: 2026-09-08
> **Owners**: vp-app（`crates/vp-app`）
> **Supersedes**: [doc 11](11-vp-app-refactor.md) §2 / §5 Q3 / Q4（flat 前提の分割案）
> **台帳**: `.vp/reports/refactor-audit-2026-09-07.md` 項目 6。review: board「app.rs 階層分割案へのレビュー」「統合案 v3 — 再レビュー」（Codex、2026-09-08）

## 0. 何を決めたか

- `crates/vp-app/src/app.rs`（7,117 行 = vp-app の 47%、直近 100 commit で 18 回変更）を、**軸ごとの directory** に分ける。
  server crate（`crates/vantage-point/src/{daemon,repo,lane,conversation,…}`）と同じ形にし、2 つの crate を同じ頭で読めるようにする。
- 判定基準（mako 2026-09-08）: **変更時に触る場所がまとまる** / **state と resource の所有者が明確** / **file を開いた瞬間に何の module か分かる**。行数は観測であって目標にしない。
- 段階: **6-0 配置替え**（既存 sibling を `git mv`、挙動差ゼロ）→ **6-0b** `client.rs` の分解と dead 削除 → **6-1 切り出し**（app から byte 一致で移設、`run()` は据え置き）→ **統合**（挙動を変える PR は別建て）→ **6-2** `run()` の分解（別 conception）。

## 1. 目標 tree

```
crates/vp-app/src/
├── main.rs / lib.rs
│  ── 共有型・共有 state（crate root。どの directory からも参照してよい）──
├── events.rs            AppEvent（tao EventLoop の UserEvent。旧 terminal.rs から移設 = doc 11 Q4）
├── daemon_wire.rs       ← client.rs の wire 型（6-0b。LaneInfo / RepoInfo / LaneSessionsWire …）
├── lane_address.rs      LaneAddressWire + lane_key_to_wire_agent（6-1。逆写像は server 側 repo/delivery_actor.rs、往復は両側の test）
├── pane.rs / session_state.rs / settings.rs
├── generated/           KDL codegen（webview との wire 型）
│  ── 処理 ──
├── app/                 UI thread の世界 = state 遷移と効果の生成・実行（6-2a、2026-09-08）
│   ├── mod.rs           run(): boot() → proxy 3 本 → event_loop.run(閉包 = preamble + routing table 57 行)
│   ├── boot.rs          struct Boot（resource: webview / window / menu / tray / daemon_conn / rt_handle / _rt / _log。
│   │                      run() が最後まで所有、Send ではない）+ fn boot() -> (EventLoop, Boot, UiState)
│   ├── state.rs         struct UiState { settings, session_state, sidebar_state, …, guards: Guards,
│   │                      win: WindowState, sessions: LaneSessions }（旧 let mut 21 個）+ GEOMETRY_SAVE_THROTTLE
│   ├── lane_view.rs     調整役 helper（activate_lane / maybe_respawn_dead_lane / ensure_conversation_attach /
│   │                      push_active_view / roster 4 本 / term_sessions_of / mark_lane_* …）。field を明示引数で受ける
│   ├── on_window.rs     Close / Resized / Moved / Focused / ShellLayout / SlotRect / MenuClicked + persist_window_geometry
│   ├── on_lanes.rs      ReposLoaded / LanesLoaded / LanesError / LaneRespawnFailed / ReposError / SubCreateResult（lanes snapshot の所有者）
│   ├── on_conversation.rs Conversation* / Session* / Console* / Agents* / OscNotification
│   ├── on_terminal.rs   TerminalOutput / Write / Resize / PasteText
│   ├── on_board.rs      CanvasMessage / EditorEval / BoardMutate（board_snapshots の所有者）
│   ├── on_sidebar.rs    SidebarIpc（fast path + handle + 28 段の効果実行、1 fn）/ UpdateFlowPhase / Settings* + settings_snapshot
│   ├── on_misc.rs       Titles / Inboxes / Ink / Debuglog / Device / Code / Wire / Activity
│   ├── catch_up.rs      WebviewReady（§3「1 つの list」。新しい面の replay はここに足す）
│   └── sidebar_ipc.rs   SidebarIpcOutcome + handle_sidebar_ipc（純粋、A-2）+ characterization test 18 本（A-1）
├── daemon/              daemon との線
│   ├── conn.rs          SharedDaemonConn / conn manager（再接続の唯一の所有者）（6-1）
│   ├── control.rs       DaemonControl（制御 RPC。統合 1 で repo_request を持つ）
│   ├── subscriptions.rs lanes / canvas / device の購読 pump（6-1。方針差は §4 の表）
│   ├── pollers.rs       activity / title / inbox / actions persist / repo fetch / sp_start（6-1）
│   ├── health_probe.rs  HTTP /api/health（Unison が壊れた時の診断用、doc 45）（6-0b）
│   ├── launcher.rs      daemon の起動確認 / 自動起動
│   └── restart.rs       「daemon を再起動」フロー
├── lane/                lane ごとの session（daemon/conn を握る）
│   ├── terminal.rs      TermCmd / LaneTerminal（6-1）
│   ├── conversation.rs  ConversationCmd / LaneConversation（6-1）
│   └── title.rs         session title の解決
├── webview/             webview との線 = IPC の decode と Rust→JS の投影
│   ├── push_main.rs / push_sidebar.rs / ipc_route.rs / editor_bridge.rs（6-1）
│   ├── terminal_ipc.rs  main_area webview からの IPC handler（decode → AppEvent）
│   └── main_area.rs / assets.rs / ink_snapshot.rs / file_explorer.rs
├── flows/               別 thread で走る対話 flow: auth.rs / update.rs / repo_dialog.rs
└── log_init.rs / log_format.rs / debug_log.rs / icon.rs / menu.rs / tray.rs / conversation_submission.rs
```

## 2. 依存 rule

「共有型の参照」と「処理の呼び出し」を区別する（Codex review ②）。

- **共有型（crate root）の参照は自由**。`events` / `daemon_wire` / `lane_address` / `pane` / `session_state` / `settings` / `generated`。
- **処理の呼び出しは一方向**: `app` → `daemon` / `lane` / `webview` / `flows`。
  - `lane` → `daemon/conn`（session が接続を握る）は許す。
  - `flows` → `daemon`（repo picker が add / start / fetch、update が `locate_vp_binary`）は許す。
  - `daemon` / `lane` / `webview` は上記以外で互いの関数を呼ばない。
- **既知の例外（移設直後に残る）**: `daemon/subscriptions`（canvas 購読）→ `webview/editor_bridge::editor_bridge_js`。
  解消は「購読側は op を `AppEvent` で渡し、JS 構築は UI 側で行う」= `AppEvent` の契約変更なので 6-2 か独立 PR。
- `pane.rs` は data only（doc 11 §2.3）。`AppEvent` は `Clone`（`EditorEval` が oneshot でなく `mpsc` を運ぶ理由）。
- `tokio::spawn` は crate 全体で禁止（`clippy.toml`）。async を起こす module は `rt_handle: &tokio::runtime::Handle` を引数で受ける。

## 3. 変えないもの（seam の内側に住む不変条件）

- **boot 窓の catch-up は 1 つの list**（`AppEvent::WebviewReady` の handler）。roster の指紋 gate を通す経路と通さない経路は「どの関数を呼ぶか」で区別する（doc 53 §11）。module を跨いで散らさない。
- **再接続は `SharedDaemonConn` の manager だけが所有**。購読 loop は自前の reconnect を持たない。
- **lane ごとの cmd channel は再接続を跨いで生きる**（切断中に積まれた write / resize は次接続で送る）。
- **conversation の demand は初回も再接続も毎回撃つ**（前任 GUI の残留購読で edge が立たない事故の対策）。
- **購読の寿命は accordion の可視性**（1→0 で daemon の demand hook が engine を寝かせる）。
- `lane_key_to_wire_agent` と逆写像 `wire_agent_to_lane_display` は対で動く（doc 44）。逆写像は crate を跨ぐ（`vantage-point::repo::delivery_actor`）ので同居できず、往復は両側の test で固定する。

## 4. 再接続 loop 5 本の方針（契約。共通化は出荷条件にしない）

骨格（`wait_client → run_session → Disconnected / AppClosing`）は似ているが、障害時の方針が違う（Codex 再レビュー ①）。共通化するなら接続待ち・retry・終了・後始末の責任が引数で読める場合だけ。読みにくくなるなら 5 本の重複を残す。

| loop | 接続待ち | 失敗時 | 終了 | 後始末 |
|---|---|---|---|---|
| lanes | 12 秒で `LanesError` を UI に出す（stalled 表示 → user が daemon restart できる）| 再試行を続ける | app 終了 | — |
| canvas | 待つ | session error は 500 ms 待って再試行 | app 終了 | — |
| terminal | 待つ | 再試行。`cmd_rx` を再接続越しに保持 | 送り手（`LaneTerminal`）消失で終了 | — |
| conversation | 待つ | 再試行。購読後に毎回 demand | collapse 等で終了 | 明示 `unsubscribe` |
| device | 待つ | 指数 backoff、`MAX_FAILURES` で終了（MIDI 非提供時の意図した縮退） | 上限到達で終了 | 切断で失敗回数 reset |

## 5. 移設の照合方法

- **順序付き比較**: 関数・型ごとに移設元と移設先の本文を順序を保って `diff`。sort して比べる方法は順序と所属（`#[cfg]` の付き先、`set_repo_expanded → save` の順）を失うので合格根拠にしない（Codex 再レビュー ③）。
- 許容差分（`use` / 可視性 / indent）は別枠の checklist。whole-file の `git mv` は `git diff -M` の rename 表示で足りる。
- **位置依存の追従**（Codex review ④）: `include_str!` の相対 path、`log_init.rs` の filter target（module path）、`tests/` の `vp_app::…` path、file 名を書いた現行 doc / コメント。歴史 doc は触らない。
- **挙動を変える PR は移設 PR と分ける**。契約を PR 本文に明記し、実機で確認。

## 6. 段階と PR

- **6-0**（1 PR）: (a) `events.rs` 抽出 → (b) directory + `git mv` + `lib.rs` + path 書き換え + `include_str` / log filter → (c) doc / コメント → (d) 本 doc
- **6-0b**（小 PR）: `client.rs` → `daemon_wire.rs` + `daemon/health_probe.rs`、`shell_detect.rs` 削除
- **6-1**（各 1 PR、順序付き照合）: push_main → editor_bridge → push_sidebar → **app/sidebar_ipc**（そのまま移設、`session.save()` 込み）→ daemon/pollers → lane_address → webview/ipc_route → daemon/conn → daemon/subscriptions → lane/terminal + lane/conversation
- **A. sidebar**（✅ 2026-09-08）: A-1 #1055 で現行を固定する test 18 本（25 arm の状態変化・`session.save()` の有無（temp dir）・outcome）→ A-2 で `save` 2 箇所（`ProcessToggle` / `ProcessReorder`）を outcome の `session_save` にして `run()` が実行（test 本体は不変、`apply` helper が呼び手を模す）。codegen PR-2 / PR-3 は既に済だった（Rust / TS とも生成型を使用）
- **B. 統合 1: daemon ask の一本化**（✅ 2026-09-08、独立 PR、Codex 再レビュー ②）: `daemon_repo_request`（呼ぶたびに QUIC connect、26 箇所）→ `DaemonControl::repo_request`。契約: **1 RPC = 1 stream**（request ごとに `open_channel("repo-proxy")` → handshake → request → 必ず `close()`）/ `REPO_ASK_TIMEOUT` = 現行と同じ 30 秒を別 const（`RPC_TIMEOUT` 10 秒に短縮しない）/ 応答を失った更新・削除は自動再送しない / `{"error"}` と transport 障害を区別
- **C. loop 共通化**（❌ 不採用、mako 2026-09-08）: 5 本の違いは「順序と後始末」（切断中の keystroke 保持 / collapse 時の unsubscribe / 失敗回数の reset）で、共通化すると引数の山になり読みにくくなる一方、利益は行数だけ。§4 の表を契約として残す。再接続の挙動を触る PR が出た時に、その loop の厳密 test（擬似 daemon harness）をその PR で置く
- **6-2a**（✅ 2026-09-08、PR #1058〜#1067 の 10 本）: PR-1 `Boot` / `UiState`（閉包は `ui.` / `boot.` の prefix 以外 byte 一致）→ PR-2 `lane_view` → PR-3〜10 arm を fn ごとに `on_*` / `catch_up` へ（本体は 12 空白 dedent → rustfmt で一致、先行コメントは fn の doc へ移動、routing 行は 100 桁超なら block 形、引数 8 個以上は `#[allow(too_many_arguments)]`、fn の引数が既に参照の proxy は `&` を外す）。`run()` 3,090 → 320 行、`app/mod.rs` 4,168 → 540 行。test 1313 維持。handler は `&mut UiState` 全体を受ける（範囲の絞り込みは §2 の表を見て module 単位で後から）。`Vec<SidebarEffect>` は延期（第 2 の生成者が現れるまで、mako 2026-09-08）
- **6-2b**（挙動を変える、test 先行）: §8 の復元と保存の表 → `app/persist.rs`（SessionState の所有者 = 復元 cursor + 保存への変換 1 箇所）→ 危険 A（壊れた file の退避）/ B + C（pending 未消費の間 daemon 値を書かない）/ E + F（daemon 順と auto-expand を session に鏡す）/ b-5（WebviewReady で push_sidebar_state）/ D（1 nightly log してから）。b-7（window 間の並び順伝播）は daemon push で項目 7 と一緒に
- **6-2 PR-EX**: 既知の例外（`daemon/subscriptions` → `editor_bridge_js`）を `AppEvent::EditorCommand` で解消

## 7. 検証で保持する動作

| 対象 | 保持する動作 |
|---|---|
| 移設 | 関数・型ごとの順序付き比較、`#[cfg]` の付き先、既存 test 数、log target、埋め込み asset、Windows `cargo check --all-targets` |
| 起動 | sidebar / console / chat が出る（boot 窓 catch-up）。snapshot 未着中の保存で復元値を消さない |
| lanes 待機 | daemon が戻らなくても 12 秒で UI に error、その後の復帰も拾う |
| device | MIDI 非提供時は規定回数で終了。切断は失敗回数 reset |
| conversation | 初回・再接続とも demand、collapse で unsubscribe、期限切れ submit は再送しない |
| repo ask（統合 1 後） | 並行操作で宛先 / 応答が混ざらない。失敗 / timeout 後に stream が残らない |
| sidebar | toggle / reorder の保存、再描画の抑制、効果の順序 |
| 再接続 | daemon 再起動で lanes / canvas / device / terminal / chat が全部復帰。accordion 折り畳みで購読解除 |

## Status log

- 2026-09-08: 6-0 (a)(b)(c)(d) 着地（PR #1043）。app.rs は `app/mod.rs` に、sibling 14 file を directory へ。
- 2026-09-08: 6-0b 着地（PR #1044）。`client.rs` → `daemon_wire.rs` + `daemon/health_probe.rs`、`shell_detect.rs` 削除。
- 2026-09-08: 6-1 移設 10 本すべて着地（PR #1045〜#1054）: push_main / editor_bridge / push_sidebar / app/sidebar_ipc /
  daemon/conn / daemon/subscriptions / daemon/pollers / lane_address + daemon/wire / webview/ipc_route / lane/terminal +
  lane/conversation。各 PR は順序付き diff で本文一致（差分は `use` / 可視性 / dedent）、test 1295 を維持。
  `app/mod.rs` は 7,117 → 4,168 行（`run()` 据え置き）。次は A（sidebar test 先行 → 純粋化）→ B（ask 一本化）→ C（採否）→ 6-2。
- 2026-09-08: A 着地（A-1 #1055 test 先行 / A-2 純粋化）。`handle_sidebar_ipc` は file を書かない。次は B（ask 一本化）→ C（採否）→ 6-2。
- 2026-09-08: C は不採用（共通化しない、test も今は足さない）。次は 6-2 の conception。
- 2026-09-08: 6-2a 着地（PR #1058〜#1067）。`run()` は routing table だけになり、resource は `Boot`、可変 state は `UiState`、arm は `on_*` / `catch_up`、調整役は `lane_view`。次は 6-2b（復元と保存の表 → persist.rs → 危険 A〜F）と PR-EX。実機（mako、`app:swap`）: boot / geometry 復元 / Cmd+N / close / resize / chat / mode 切替 / ROTO switch_lane（2 window）/ reopen / settings:save / toggle・reorder の永続。
- 2026-09-08: B 着地。`daemon_repo_request` は共有接続の `DaemonControl::repo_request`（1 RPC = 1 stream / 必ず close / `REPO_ASK_TIMEOUT` 30 秒 / 自動再送しない）に一本化、呼び手 25 箇所は第 1 引数が port → `&SharedDaemonConn`。次は C（採否）→ 6-2。実機: daemon 再起動中の lane 操作が失敗として見えること / 失敗後に stream が残らないこと（mako、`app:swap`）。
