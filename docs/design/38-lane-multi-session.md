# doc 38 — Lane 複数 Echoes session（1 Lane = N session）

> **status**: 設計確定（2026-07-16 hearing、mako + Opus 4.8）。実装は fresh session から（入口 todo = `mem_1Cd3bo6Y4YepXnRqQyeWf8`）
> **supersedes**: [doc 37](./37-echoes-two-axes.md) §5「engine は lane-pinned」→ 本 doc が N session へ拡張（§6 の再カット条項の発動）。[doc 36](./36-echoes-engine-axis.md) の主+副サブコンソール → N 本対称に一般化（実装知見は §6 で継承）
> **改定対象**: [doc 33](./33-console-unification.md) §0 の法（下記 §2）
> **発端**: 2026-07-16 dogfood。4 エンジン化（PR #778-780）を触った直後に mako「まずは Lane に対して複数 ECHOES 立ち上げしたい」— doc 37 §6 が「dogfood の実感が出たら」と予約した再カット信号が即日到来した。

## 0. TL;DR

> **1 Lane = N session。session は VP が採番する実体、engine は session 単位（同一 engine の並列も可）、
> 会話 id は session の後着属性（無いこともある）。Act I の床は lane に 1 枚の「設備」で、
> session はそこに注入されて化身する。**

- 方針: **動作確実 > UI。GUI の表示場所は仮置き（dogfood で変わる前提）。backend を唯一の真実源にし、UI は薄い view に徹する**（2026-07-16 の経路分裂バグ ×2 の教訓の直接適用）
- Phase 1 は既存 1-session 動作を「N=1 の特殊ケース」として温存したまま器だけ広げる（中間状態を作らない）

## 1. モデル — 3 層の分離

```
床（Act I の PTY）     = lane の設備（1 枚）。session の view ではなく、session が化身する「場所」
session               = 会話の実体。identity は VP 採番のローカル key（<lane>#1, #2 …）。N 本
会話 id（cc_session 等）= session の Option<String> 属性。エンジンから後着で届く。届かないエンジンもいる
```

### 1.1 なぜ会話 id を identity にしないか（hearing で確定したケース分析）

Act I には「session id が存在しない」状態が**一時的にも恒久的にも**ある:

| ケース | id |
|---|---|
| fresh spawn 直後（SessionStart hook 発火前の窓）| 一時的に無い |
| New Session 直後（clear 済・会話前）| 一時的に無い |
| `/exit` で agent を抜けた床（**正規操作**）| engine 不在。旧 id は残存し得る |
| codex の Act I-only lane（Act II 未経由 = record-from-init 不発）| 恒久的に無い |
| agy（id 取得手段そのものが無い）| 恒久的に無い |

会話 id を identity にするとこれら全部が表現不能。**id は属性**とすれば全ケースが状態機械に収まる:

```
Draft（id 無し。誕生直後 / 取得不能エンジン）
  → Bound（会話 id 観測 = hook / record-from-init が書く）
  → Detached（engine down・id 保持）→ resume で Bound へ復帰
agy は Draft のまま会話し続ける（resume 不可はエンジンの性質として正直に表現）
```

header の session chip（PR #778/#779）の presence-driven 表示（id が Some の時だけ点灯）はこのモデルと
そのまま整合する。

### 1.2 床は unbound が正常状態（focused 注入）

`/exit` は正規操作 = 「床に agent が居ない」は一級市民。床と session の関係は**注入のたびに結ばれ、
/exit で解ける動的な関係**:

- VP が session を Act I で開く = その session の resume command（`--resume '<id>' || <cli>`）を床に type-ahead 注入
- 注入先の選択 = **focused session**（respawn / Act 切替 / 明示操作時）
- /exit 後 = 床は素の shell。VP は何もしない（engine down は session 側に Detached として表示）
- 手打ち起動も観測できる: claude は SessionStart hook（VP_LANE 環境）が id を書く既存機構がそのまま効く

これにより「Act I は複数化しない（床は 1 枚）が、どの session も Act I に呼べる」— doc 37 §5 案 A の
弱点（2 本目以降を Act I で見られない）がモデルから消える。

## 2. doc 33 の法の改定

| | 旧（doc 33 §0） | 新（本 doc） |
|---|---|---|
| 法 | 1 lane = 1 Console = 高々 1 エンジン = 1 cc_session | **Act I: 床は lane に 1 枚（注入で任意 session が化身）/ Act II: 1 session = 1 engine = 1 会話 id、lane は N session** |
| 排他の単位 | lane | **session**（session 内は従来どおり engine 排他。lane 内の session 同士は独立） |
| engine-less | lane の正常形（chat lazy） | **session の Detached が正常形**。focused は eager resume（§4）|

守るもの: 「1 会話 1 エンジン」（1 session に 2 engine を同時に立てない）は不変。doc 33 が守った
二重駆動防止は session 粒度で継続する。

## 3. hearing 決定（2026-07-16、シンプル first 原則）

| 論点 | 決定 | 備考 |
|---|---|---|
| スコープ | **Act II のみ N 本**（床は 1 枚 + focused 注入） | モデル §1.2 の帰結 |
| 見せ方 | **タブ（仮置き）** — EchoesHeader 下に session tab 列、1 枚表示 | **表示場所は dogfood で変わる前提**。UI は state を持たない薄い view に徹する（§5 原則）。split は将来の上乗せ |
| 作成 UX | **chat header の「+」**（engine 選択 = stands_list 再利用、選ぶと Draft session が即 tab に出る） | 既存「✨ New」は「現 session を fresh」に意味を絞る |
| lifecycle | **focused のみ eager** — attach / tab 切替で自動 resume spawn。背景 session は Detached | 全 session eager は spawn CPU cap（#667）と干渉するため不採用。「背景も稼働維持」は将来オプション |

## 4. lifecycle 改修の合流（todo `mem_1Cd4Mse1dwKURWDavhMN4w`）

doc 33 C1 の lazy 決定（submit まで engine-less）+ #683 ガード（chat lane は auto-respawn 外）を、
本設計の focused-eager へ転換する:

1. **eager 自動起動 + 前回状態キープ**: SP 再起動 / lane attach / tab 切替で **focused session を自動
   resume spawn**（`ensure_chat_engine` は既に `--resume` + transcript pre-flight を持つ = 呼ぶ契機を
   足すのが本体）
2. **New Session**: fresh = 「focused session を clear して Draft に戻す」or「新 Draft session を作って
   focus」— タブモデルでは後者が自然（旧会話はタブに残る = 前回状態キープの延長）。実装時に UX 確認
3. **再接続アイコンの正確化**: resync-loader（`activeLaneReplaying`、replay_start→end window 駆動）は
   replay_end が来ない経路（Act I lane / error 中断）で出っぱなしになれる。**focused session の attach
   状態機械に表示を束縛**し、lane/Act/tab 切替で必ず解除 + timeout 安全網

## 5. 実装計画（バグの出にくい順、実装は fresh session）

**原則**: session 一覧・focused・会話 id は **SP が唯一の真実源**。UI はそれを描くだけ。
（2026-07-16 の教訓: 経路分裂バグは「状態の供給が複数経路で片方だけ直す」から起きる。供給を 1 系統に）

### Phase 1 — backend 核（GUI 無改修・既存動作不変・cargo test で完結）

1. `LanePool.chat_engines` の key を lane → **(lane, session_key)** に拡張。session_key = VP 採番
2. session registry の永続: lane ごとの session 一覧（key / engine 種別 / 会話 id Option / focused）を
   state file 化。`session_store` の key を `<lane>#<n>` に広げる（doc 36 実証: `#` は sanitize 安全）
3. RPC（echoes_submit / interrupt / demand_start / set_console_mode 系）に session key を **additive**
   に追加 — **省略時 = focused で完全後方互換**
4. topic: EchoesEvent に session field を additive 追加（pump は無改修流用）

⚠️ doc 36 §6 の落とし穴を厳守:
- **session key を wire の lane 名に埋めない**（`parse_address` が `foo#1` を実在不一致のまま通す
  → "webview では動いて SP だけ壊れる"）。session は**別 field / method 名**で運ぶ
- restart（New Session）の clear 対象に全 session の state を含める（「fresh が副を知らない」の再演防止）
- console_mode ガードを session 経路に流用しない（意図しない制約の混入）

### Phase 2 — 薄い UI（仮置きタブ）

- tab strip = session 一覧の描画 + focused 切替 RPC のみ（状態を持たない）
- 「+」→ engine 選択（stands_list）→ Draft session が即 tab に出る（§1.1 モデルの実地確認）
- 表示場所の引っ越し（sidebar 等）が backend 無傷でできることを設計検収条件にする

### Phase 3 — lifecycle 磨き（§4 の 3 点、同じ attach 状態機械に載せて一体実装）

## 6. doc 36 からの資産継承

- `sub_chat_engines` 並列 map の発想 → (lane, session_key) key に一般化（「主/副」の非対称は捨てる）
- 副 topic 分離 / ChatView の lane 引数化 / resync-loader 等の描画資産再利用 → そのまま有効
- `cursor_sessions/<project>__<lane>#sub` の state file 命名 → `#<n>` に一般化
- PR #778 の基盤がそのまま土台: **TurnHost は既に「1 会話 = 1 host」で lane 非結合 / EngineKind /
  session_store**

## 7. 非目標（over-scope 防止）

- Act I（PTY）の session ごと複数化 — 床は 1 枚 + 注入（§1.2）。フル対称化は将来検討
- split / pin 表示 — タブで dogfood してから
- 背景 session の稼働維持（裏で turn 継続）— focused eager で dogfood してから
- performer lane 間の session 移動 / session の wire address 化 — 別 doc
- GUI add_performer の stand 落ち bug（`mem_1Cd4M7i5Enp3HHMLVYayRe`）— 独立に修正可能（watcher spawn が
  descriptor の stand を読む fix。per-lane stand 永続の解消を兼ねる）
