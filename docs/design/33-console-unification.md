# 33. Console 統合 — Act I/II 交通整理

- **日付**: 2026-07-09（PR2a まで実装済みの時点での構造リデザイン）
- **status**: 設計確定
- **関連 doc**: [31 語彙（Console/Monitor/Canvas）](31-console-monitor-canvas.md) / [32 Echoes Act II](32-echoes-act2-gui.md) / [tmux-decoupling](tmux-decoupling.md)
- **動機**: Echoes の TUI（Act I）と GUI（Act II）が混在し始め、エンジン排他・lane 生存判定・JS 二世界・命名の 4 方向で交通渋滞が起きる前に、強い不変条件で交通整理する（user 指示 2026-07-09）。

## 0. TL;DR — 法

> **1 lane = 1 Console = 高々 1 エンジン = 1 cc_session。**

- エンジンは **排他スロット**: `Tui(PtySlot+claude TUI)` xor `Headless(EchoesAgentHost)` xor 空
- モード切替 = 旧エンジン stop → `console_mode` 永続 → 新エンジン `--resume` spawn（cc_session が背骨）
- `echoes_submit` は **mode=chat でのみ受理**。暗黙切替はしない（生きた TUI をストレイ submit で殺さない）
- この不変条件は **SP の LanePool が構造的に保証**する（呼び出し規約に頼らない）

## 1. 渋滞の棚卸し（Why）

| # | 渋滞 | 現状 |
|---|---|---|
| 1 | エンジン二重駆動 | lane 起動で PTY claude が居るのに `echoes_submit` が同じ cc_session に headless を `--resume` で立てられる（1 会話 2 エンジン） |
| 2 | JS 二世界 | Act I xterm = main_area.rs インライン JS（World A）/ Act II SolidJS = esbuild バンドル（World B）。同一面が 2 コードベース |
| 3 | lane 生存判定が PTY 前提 | vp-app reconcile は pid=None → Dead → respawn。chat lane（PTY 無し）を殺す（#683 の再演コース） |
| 4 | 命名三重 | `pane-terminal` / `data-kind="terminal"` / `data-frame-id="echoes"` + 新規 `vpEchoes` で四重化寸前 |
| 5 | モード状態に家が無い | lane が今 TUI か GUI かを誰も所有・永続しない |

## 2. SP: ConsoleEngine スロット（渋滞 1・5 の根治）

```
             console_set_mode(chat)
   ┌──── stop PtySlot → mode 記録・永続 ────┐
   │                                        ▼
 [TUI]                                   [CHAT]
 PtySlot(claude TUI)              EchoesAgentHost(初回 submit で lazy spawn)
   ▲                                        │
   └──── stop host → spawn PtySlot(--resume) ┘
             console_set_mode(tui)
```

- **`ConsoleMode { Tui, Chat }`** を lane 単位で永続: `lane/console_mode.rs`（`cc_session.rs` と同型の state file、`vp_state_dir()/console_modes/<project>__<lane>`、default = Tui）
- **engine 所有の一元化**: `AppState.echoes_hosts`/`echoes_pumps` を **LanePool へ移管**。`pty_slots` と併せて「lane のエンジンスロット」を LanePool が一元所有し、排他を型/実装で保証（同時二重を作る経路を消す）
- **`console_set_mode` dispatch method**（新設）: 上図の遷移を実行。stop → record → spawn(--resume)。chat→spawn は lazy（submit まで engine-less で良い）
- **`echoes_submit` は mode=chat ガード**: mode=tui なら Err（「console_set_mode で切替えてから」）。`ensure_echoes_host` も同ガード下に置く
- **boot 時**: `with_conductor` / lane spawn 系は `console_mode::last` を読み、**chat なら PtySlot を spawn しない**（engine-less で lane 登録、submit で host が立つ）
- 適用範囲: mode=Chat が有効なのは stand="echoes" の lane のみ（shell 等は常に Tui）。conductor / performer は一様に同モデル

## 3. wire + reconcile 安全（渋滞 3 の根治、#683 の再演防止）

- `LaneInfo` に **`console_mode`** field を追加（serde default = tui で wire 後方互換）
- vp-app の Dead-lane 自動 respawn 判定を「**mode=tui かつ pid=None**」に限定。chat lane は respawn 対象外（engine-less は正常状態）
- chat lane の `pid` = host 稼働中はその pid / idle は None。sidebar は mode バッジで区別（C2 で最小表示）
- ⚠️ generated TS（`webview/src/generated/LaneInfo.ts` 等）の再生成が要る

## 4. vp-app: Console facade（渋滞 2・4 の交通整理）

- **`window.vpEchoes` → `window.vpConsole` に改名**（PR2a の evaluate_script 1 行 + 新 TS）。理由: EchoesEvent 語彙は engine 非依存（doc 32 §4）であり、sink は**面（Console）**の名を持つべき。将来 Antigravity engine のイベントも同じ sink に流れる
  - data plane: `vpConsole.handleEvent(lane, event)`
  - control plane: `vpConsole.setMode(lane, mode)`（Rust push）
- **World B に `webview/console/` module**: per-lane `ConsoleHost` が chat container の mount と mode 表示切替を所有。C1 時点では handleEvent は per-lane ring buffer に蓄積（C2 の ChatView mount 時に replay。devtools から `vpConsole.peek(lane)` で検分可能 = throwaway デバッグ pane を作らずに検証可能性を確保）
- **World A（インライン xterm JS）は不可侵**: input-doubling 調査（VP_TERM_TRACE hop A/B）の診断ベースラインを壊さない。xterm の bundle 移管は input-doubling 決着後の専用 PR
- **ビューとエンジンの分離（2026-07-09 user 要件）**: 排他なのは**エンジン**であって**ビューではない**。Lane 内で Act I pane（xterm）と Act II pane（chat）は**共存し得る**（split 表示等、後追加）。既定 UX は「エンジンに一致するビューを表示」だが、非アクティブ側ビューは履歴として表示可能（engine=chat 時の xterm scrollback / engine=tui 時の chat 履歴）。将来: TUI エンジンの会話を **session transcript（`~/.claude/projects/`）追尾 → 翻訳層 → EchoesEvent** で chat ビューに読み取り専用ミラーする道が開いている（エンジン排他を破らない cross-mode mirror、doc 05 §7 の実現路）
- DOM 構造（既存 lane-host は触らない）:

```
#pane-terminal（現名のまま、rename は PR1.5）
  ├─ lane-host[data-lane=X]      ← World A 所有（xterm、既存のまま）
  └─ .console-chat[data-lane=X]  ← World B 所有（ChatView mount 先、C1 で骨格）
  既定はエンジンに一致する側を表示（vpConsole.setMode が司る）。
  両方の同時表示（split）は layout の自由 — mode は表示を強制しない（上記ビュー/エンジン分離）
```

## 5. 型の契約

- `EchoesEvent` / `ConsoleMode` に **ts-rs derive**（repo 既存の generated/ 経路に乗せる）→ `webview/src/generated/EchoesEvent.ts`。ChatView は typed switch で描画（stringly-typed の増殖を止める）

## 6. 非目標（over-scope 防止、各理由付き）

- `window.vpXxx` 全体の event-bus 統一 → Console 以外に波及する大手術。別 doc
- xterm インライン JS の bundle 移管 → input-doubling 診断ベースライン保護（§4）
- `pane-terminal` 等の DOM id/kind rename → PR1.5（doc 31 語彙実装）で
- vp-app 側 terminal_sessions / echoes_sessions map の統合 → transport は独立で害なし
- ゲート型 permission / plan mode UI → doc 32 の非スコープ踏襲

## 7. 実装順序（Epic 更新: 旧 PR2 を分割）

| PR | 内容 | Exit |
|---|---|---|
| **C1 — Console 骨格（交通整理本体）** | §2 SP engine slot + console_mode + `console_set_mode` + submit ガード / §3 wire + reconcile 安全 / §4 vpConsole facade + ring buffer / §5 ts-rs | 実機: tui⇄chat 切替で同一会話が継続（`vp lane capture` と `vpConsole.peek` で両モード確認）。二重エンジンが**作れない** |
| **C2 — ChatView（旧 PR2b）** | SolidJS ChatView（MVP a: streaming markdown + thinking 折りたたみ + tool 1 行 + e: plan ウィジェット + motion）+ **最小 Act toggle**（explicit 切替に必須。**root = conductor の Console 先行**、2026-07-09 user 要件。performer への露出と正式な切替 UX は C4） | 実会話 1 本を GUI だけで完走 |
| C3（旧 PR3） | 事後 diff カード | doc 32 §8 のまま |
| C4（旧 PR4） | 画像・@-mention + 正式切替 UX | doc 32 §8 のまま |

PR1.5（doc 31 語彙）は独立。C2 の後が視覚的にまとまる（pane ラベルと ChatView が同時に新語彙になる）。

## 8. リスク

- **LanePool 改修の blast radius**: `subscribe_output` / `write_to_lane` / spawn 経路の callers — 実装前に gitnexus impact 必須
- **reconcile 変更**: #683（performer teardown）の隣接領域。LaneInfo 拡張は additive + serde default で後方互換に留める
- **World A/B の境界規律**: C1 以降、Console 関連の新 JS は必ず World B（bundle）に書く。World A への追記は禁止（境界が再び溶ける）

## 9. 方向性 — Act II を primary console へ（2026-07-09 dogfood 中の user 所感）

> 「多分 Act II できたら、そっちメインになりそう」— C2 の初回実機 dogfood で。

Act II（Console GUI）が成熟したら **Console の既定モードを Chat へ倒す**（現状 `ConsoleMode::default() = Tui` は後方互換のための保守的既定）。含意:

- **default 反転**: `ConsoleMode::default()` を Chat へ、または `config.kdl` で per-user 既定を選べるように（Tui 派も残す）。boot 時 `with_conductor` が既定 mode を honor する土台は C1 で入っている
- **成熟条件**（反転の前提）: C2 streaming / C3 diff / C4 入力が揃い、resume 継続・self-heal・reconnect が実機で安定してから。中途半端な Act II を既定にしない
- **cc_session 継承が体験の核**（user 明言「session id 引き継げたら最高」）: Act I ⇄ II は同一 cc_session の resume。transcript pre-flight（`cc_session::transcript_exists`）で stale id は fresh に倒し、live session は継続する（C2 実装）
- **示唆**: primary になるなら Act II の完成度優先度が上がる。TUI(Act I) は power user / 低レベル操作用に残るが、1st ビューは GUI へ

## 10. dogfood で潰した配線バグ（C2 初回、2026-07-09）— 同型の「新経路の登録漏れ」

Act II の新経路が既存インフラの登録リストから漏れる同型バグを 4 連続で発見・修正。今後 engine/pane 種別を足す時のチェックリスト:

1. **toggle no-op**: 宛先 lane を起動レースで未設定の変数から取っていた → `setActivePane` 追跡の `activeLaneAddress` を使う
2. **IPC 誤配送**: `is_main_ipc_tag`（app.rs）に新 tag 未登録 → `console:set_mode`/`echoes:submit` を追加。**新 IPC tag は必ずここに足す**
3. **SP→World 転送漏れ**: canvas-ingest driver（discovery.rs）が `process/echoes/data/#` 未購読 → 追加。**新 topic 系統は canvas driver の subscribe に足す**
4. **resume ハードエラー**: stale cc_session id で `--resume` 失敗 → `transcript_exists` pre-flight で fresh に fallback（TUI の `|| claude` 相当）

## 11. 未決事項

- `id` 移行の blast radius — 実装時に gitnexus impact で確定
- default mode 反転のタイミング（§9、成熟条件を満たしてから）
